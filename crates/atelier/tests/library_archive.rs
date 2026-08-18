//! End-to-end contract for portable document archives in the native CLI.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use atelier_studio::Studio;
use serde_json::Value;

struct TestArea {
    root: PathBuf,
}

impl TestArea {
    fn new(label: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "atelier-library-archive-{label}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&root).expect("create test area");
        Self { root }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
}

impl Drop for TestArea {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn path_arg(path: &Path) -> String {
    path.to_str().expect("test path is UTF-8").to_string()
}

fn atelier(args: &[String], ambient_home: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_atelier"))
        .args(args)
        .env("ATELIER_HOME", ambient_home)
        .env("ATELIER_LOG", "off")
        .output()
        .expect("run atelier")
}

fn json_report(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "expected JSON report ({error}); stdout: {}; stderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[test]
fn library_archive_round_trips_and_requires_confirmed_replacement() {
    let area = TestArea::new("roundtrip");
    let source_home = area.path("source-home");
    let restored_home = area.path("restored-home");
    let ambient_home = area.path("ambient-home");
    let archive = area.path("hero.atelierpack");

    let source = Studio::with_home(source_home.clone());
    let created = source.doc_new("hero", 11, 7).expect("create source");
    let id = created["doc_id"].as_str().expect("document id").to_string();
    source
        .set_document_revision(&id, 7)
        .expect("mark source revision");

    let pack_args = vec![
        "library".into(),
        "pack".into(),
        id.clone(),
        "--out".into(),
        path_arg(&archive),
        "--home".into(),
        path_arg(&source_home),
    ];
    let packed = atelier(&pack_args, &ambient_home);
    assert!(
        packed.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&packed.stderr)
    );
    let report = json_report(&packed);
    assert_eq!(report["ok"], true);
    assert_eq!(report["doc_id"], id);
    assert_eq!(report["path"], path_arg(&archive));
    assert!(archive.is_file());
    assert!(!ambient_home.join("documents").join(&id).exists());

    let overwrite = atelier(&pack_args, &ambient_home);
    assert_eq!(overwrite.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&overwrite.stderr).contains("refusing to overwrite"),
        "stderr: {}",
        String::from_utf8_lossy(&overwrite.stderr)
    );

    let unpack_args = vec![
        "library".into(),
        "unpack".into(),
        path_arg(&archive),
        "--home".into(),
        path_arg(&restored_home),
    ];
    let unpacked = atelier(&unpack_args, &ambient_home);
    assert!(
        unpacked.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&unpacked.stderr)
    );
    let report = json_report(&unpacked);
    assert_eq!(report["doc_id"], id);
    assert_eq!(report["replaced"], false);

    let restored = Studio::with_home(restored_home.clone());
    assert_eq!(
        restored.doc_info(&id).expect("restored document"),
        source.doc_info(&id).expect("source document")
    );
    assert_eq!(restored.document_revision(&id).unwrap(), 7);

    restored
        .set_document_revision(&id, 99)
        .expect("change destination revision");
    let collision = atelier(&unpack_args, &ambient_home);
    assert_eq!(collision.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&collision.stderr).contains("already exists"),
        "stderr: {}",
        String::from_utf8_lossy(&collision.stderr)
    );
    assert_eq!(restored.document_revision(&id).unwrap(), 99);

    for lone_confirmation in ["--replace", "--yes"] {
        let mut args = unpack_args.clone();
        args.push(lone_confirmation.into());
        let refused = atelier(&args, &ambient_home);
        assert_eq!(refused.status.code(), Some(2));
        assert!(
            String::from_utf8_lossy(&refused.stderr)
                .contains("--replace and --yes must be passed together")
        );
        assert_eq!(restored.document_revision(&id).unwrap(), 99);
    }

    let mut replace_args = unpack_args;
    replace_args.extend(["--yes".into(), "--replace".into()]);
    let replaced = atelier(&replace_args, &ambient_home);
    assert!(
        replaced.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&replaced.stderr)
    );
    assert_eq!(json_report(&replaced)["replaced"], true);
    assert_eq!(restored.document_revision(&id).unwrap(), 7);
    assert!(!ambient_home.join("documents").join(&id).exists());
}

#[test]
fn library_archive_parser_errors_exit_with_usage_status() {
    let area = TestArea::new("usage");
    let ambient_home = area.path("ambient-home");
    let archive = path_arg(&area.path("unused.atelierpack"));
    let cases = [
        vec!["library", "pack", "id", "--out"],
        vec!["library", "pack", "id", "--out", "one", "--out", "two"],
        vec!["library", "pack", "id", "--out", "one", "--unknown"],
        vec!["library", "unpack", &archive, "extra"],
        vec![
            "library", "unpack", &archive, "--home", "one", "--home", "two",
        ],
        vec!["library", "unpack", &archive, "--force"],
    ];
    for args in cases {
        let args: Vec<String> = args.into_iter().map(str::to_string).collect();
        let output = atelier(&args, &ambient_home);
        assert_eq!(
            output.status.code(),
            Some(2),
            "args {args:?}; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("usage:"),
            "args {args:?}; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
