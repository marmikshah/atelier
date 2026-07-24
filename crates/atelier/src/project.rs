//! Reproducible project exports.
//!
//! `atelier init` writes `.atelier/project.toml`; `atelier build` reads its
//! named export entries and sends each one through the same `doc_export`
//! dispatch path used by CLI calls, replay, and MCP. The manifest is deliberately
//! narrower than the tool schema: portable paths only, strict keys, and an
//! explicit version so typos and future format changes fail loudly.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use atelier_mcp::server::{self, Atelier};
use atelier_studio::{MAX_EXPORT_SCALE, Studio};
use serde_json::{Map, Value, json};
use toml_edit::{DocumentMut, Table};

const MANIFEST_PATH: &str = ".atelier/project.toml";
const MANIFEST_VERSION: i64 = 1;
const USAGE: &str = "usage: atelier build [--only NAME] [--dry-run]";

/// The starter manifest is useful without inventing a sample document that
/// does not exist. Uncommenting the example is enough to opt into a build.
pub(crate) const MANIFEST_TEMPLATE: &str = r#"# Atelier project manifest.
# Output paths are relative to the project root and cannot leave it.
version = 1

# [[exports]]
# name = "hero-sheet"
# doc = "hero"
# op = "sheet" # sheet | anim | tileset | all | atlas
# out = "assets/hero.png"
# scale = 4
"#;

/// Create the starter manifest without ever replacing an existing one.
pub(crate) fn ensure_manifest(store_root: &Path) -> std::io::Result<bool> {
    let path = store_root.join("project.toml");
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut file) => {
            file.write_all(MANIFEST_TEMPLATE.as_bytes())?;
            file.sync_all()?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && path.is_file() => {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Options {
    only: Option<String>,
    dry_run: bool,
}

enum Command {
    Build(Options),
    Help,
}

fn parse_args(args: &[String]) -> Result<Command, String> {
    let mut only = None;
    let mut dry_run = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--only" => {
                if only.is_some() {
                    return Err("--only may be passed only once".into());
                }
                let Some(name) = args.get(i + 1).filter(|value| !value.starts_with("--")) else {
                    return Err("--only needs an export name".into());
                };
                only = Some(name.clone());
                i += 2;
            }
            "--dry-run" => {
                dry_run = true;
                i += 1;
            }
            "--help" | "-h" => return Ok(Command::Help),
            other => return Err(format!("unknown argument '{other}'")),
        }
    }
    Ok(Command::Build(Options { only, dry_run }))
}

#[derive(Debug)]
struct Project {
    root: PathBuf,
    manifest: PathBuf,
    exports: Vec<Export>,
}

#[derive(Debug)]
struct Export {
    name: String,
    op: String,
    doc: Option<String>,
    out: String,
    out_path: PathBuf,
    scale: Option<u32>,
    params: Map<String, Value>,
}

impl Export {
    fn dispatch_args(&self) -> Value {
        let mut args = self.params.clone();
        args.insert("op".into(), json!(self.op));
        args.insert(
            "out_path".into(),
            json!(self.out_path.to_string_lossy().as_ref()),
        );
        if let Some(doc) = &self.doc {
            args.insert("doc_id".into(), json!(doc));
        }
        if let Some(scale) = self.scale {
            args.insert("scale".into(), json!(scale));
        }
        Value::Object(args)
    }
}

