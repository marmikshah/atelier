use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use atelier_mcp::recipe::Recipe;
use serde_json::Value;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn strings<'a>(manifest: &'a Value, key: &str) -> Vec<&'a str> {
    manifest[key]
        .as_array()
        .unwrap_or_else(|| panic!("benchmarks/runs.json: '{key}' must be an array"))
        .iter()
        .map(|value| {
            value
                .as_str()
                .unwrap_or_else(|| panic!("benchmarks/runs.json: '{key}' entries must be strings"))
        })
        .collect()
}

#[test]
fn every_showcase_run_has_a_current_replay_and_gif() {
    let root = repo_root();
    let manifest: Value = serde_json::from_str(
        &fs::read_to_string(root.join("benchmarks/runs.json")).expect("read benchmarks/runs.json"),
    )
    .expect("parse benchmarks/runs.json");
    let tasks = strings(&manifest, "tasks");
    let models = strings(&manifest, "models");

    let expected: BTreeSet<String> = models
        .iter()
        .flat_map(|model| tasks.iter().map(move |task| format!("{model}/{task}")))
        .collect();
    let runs = manifest["runs"]
        .as_array()
        .expect("benchmarks/runs.json: 'runs' must be an array");
    let declared: BTreeSet<String> = runs
        .iter()
        .map(|run| {
            let model = run["model"].as_str().expect("run.model must be a string");
            let task = run["task"].as_str().expect("run.task must be a string");
            format!("{model}/{task}")
        })
        .collect();

    assert_eq!(
        runs.len(),
        declared.len(),
        "benchmarks/runs.json contains a duplicate model/task run"
    );
    assert_eq!(
        declared, expected,
        "benchmark runs must cover every declared model/task pair exactly once"
    );

    let replay_root = root.join("benchmarks/replays");
    let mut committed = BTreeSet::new();
    for model in fs::read_dir(&replay_root).expect("read benchmarks/replays") {
        let model = model.expect("read replay model entry");
        assert!(
            model.file_type().expect("read replay model type").is_dir(),
            "unexpected file in benchmarks/replays: {}",
            model.path().display()
        );
        let model_name = model.file_name().to_string_lossy().into_owned();
        for replay in fs::read_dir(model.path()).expect("read model replay directory") {
            let replay = replay.expect("read replay entry");
            let path = replay.path();
            assert_eq!(
                path.extension().and_then(|extension| extension.to_str()),
                Some("jsonl"),
                "unexpected replay artifact: {}",
                path.display()
            );
            let task = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .expect("replay filename must be UTF-8");
            committed.insert(format!("{model_name}/{task}"));
        }
    }
    assert_eq!(
        committed, expected,
        "committed replay files must exactly match the benchmark matrix"
    );

    for pair in expected {
        let (model, task) = pair.split_once('/').expect("model/task pair");
        let replay_path = replay_root.join(model).join(format!("{task}.jsonl"));
        let source = fs::read_to_string(&replay_path)
            .unwrap_or_else(|error| panic!("read {}: {error}", replay_path.display()));
        Recipe::parse(&source)
            .unwrap_or_else(|error| panic!("parse {}: {error}", replay_path.display()));

        let gif = root
            .join("site/showcase")
            .join(model)
            .join(format!("{task}.gif"));
        assert!(gif.is_file(), "missing showcase GIF: {}", gif.display());
    }
}
