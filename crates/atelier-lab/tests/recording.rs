//! The episode record is the training data: assert what an episode actually
//! writes — event ordering and envelope, rejected actions captured, renders
//! referenced by hash rather than embedded.

use std::path::{Path, PathBuf};

use atelier_lab::*;

fn sample_task() -> Task {
    serde_json::from_str(
        r#"{
      "id": "character-001",
      "prompt": "A tired knight carrying a chipped red shield",
      "category": "character",
      "width": 32,
      "height": 32,
      "max_colors": 16,
      "must_include": ["knight", "red shield", "visible damage"],
      "must_avoid": [],
      "style": {"outline": "selective", "lighting": "upper-left", "detail": "medium"},
      "split": "development"
    }"#,
    )
    .unwrap()
}

fn test_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("atelier-lab-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn paint(x: i32, y: i32, size: u32, color: u32) -> Action {
    Action::new(ActionKind::PaintPatch {
        layer: 0,
        x,
        y,
        width: size,
        height: size,
        grid: vec![color; (size * size) as usize],
    })
}

/// A short but complete episode: one rejected action, palette, a paint, a
/// full observation, a checkpoint/restore round-trip, and a finish.
fn run_episode(root: &Path, seed: u64) -> PathBuf {
    let mut env = AtelierEnv::new(root, seed).unwrap();
    env.reset(&sample_task()).unwrap();
    // Stage violation — must be recorded, not dropped.
    let t = env.step(&paint(0, 0, 2, 0)).unwrap();
    assert!(!t.accepted);
    let mut palette = Action::new(ActionKind::SetPalette {
        colors: vec![[0, 0, 0, 255], [200, 30, 30, 255], [30, 30, 200, 255]],
    });
    palette.intent = Some("establish the shield's red focal ramp".into());
    env.step(&palette).unwrap();
    env.step(&Action::new(ActionKind::AdvanceStage)).unwrap();
    env.step(&paint(2, 3, 4, 1)).unwrap();
    env.observe(ObservationLevel::Full).unwrap();
    let cp = env.checkpoint().unwrap();
    env.step(&paint(10, 10, 2, 2)).unwrap();
    env.restore(&cp).unwrap();
    env.step(&paint(20, 20, 3, 2)).unwrap();
    env.step(&Action::new(ActionKind::Finish)).unwrap();
    env.finish().unwrap();
    env.episode_dir().to_path_buf()
}

fn read_events(episode_dir: &Path) -> Vec<Event> {
    Recorder::read(&episode_dir.join(EPISODE_LOG_FILE)).unwrap()
}