impl Project {
    fn load(root: &Path) -> Result<Self, String> {
        let manifest = root.join(MANIFEST_PATH);
        let source = std::fs::read_to_string(&manifest).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                format!(
                    "{} not found — run `atelier init` in the project root",
                    manifest.display()
                )
            } else {
                format!("cannot read {}: {error}", manifest.display())
            }
        })?;
        let document = source
            .parse::<DocumentMut>()
            .map_err(|error| format!("cannot parse {} as TOML: {error}", manifest.display()))?;

        for (key, _) in document.iter() {
            if !matches!(key, "version" | "exports") {
                return Err(format!(
                    "{}: unknown top-level key '{key}' (expected version or exports)",
                    manifest.display()
                ));
            }
        }
        let version = document
            .get("version")
            .and_then(toml_edit::Item::as_integer)
            .ok_or_else(|| {
                format!(
                    "{}: `version` must be the integer {MANIFEST_VERSION}",
                    manifest.display()
                )
            })?;
        if version != MANIFEST_VERSION {
            return Err(format!(
                "{}: unsupported version {version}; this Atelier supports version {MANIFEST_VERSION}",
                manifest.display()
            ));
        }

        let tables = match document.get("exports") {
            Some(item) => item.as_array_of_tables().ok_or_else(|| {
                format!(
                    "{}: `exports` must use [[exports]] tables",
                    manifest.display()
                )
            })?,
            None => {
                return Ok(Self {
                    root: root.to_path_buf(),
                    manifest,
                    exports: Vec::new(),
                });
            }
        };

        let mut exports = Vec::with_capacity(tables.len());
        let mut names = HashSet::new();
        let mut outputs = HashSet::new();
        for (index, table) in tables.iter().enumerate() {
            let mut export = parse_export(table, index, &manifest)?;
            if !names.insert(export.name.clone()) {
                return Err(format!(
                    "{}: duplicate export name '{}'",
                    manifest.display(),
                    export.name
                ));
            }
            export.out_path = resolve_output(root, &export.out)?;
            if !outputs.insert(export.out_path.clone()) {
                return Err(format!(
                    "{}: more than one export writes '{}'",
                    manifest.display(),
                    export.out
                ));
            }
            exports.push(export);
        }

        Ok(Self {
            root: root.to_path_buf(),
            manifest,
            exports,
        })
    }
}

const BASE_KEYS: &[&str] = &["name", "doc", "op", "out", "scale"];
const ALL_KEYS: &[&str] = &[
    "name",
    "doc",
    "op",
    "out",
    "scale",
    "meta",
    "format",
    "tag",
    "tile_w",
    "tile_h",
    "max_width",
];

fn parse_export(table: &Table, index: usize, manifest: &Path) -> Result<Export, String> {
    let label = format!("{}: exports[{}]", manifest.display(), index + 1);
    for (key, _) in table.iter() {
        if !ALL_KEYS.contains(&key) {
            return Err(format!("{label}: unknown key '{key}'"));
        }
    }

    let name = required_string(table, "name", &label)?;
    let op = required_string(table, "op", &label)?;
    let out = required_string(table, "out", &label)?;
    let doc = optional_string(table, "doc", &label)?;
    if doc.as_deref().is_some_and(|doc| doc.trim().is_empty()) {
        return Err(format!("{label}: `doc` cannot be empty"));
    }
    let scale = optional_u32(table, "scale", &label)?;
    if let Some(scale) = scale
        && !(1..=MAX_EXPORT_SCALE).contains(&scale)
    {
        return Err(format!(
            "{label}: `scale` must be between 1 and {MAX_EXPORT_SCALE}"
        ));
    }

    let op_keys: &[&str] = match op.as_str() {
        "sheet" => &["meta"],
        "anim" => &["format", "tag"],
        "tileset" => &["tile_w", "tile_h"],
        "all" => &[],
        "atlas" => &["max_width"],
        _ => {
            return Err(format!(
                "{label}: unknown op '{op}' (use sheet, anim, tileset, all, or atlas)"
            ));
        }
    };
    for (key, _) in table.iter() {
        if !BASE_KEYS.contains(&key) && !op_keys.contains(&key) {
            return Err(format!("{label}: `{key}` is not valid for op={op}"));
        }
    }

    match op.as_str() {
        "all" | "atlas" if doc.is_some() => {
            return Err(format!("{label}: op={op} spans the library; omit `doc`"));
        }
        "sheet" | "anim" | "tileset" if doc.is_none() => {
            return Err(format!("{label}: op={op} requires `doc`"));
        }
        _ => {}
    }

    let mut params = Map::new();
    match op.as_str() {
        "sheet" => {
            if let Some(meta) = optional_string(table, "meta", &label)? {
                if !matches!(meta.as_str(), "atelier" | "standard") {
                    return Err(format!(
                        "{label}: `meta` must be \"atelier\" or \"standard\""
                    ));
                }
                params.insert("meta".into(), json!(meta));
            }
        }
        "anim" => {
            if let Some(format) = optional_string(table, "format", &label)? {
                if !matches!(format.as_str(), "gif" | "apng") {
                    return Err(format!("{label}: `format` must be \"gif\" or \"apng\""));
                }
                params.insert("format".into(), json!(format));
            }
            if let Some(tag) = optional_string(table, "tag", &label)? {
                params.insert("tag".into(), json!(tag));
            }
        }
        "tileset" => {
            for key in ["tile_w", "tile_h"] {
                let value = required_u32(table, key, &label)?;
                if value == 0 {
                    return Err(format!("{label}: `{key}` must be greater than zero"));
                }
                params.insert(key.into(), json!(value));
            }
        }
        "atlas" => {
            if let Some(max_width) = optional_u32(table, "max_width", &label)? {
                if max_width == 0 {
                    return Err(format!("{label}: `max_width` must be greater than zero"));
                }
                params.insert("max_width".into(), json!(max_width));
            }
        }
        "all" => {}
        _ => unreachable!("op was validated above"),
    }

    Ok(Export {
        name,
        op,
        doc,
        out,
        out_path: PathBuf::new(),
        scale,
        params,
    })
}

