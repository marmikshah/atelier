//! End-to-end episode: reset → paint → checkpoint → paint → restore, against
//! a real embedded atelier in a temp episode root. Kept small (32×32, one
//! layer, one frame) so the suite stays fast.

use std::path::PathBuf;

use atelier_lab::*;

/// The lab.md item-8 example record, verbatim.
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

fn light(obs: &Observation) -> &LightObservation {
    obs.light()
}

/// Palette index at (x, y) of layer 0 in a light observation.
fn idx(obs: &LightObservation, x: u32, y: u32) -> Option<u32> {
    obs.layers[0].indices[(y * obs.width + x) as usize]
}

#[test]
fn episode_paint_checkpoint_restore() {
    let root = test_root("episode");
    let mut env = AtelierEnv::new(&root, 42).unwrap();
    assert!(
        env.episode_dir().starts_with(&root),
        "episode dir lives under the configured root"
    );

    // Reset: transparent 32×32, stage Specification, palette empty.
    let obs = env.reset(&sample_task()).unwrap();
    let obs = light(&obs);
    assert_eq!((obs.width, obs.height), (32, 32));
    assert_eq!(obs.stage, Stage::Specification);
    assert!(obs.palette.is_empty());
    assert!(obs.layers[0].indices.iter().all(|i| i.is_none()));
    assert!(obs.integrity.on_palette);
    assert_eq!(obs.integrity.opaque_pixels, 0);

    // Painting in Specification is a stage violation — rejected without any
    // tool call, and the document is untouched.
    let paint = Action::new(ActionKind::PaintPatch {
        layer: 0,
        x: 2,
        y: 3,
        width: 4,
        height: 4,
        grid: vec![1; 16],
    });
    let t = env.step(&paint).unwrap();
    assert!(!t.accepted);
    assert!(t.compiled.is_empty() && t.tool_results.is_empty());
    assert!(t.observation_after.is_none());
    assert!(
        matches!(
            t.error,
            Some(CompileError::StageViolation {
                stage: Stage::Specification,
                ..
            })
        ),
        "{:?}",
        t.error
    );

    // Set the palette, advance to Silhouette, then the same patch is legal.
    let t = env
        .step(&Action::new(ActionKind::SetPalette {
            colors: vec![[0, 0, 0, 255], [200, 30, 30, 255], [30, 30, 200, 255]],
        }))
        .unwrap();
    assert!(t.accepted, "{:?}", t.tool_results);
    assert_eq!(t.compiled[0].tool, "doc_palette");
    let t = env.step(&Action::new(ActionKind::AdvanceStage)).unwrap();
    assert!(t.accepted);
    let t = env.step(&paint).unwrap();
    assert!(t.accepted, "{:?}", t.tool_results);
    let after = light(t.observation_after.as_ref().unwrap());
    assert_eq!(after.stage, Stage::Silhouette);
    assert_eq!(idx(after, 2, 3), Some(1));
    assert_eq!(idx(after, 5, 6), Some(1), "patch covers (2..6, 3..7)");
    assert_eq!(idx(after, 6, 3), None, "outside the patch");
    assert_eq!(after.integrity.opaque_pixels, 16);
    assert_eq!(after.recent_actions.len(), 3);

    // Full observation: renders and audits on top of the light state.
    let full = env.observe(ObservationLevel::Full).unwrap();
    let Observation::Full(full) = full else {
        panic!("expected a full observation")
    };
    assert!(!full.renders.native.is_empty());
    assert!(!full.renders.enlarged.is_empty());
    assert!(!full.renders.grayscale.is_empty());
    assert!(!full.renders.notan.is_empty());
    assert_eq!(full.doc.layer_count, 1);
    assert_eq!(full.doc.palette_len, 3);

    // Checkpoint, paint elsewhere, restore: the second patch is gone, the
    // first is back exactly.
    let cp = env.checkpoint().unwrap();
    let t = env
        .step(&Action::new(ActionKind::PaintPatch {
            layer: 0,
            x: 10,
            y: 10,
            width: 2,
            height: 2,
            grid: vec![2; 4],
        }))
        .unwrap();
    assert!(t.accepted, "{:?}", t.tool_results);
    let obs = env.restore(&cp).unwrap();
    let obs = light(&obs);
    assert_eq!(idx(obs, 2, 3), Some(1), "first patch survives the restore");
    assert_eq!(idx(obs, 10, 10), None, "second patch rolled back");
    assert_eq!(obs.integrity.opaque_pixels, 16);

    // Finish via the action, then finish() reports a completed episode.
    let t = env.step(&Action::new(ActionKind::Finish)).unwrap();
    assert!(t.accepted);
    assert_eq!(env.stage(), Stage::Finished);
    let result = env.finish().unwrap();
    assert!(result.completed);
    assert_eq!(result.task_id, "character-001");
    assert_eq!(result.seed, 42);
    assert_eq!(result.steps, 5);
    assert!(
        env.step(&paint).is_err(),
        "stepping a finished episode errors"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn isolation_between_envs() {
    let root = test_root("isolation");
    let mut a = AtelierEnv::new(&root, 1).unwrap();
    let mut b = AtelierEnv::new(&root, 1).unwrap();
    assert_ne!(a.episode_dir(), b.episode_dir());
    a.reset(&sample_task()).unwrap();
    b.reset(&sample_task()).unwrap();
    a.step(&Action::new(ActionKind::SetPalette {
        colors: vec![[9, 9, 9, 255]],
    }))
    .unwrap();
    // B never saw A's palette: the per-episode Studio is the isolation.
    let obs_b = b.observe(ObservationLevel::Light).unwrap();
    assert!(light(&obs_b).palette.is_empty());
    let _ = std::fs::remove_dir_all(&root);
}