#[test]
fn episode_log_captures_the_full_flow() {
    let root = test_root("recording");
    let episode = run_episode(&root, 7);
    let events = read_events(&episode);
    let kinds: Vec<&str> = events
        .iter()
        .map(|e| match &e.event {
            EventKind::Reset { .. } => "reset",
            EventKind::Step { .. } => "step",
            EventKind::Observation { .. } => "observation",
            EventKind::Checkpoint { .. } => "checkpoint",
            EventKind::Restore { .. } => "restore",
            EventKind::Feedback { .. } => "feedback",
            EventKind::Finish { .. } => "finish",
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            "reset",
            "step", // rejected paint
            "step", // set palette
            "step", // advance stage
            "step", // paint
            "observation",
            "checkpoint",
            "step", // second paint
            "restore",
            "step", // third paint
            "step", // finish action
            "finish",
        ]
    );
    // Envelope: version, session, monotonic seq from 0.
    let session = episode.file_name().unwrap().to_string_lossy().into_owned();
    for (i, e) in events.iter().enumerate() {
        assert_eq!(e.format_version, FORMAT_VERSION);
        assert_eq!(e.session_id, session);
        assert_eq!(e.seq, i as u64);
    }
    // The rejected action is a first-class record: accepted=false, the
    // compile error attached, no observation_after.
    let EventKind::Step {
        accepted,
        error,
        observation_after,
        ..
    } = &events[1].event
    else {
        panic!("event 1 is the rejected step")
    };
    assert!(!accepted);
    assert!(matches!(error, Some(CompileError::StageViolation { .. })));
    assert!(observation_after.is_none());
    let EventKind::Step { intent, .. } = &events[2].event else {
        panic!("event 2 is the palette step")
    };
    assert_eq!(
        intent.as_deref(),
        Some("establish the shield's red focal ramp")
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn renders_are_referenced_by_hash_never_embedded() {
    let root = test_root("refs");
    let episode = run_episode(&root, 7);
    let events = read_events(&episode);
    let artifacts = ArtifactStore::new(episode.join(ARTIFACTS_DIR)).unwrap();

    // The full observation carries four artifact references whose bytes are
    // actually in the store.
    let full = events.iter().find_map(|e| match &e.event {
        EventKind::Observation {
            observation: RecordedObservation::Full(f),
        } => Some(f.renders.clone()),
        _ => None,
    });
    let renders = full.expect("a full observation was recorded");
    for r in [
        renders.native,
        renders.enlarged,
        renders.grayscale,
        renders.notan,
    ] {
        assert_eq!(r.sha256.len(), 64);
        assert!(artifacts.exists(&r.sha256), "{} stored", r.sha256);
    }

    // The finish event's final render is stored, and it is the 32×32 PNG.
    let reference = events.iter().find_map(|e| match &e.event {
        EventKind::Finish { final_render, .. } => Some(final_render.clone()),
        _ => None,
    });
    let reference = reference.expect("a finish event was recorded");
    assert_eq!(reference.kind, ArtifactKind::FinalImage);
    let png = artifacts.get(&reference.sha256).unwrap();
    let img = image::load_from_memory(&png).unwrap();
    assert_eq!((img.width(), img.height()), (32, 32));

    // No log line embeds image bytes: PNG bytes serialized as JSON would be
    // a five-figure-number array, so any line that long means embedding.
    let body = std::fs::read_to_string(episode.join(EPISODE_LOG_FILE)).unwrap();
    let longest = body.lines().map(str::len).max().unwrap_or(0);
    assert!(
        longest < 64_000,
        "a {longest}-char line smells like embedded bytes"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn replay_rebuilds_the_episode_pixel_exact() {
    let root = test_root("replay");
    let episode = run_episode(&root, 7);
    let replay_root = test_root("replay-fresh");
    let report = replay(&episode, &replay_root).unwrap();
    assert!(report.matched, "divergence: {:?}", report.divergence);
    assert_eq!(report.steps_replayed, 6, "accepted steps, finish included");
    assert_eq!(report.steps_rejected_skipped, 1);
    assert!(report.events > 0);
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&replay_root);
}

#[test]
fn replay_names_the_first_tampered_step() {
    let root = test_root("tamper");
    let episode = run_episode(&root, 7);
    let mut events = read_events(&episode);

    // Tamper: flip one recorded cell of the FIRST accepted paint's
    // observation_after from transparent to swatch 1. The rebuilt raster
    // will disagree at exactly that step.
    let mut tampered_seq = None;
    for e in events.iter_mut() {
        if let EventKind::Step {
            accepted: true,
            action,
            observation_after: Some(after),
            ..
        } = &mut e.event
        {
            if matches!(action.action, ActionKind::PaintPatch { .. }) {
                let cell = &mut after.layers[0].indices[0];
                assert_eq!(*cell, None, "the tamper target starts transparent");
                *cell = Some(1);
                tampered_seq = Some(e.seq);
                break;
            }
        }
    }
    let tampered_seq = tampered_seq.expect("an accepted paint step exists");
    // Rewrite the log with the tampered events.
    let mut body = String::new();
    for e in &events {
        body.push_str(&serde_json::to_string(e).unwrap());
        body.push('\n');
    }
    std::fs::write(episode.join(EPISODE_LOG_FILE), body).unwrap();

    let replay_root = test_root("tamper-fresh");
    let report = replay(&episode, &replay_root).unwrap();
    assert!(!report.matched, "a tampered record must not replay clean");
    let d = report.divergence.expect("divergence reported");
    assert_eq!(d.seq, tampered_seq);
    assert_eq!(d.kind, DivergenceKind::RasterMismatch);
    assert_eq!(d.differing_pixels, Some(1));
    assert_eq!(d.diff_bbox, Some([0, 0, 0, 0]));
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&replay_root);
}

#[test]
fn episodes_bundle_and_annotations_export_in_canonical_order() {
    let root = test_root("dataset-loop");
    let episode_a = run_episode(&root, 11);
    let episode_b = run_episode(&root, 12);
    let manifest = root.join("pairs.jsonl");
    let input = ComparisonBundleInput {
        format_version: EVALUATION_FORMAT_VERSION,
        id: "character-001-baseline-v-model".into(),
        candidate_a: EpisodeCandidateInput {
            id: "baseline".into(),
            episode_dir: episode_a,
            source: SampleSource::Model,
            generator: Some("baseline-model".into()),
        },
        candidate_b: EpisodeCandidateInput {
            id: "challenger".into(),
            episode_dir: episode_b,
            source: SampleSource::Search,
            generator: Some("search-policy".into()),
        },
        consistency_group: None,
    };
    std::fs::write(
        &manifest,
        format!("{}\n", serde_json::to_string(&input).unwrap()),
    )
    .unwrap();

    let bundle = root.join("bundle");
    let comparisons = bundle_episode_comparisons(&manifest, &bundle).unwrap();
    assert_eq!(comparisons.len(), 1);
    let comparison = &comparisons[0];
    let store = ArtifactStore::new(bundle.join(ARTIFACTS_DIR)).unwrap();
    for artifact in [
        &comparison.candidate_a.native,
        &comparison.candidate_a.enlarged,
        comparison.candidate_a.grayscale.as_ref().unwrap(),
        comparison.candidate_a.notan.as_ref().unwrap(),
    ] {
        assert!(store.exists(&artifact.sha256));
    }
    let enlarged =
        image::load_from_memory(&store.get(&comparison.candidate_a.enlarged.sha256).unwrap())
            .unwrap();
    assert_eq!((enlarged.width(), enlarged.height()), (256, 256));

    let annotations_path = root.join("annotations.jsonl");
    write_annotations_jsonl(
        &annotations_path,
        &[PairwiseAnnotation {
            format_version: EVALUATION_FORMAT_VERSION,
            comparison_id: comparison.id.clone(),
            annotator_id: "reviewer-opaque".into(),
            // Browser showed canonical B on the left.
            presented: ["challenger".into(), "baseline".into()],
            overall: Preference::Left,
            requirement_adherence: Preference::Right,
            native_readability: Preference::Tie,
            reasons: vec![PreferenceReason::Silhouette],
            explanation: Some("clearer shape".into()),
        }],
    )
    .unwrap();
    let critic_path = root.join("critic.jsonl");
    let count = export_annotated_critic_jsonl(
        &bundle.join(COMPARISONS_FILE),
        &annotations_path,
        &critic_path,
    )
    .unwrap();
    assert_eq!(count, 1);
    let row: serde_json::Value =
        serde_json::from_str(std::fs::read_to_string(&critic_path).unwrap().trim()).unwrap();
    assert_eq!(row["overall"], "candidate_b");
    assert_eq!(row["requirement_adherence"], "candidate_a");
    assert_eq!(row["native_readability"], "tie");
    assert!(row.get("annotator_id").is_none());
    let _ = std::fs::remove_dir_all(&root);
}