fn required_string(table: &Table, key: &str, label: &str) -> Result<String, String> {
    let value = optional_string(table, key, label)?
        .ok_or_else(|| format!("{label}: missing required string `{key}`"))?;
    if value.trim().is_empty() {
        return Err(format!("{label}: `{key}` cannot be empty"));
    }
    Ok(value)
}

fn optional_string(table: &Table, key: &str, label: &str) -> Result<Option<String>, String> {
    table
        .get(key)
        .map(|item| {
            item.as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("{label}: `{key}` must be a string"))
        })
        .transpose()
}

fn required_u32(table: &Table, key: &str, label: &str) -> Result<u32, String> {
    optional_u32(table, key, label)?
        .ok_or_else(|| format!("{label}: missing required integer `{key}`"))
}

fn optional_u32(table: &Table, key: &str, label: &str) -> Result<Option<u32>, String> {
    table
        .get(key)
        .map(|item| {
            let value = item
                .as_integer()
                .ok_or_else(|| format!("{label}: `{key}` must be an integer"))?;
            u32::try_from(value)
                .map_err(|_| format!("{label}: `{key}` must fit an unsigned 32-bit integer"))
        })
        .transpose()
}

/// Resolve a portable manifest path and prove its closest existing ancestor
/// remains inside the project after symlinks are resolved.
fn resolve_output(root: &Path, raw: &str) -> Result<PathBuf, String> {
    if raw.trim().is_empty() {
        return Err("project export path cannot be empty".into());
    }
    let path = Path::new(raw);
    let mut output = root.to_path_buf();
    for component in path.components() {
        match component {
            Component::Normal(part) => output.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "project export path '{raw}' must be relative and stay inside the project"
                ));
            }
        }
    }
    if output == root {
        return Err(format!(
            "project export path '{raw}' must name a file or directory inside the project"
        ));
    }

    let canonical_root = root
        .canonicalize()
        .map_err(|error| format!("cannot resolve project root {}: {error}", root.display()))?;
    let mut ancestor = output.as_path();
    loop {
        match std::fs::symlink_metadata(ancestor) {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                ancestor = ancestor.parent().ok_or_else(|| {
                    format!(
                        "cannot resolve parent of export path '{}'",
                        output.display()
                    )
                })?;
            }
            Err(error) => {
                return Err(format!(
                    "cannot inspect export path '{}': {error}",
                    ancestor.display()
                ));
            }
        }
    }
    let canonical_ancestor = ancestor.canonicalize().map_err(|error| {
        format!(
            "cannot resolve export path ancestor '{}': {error}",
            ancestor.display()
        )
    })?;
    if !canonical_ancestor.starts_with(&canonical_root) {
        return Err(format!(
            "project export path '{raw}' escapes the project through '{}'",
            ancestor.display()
        ));
    }
    Ok(output)
}

/// Entry point for `atelier build`.
pub(crate) async fn run(args: &[String]) -> i32 {
    let options = match parse_args(args) {
        Ok(Command::Build(options)) => options,
        Ok(Command::Help) => {
            println!("{USAGE}");
            return 0;
        }
        Err(error) => {
            eprintln!("atelier build: {error}\n{USAGE}");
            return 2;
        }
    };
    let root = match std::env::current_dir() {
        Ok(root) => root,
        Err(error) => {
            eprintln!("atelier build: cannot read the current directory: {error}");
            return 1;
        }
    };
    let project = match Project::load(&root) {
        Ok(project) => project,
        Err(error) => {
            eprintln!("atelier build: {error}");
            return 1;
        }
    };
    let (store_root, _) = Studio::resolve_home(
        std::env::var_os("ATELIER_HOME").as_deref(),
        &project.root,
        None,
    );
    match execute(&project, &options, &store_root.join("documents")).await {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("atelier build: {error}");
            1
        }
    }
}

async fn execute(project: &Project, options: &Options, docs_dir: &Path) -> Result<(), String> {
    let exports: Vec<&Export> = match &options.only {
        Some(name) => vec![
            project
                .exports
                .iter()
                .find(|export| export.name == *name)
                .ok_or_else(|| {
                    let available = project
                        .exports
                        .iter()
                        .map(|export| export.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!(
                        "no export named '{name}' in {}{}",
                        project.manifest.display(),
                        if available.is_empty() {
                            String::new()
                        } else {
                            format!(" (available: {available})")
                        }
                    )
                })?,
        ],
        None => project.exports.iter().collect(),
    };
    if exports.is_empty() {
        println!(
            "atelier build: no exports configured in {}",
            project.manifest.display()
        );
        return Ok(());
    }

    if options.dry_run {
        let count = exports.len();
        for export in exports {
            println!(
                "dry-run {}: doc_export {}",
                export.name,
                serde_json::to_string(&export.dispatch_args())
                    .map_err(|error| format!("cannot encode export '{}': {error}", export.name))?
            );
        }
        println!("atelier build: {count} export(s) planned");
        return Ok(());
    }

    let atelier = Atelier::with_studio(Arc::new(Mutex::new(Studio::with_docs_dir(
        docs_dir.to_path_buf(),
    ))));
    for export in &exports {
        let result = atelier
            .dispatch("doc_export", export.dispatch_args(), "build")
            .await
            .map_err(|error| format!("{}: {error}", export.name))?;
        if server::is_error_result(&result) {
            let report = server::result_json(&result);
            let detail = report
                .as_ref()
                .and_then(|value| value.get("error"))
                .and_then(Value::as_str)
                .unwrap_or("export failed");
            return Err(format!("{}: {detail}", export.name));
        }
        println!("built {} -> {}", export.name, export.out);
    }
    println!("atelier build: {} export(s) ok", exports.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let serial = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "atelier-project-test-{}-{serial}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(path.join(".atelier")).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_manifest(root: &Path, source: &str) {
        std::fs::write(root.join(MANIFEST_PATH), source).unwrap();
    }

    #[test]
    fn starter_manifest_is_valid_and_empty() {
        let root = TempDir::new();
        write_manifest(&root.0, MANIFEST_TEMPLATE);
        let project = Project::load(&root.0).unwrap();
        assert!(project.exports.is_empty());
    }

    #[test]
    fn parses_every_export_shape_into_dispatch_args() {
        let root = TempDir::new();
        write_manifest(
            &root.0,
            r#"version = 1
[[exports]]
name = "sheet"
doc = "hero"
op = "sheet"
out = "dist/hero.png"
meta = "standard"
scale = 4

[[exports]]
name = "anim"
doc = "hero"
op = "anim"
out = "dist/hero.gif"
format = "gif"
tag = "walk"

[[exports]]
name = "tiles"
doc = "world"
op = "tileset"
out = "dist/world.png"
tile_w = 8
tile_h = 8

[[exports]]
name = "all"
op = "all"
out = "dist/all"

[[exports]]
name = "atlas"
op = "atlas"
out = "dist/atlas.png"
max_width = 256
"#,
        );
        let project = Project::load(&root.0).unwrap();
        assert_eq!(project.exports.len(), 5);
        assert_eq!(project.exports[0].dispatch_args()["doc_id"], "hero");
        assert_eq!(project.exports[0].dispatch_args()["meta"], "standard");
        assert_eq!(project.exports[2].dispatch_args()["tile_w"], 8);
        assert_eq!(project.exports[4].dispatch_args()["max_width"], 256);
    }

    #[test]
    fn rejects_format_typos_and_unsafe_or_duplicate_outputs() {
        let root = TempDir::new();
        for (source, expected) in [
            ("version = 2", "unsupported version"),
            ("version = 1\nwat = true", "unknown top-level key"),
            (
                "version = 1\n[[exports]]\nname='x'\nop='sheet'\nout='x.png'",
                "requires `doc`",
            ),
            (
                "version = 1\n[[exports]]\nname='x'\ndoc='x'\nop='sheet'\nout='../x.png'",
                "stay inside the project",
            ),
            (
                "version = 1\n[[exports]]\nname='x'\ndoc='x'\nop='sheet'\nout='x.png'\nformat='gif'",
                "not valid for op=sheet",
            ),
            (
                "version = 1\n[[exports]]\nname='x'\ndoc='x'\nop='sheet'\nout='x.png'\n[[exports]]\nname='y'\ndoc='y'\nop='sheet'\nout='./x.png'",
                "more than one export writes",
            ),
        ] {
            write_manifest(&root.0, source);
            let error = Project::load(&root.0).unwrap_err();
            assert!(
                error.contains(expected),
                "expected {expected:?}, got {error}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn rejects_an_output_that_escapes_through_a_symlink() {
        use std::os::unix::fs::symlink;

        let root = TempDir::new();
        let outside = TempDir::new();
        symlink(&outside.0, root.0.join("linked")).unwrap();
        write_manifest(
            &root.0,
            "version=1\n[[exports]]\nname='x'\ndoc='x'\nop='sheet'\nout='linked/x.png'",
        );
        let error = Project::load(&root.0).unwrap_err();
        assert!(error.contains("escapes the project"), "got: {error}");
    }

    #[test]
    fn build_arguments_are_strict() {
        assert_eq!(
            parse_args(&["--only".into(), "hero".into(), "--dry-run".into()])
                .ok()
                .and_then(|command| match command {
                    Command::Build(options) => Some(options),
                    Command::Help => None,
                }),
            Some(Options {
                only: Some("hero".into()),
                dry_run: true,
            })
        );
        assert!(parse_args(&["--only".into()]).is_err());
        assert!(parse_args(&["--wat".into()]).is_err());
    }

    #[tokio::test]
    async fn build_exports_through_dispatch() {
        let root = TempDir::new();
        std::fs::create_dir_all(root.0.join(".atelier/documents")).unwrap();
        write_manifest(
            &root.0,
            "version=1\n[[exports]]\nname='hero'\ndoc='hero'\nop='sheet'\nout='dist/hero.png'\nscale=1",
        );
        let atelier = Atelier::with_studio(Arc::new(Mutex::new(Studio::with_docs_dir(
            root.0.join(".atelier/documents"),
        ))));
        let result = atelier
            .dispatch(
                "doc_create",
                json!({"name": "hero", "width": 4, "height": 4}),
                "test",
            )
            .await
            .unwrap();
        assert!(!server::is_error_result(&result));

        let project = Project::load(&root.0).unwrap();
        execute(
            &project,
            &Options {
                only: None,
                dry_run: false,
            },
            &root.0.join(".atelier/documents"),
        )
        .await
        .unwrap();
        assert!(root.0.join("dist/hero.png").is_file());
        assert!(root.0.join("dist/hero.json").is_file());
    }
}
