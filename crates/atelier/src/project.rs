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

mod transaction;
use transaction::{BuildLock, BuildWorkspace, promote_outputs};

const MANIFEST_PATH: &str = ".atelier/project.toml";
const MANIFEST_VERSION: i64 = 1;
const USAGE: &str = "usage: atelier build [--only NAME] [--dry-run]";

/// The starter manifest is useful without inventing a sample document that
/// does not exist. Uncommenting the example is enough to opt into a build.
pub(crate) const MANIFEST_TEMPLATE: &str = r#"# Atelier project manifest.
# Output paths are relative to the project root; .atelier is reserved.
# Generated sidecars participate in collision checks.
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
pub(crate) struct Project {
    root: PathBuf,
    manifest: PathBuf,
    exports: Vec<Export>,
}

#[derive(Debug)]
pub(crate) struct Export {
    name: String,
    op: String,
    doc: Option<String>,
    out: String,
    out_path: PathBuf,
    scale: Option<u32>,
    params: Map<String, Value>,
}

impl Export {
    pub(crate) fn dispatch_args(&self) -> Value {
        self.dispatch_args_to(&self.out_path)
    }

    fn dispatch_args_to(&self, out_path: &Path) -> Value {
        let mut args = self.params.clone();
        args.insert("op".into(), json!(self.op));
        args.insert(
            "out_path".into(),
            json!(out_path.to_string_lossy().as_ref()),
        );
        if let Some(doc) = &self.doc {
            args.insert("doc_id".into(), json!(doc));
        }
        if let Some(scale) = self.scale {
            args.insert("scale".into(), json!(scale));
        }
        Value::Object(args)
    }

    /// Every path an export owns. Sidecars participate in collision detection
    /// and transactional promotion just like the primary output.
    fn output_claims(&self) -> Vec<PathBuf> {
        match self.op.as_str() {
            "sheet" | "atlas" => {
                vec![self.out_path.clone(), self.out_path.with_extension("json")]
            }
            "tileset" => vec![
                self.out_path.clone(),
                self.out_path.with_extension("tsx"),
                self.out_path.with_extension("json"),
            ],
            "anim" | "all" => vec![self.out_path.clone()],
            _ => unreachable!("export op was validated"),
        }
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn op(&self) -> &str {
        &self.op
    }

    pub(crate) fn doc(&self) -> Option<&str> {
        self.doc.as_deref()
    }

    pub(crate) fn out(&self) -> &str {
        &self.out
    }

    pub(crate) fn tag(&self) -> Option<&str> {
        self.params.get("tag").and_then(Value::as_str)
    }
}

impl Project {
    pub(crate) fn load(root: &Path) -> Result<Self, String> {
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
        let mut outputs: Vec<(String, PathBuf, PathBuf)> = Vec::new();
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
            for output in export.output_claims() {
                let identity = canonicalize_with_missing(&output)?;
                if let Some((owner, existing, _)) = outputs
                    .iter()
                    .find(|(_, _, existing)| portable_paths_overlap(existing, &identity))
                {
                    let conflict = if owner == &export.name {
                        format!(
                            "export '{}' writes overlapping paths '{}' and '{}'",
                            export.name,
                            display_output(root, existing),
                            display_output(root, &output),
                        )
                    } else {
                        format!(
                            "more than one export writes overlapping paths: '{}' ({owner}) and '{}' ({})",
                            display_output(root, existing),
                            display_output(root, &output),
                            export.name,
                        )
                    };
                    return Err(format!("{}: {conflict}", manifest.display()));
                }
                outputs.push((export.name.clone(), output, identity));
            }
            exports.push(export);
        }

        Ok(Self {
            root: root.to_path_buf(),
            manifest,
            exports,
        })
    }

    pub(crate) fn manifest(&self) -> &Path {
        &self.manifest
    }

    pub(crate) fn exports(&self) -> &[Export] {
        &self.exports
    }

    /// Whether a configured deliverable depends on this document. Library-wide
    /// exports consume every document; per-document exports name their target.
    pub(crate) fn requires_recipe(&self, id: &str) -> bool {
        self.exports.iter().any(|export| {
            matches!(export.op.as_str(), "all" | "atlas") || export.doc.as_deref() == Some(id)
        })
    }
}

fn display_output<'a>(root: &Path, output: &'a Path) -> std::borrow::Cow<'a, str> {
    output
        .strip_prefix(root)
        .unwrap_or(output)
        .to_string_lossy()
}

/// Compare paths with a deliberately stricter, case-folded component model so
/// a manifest accepted on Linux cannot alias on default Windows/macOS filesystems.
fn portable_paths_overlap(left: &Path, right: &Path) -> bool {
    let components = |path: &Path| {
        path.components()
            .map(|component| component.as_os_str().to_string_lossy().to_lowercase())
            .collect::<Vec<_>>()
    };
    let left = components(left);
    let right = components(right);
    left.starts_with(&right) || right.starts_with(&left)
}

/// Resolve the existing prefix of a possibly-not-yet-created path, then append
/// its missing suffix. This gives collision checks the physical identity behind
/// in-project symlink aliases without requiring the output itself to exist.
fn canonicalize_with_missing(path: &Path) -> Result<PathBuf, String> {
    let mut ancestor = path;
    let mut suffix = Vec::new();
    loop {
        match std::fs::symlink_metadata(ancestor) {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = ancestor.file_name().ok_or_else(|| {
                    format!("cannot resolve parent of output path '{}'", path.display())
                })?;
                suffix.push(name.to_os_string());
                ancestor = ancestor.parent().ok_or_else(|| {
                    format!("cannot resolve parent of output path '{}'", path.display())
                })?;
            }
            Err(error) => {
                return Err(format!(
                    "cannot inspect output path '{}': {error}",
                    ancestor.display()
                ));
            }
        }
    }
    let mut resolved = ancestor.canonicalize().map_err(|error| {
        format!(
            "cannot resolve output path ancestor '{}': {error}",
            ancestor.display()
        )
    })?;
    for component in suffix.into_iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

fn validate_portable_component(component: &std::ffi::OsStr) -> Result<(), &'static str> {
    let value = component.to_string_lossy();
    if value.ends_with(['.', ' ']) {
        return Err("components cannot end with a dot or space");
    }
    if value
        .chars()
        .any(|character| character.is_control() || r#"<>:"|?*"#.contains(character))
    {
        return Err("components contain characters unsupported on Windows");
    }
    let stem = value
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|prefix| {
            stem.strip_prefix(prefix).is_some_and(|number| {
                matches!(number, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
            })
        });
    if reserved {
        return Err("components cannot use a reserved Windows device name");
    }
    Ok(())
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
    if raw.contains('\\') {
        return Err(format!(
            "project export path '{raw}' must use '/' as its portable separator"
        ));
    }
    let path = Path::new(raw);
    let mut output = root.to_path_buf();
    let mut first_component = true;
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                validate_portable_component(part)
                    .map_err(|reason| format!("project export path '{raw}': {reason}"))?;
                if first_component && part.to_string_lossy().eq_ignore_ascii_case(".atelier") {
                    return Err(format!(
                        "project export path '{raw}' cannot write inside the reserved .atelier directory"
                    ));
                }
                first_component = false;
                output.push(part);
            }
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
    let canonical_output = canonicalize_with_missing(&output)?;
    if !canonical_output.starts_with(&canonical_root) {
        return Err(format!(
            "project export path '{raw}' escapes the project through '{}'",
            canonical_output.display()
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

    let _build_lock = BuildLock::acquire(&project.root)?;
    let workspace = BuildWorkspace::new(&project.root)?;
    let studio = Studio::with_docs_dir(docs_dir.to_path_buf());
    // One source snapshot feeds the complete staged build. A mutation waits
    // until every export has succeeded and its outputs have been promoted.
    let _store_lock = studio.lock_store_shared()?;
    let atelier = Atelier::with_studio(Arc::new(Mutex::new(studio)));
    for export in &exports {
        let staged = workspace.staged_output(&project.root, &export.out_path)?;
        let result = atelier
            .dispatch("doc_export", export.dispatch_args_to(&staged), "build")
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
    }
    promote_outputs(project, &exports, &workspace)?;
    for export in &exports {
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
            (
                "version = 1\n[[exports]]\nname='x'\ndoc='x'\nop='sheet'\nout='dist/Hero.png'\n[[exports]]\nname='y'\ndoc='y'\nop='sheet'\nout='dist/hero.png'",
                "more than one export writes",
            ),
            (
                "version = 1\n[[exports]]\nname='sheet'\ndoc='x'\nop='sheet'\nout='dist/x.png'\n[[exports]]\nname='meta'\ndoc='x'\nop='anim'\nout='dist/x.json'",
                "more than one export writes",
            ),
            (
                "version = 1\n[[exports]]\nname='all'\nop='all'\nout='dist'\n[[exports]]\nname='anim'\ndoc='x'\nop='anim'\nout='dist/x.gif'",
                "more than one export writes",
            ),
            (
                "version = 1\n[[exports]]\nname='x'\ndoc='x'\nop='sheet'\nout='dist/x.json'",
                "writes overlapping paths",
            ),
            (
                "version = 1\n[[exports]]\nname='x'\ndoc='x'\nop='anim'\nout='./.atelier/x.gif'",
                "reserved .atelier",
            ),
            (
                "version = 1\n[[exports]]\nname='x'\ndoc='x'\nop='anim'\nout='dist/con.gif'",
                "reserved Windows",
            ),
            (
                "version = 1\n[[exports]]\nname='x'\ndoc='x'\nop='anim'\nout='dist/bad.'",
                "end with a dot",
            ),
            (
                "version = 1\n[[exports]]\nname='x'\ndoc='x'\nop='anim'\nout='dist/a:b.gif'",
                "unsupported on Windows",
            ),
            (
                "version = 1\n[[exports]]\nname='x'\ndoc='x'\nop='anim'\nout='dist\\x.gif'",
                "portable separator",
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

    #[cfg(unix)]
    #[test]
    fn rejects_outputs_that_alias_through_two_in_project_symlinks() {
        use std::os::unix::fs::symlink;

        let root = TempDir::new();
        std::fs::create_dir(root.0.join("assets")).unwrap();
        symlink(root.0.join("assets"), root.0.join("alias-a")).unwrap();
        symlink(root.0.join("assets"), root.0.join("alias-b")).unwrap();
        write_manifest(
            &root.0,
            "version=1\n\
             [[exports]]\nname='a'\ndoc='x'\nop='anim'\nout='alias-a/x.gif'\n\
             [[exports]]\nname='b'\ndoc='x'\nop='anim'\nout='alias-b/x.gif'\n",
        );
        let error = Project::load(&root.0).unwrap_err();
        assert!(
            error.contains("more than one export writes"),
            "got: {error}"
        );
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
        std::fs::create_dir_all(root.0.join("dist")).unwrap();
        std::fs::write(root.0.join("dist/hero.png"), b"old image").unwrap();
        std::fs::write(root.0.join("dist/hero.json"), b"old metadata").unwrap();

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
        assert_ne!(
            std::fs::read(root.0.join("dist/hero.png")).unwrap(),
            b"old image"
        );
        assert_ne!(
            std::fs::read(root.0.join("dist/hero.json")).unwrap(),
            b"old metadata"
        );
        let metadata: Value =
            serde_json::from_slice(&std::fs::read(root.0.join("dist/hero.json")).unwrap()).unwrap();
        assert_eq!(
            metadata["path"],
            root.0.join("dist/hero.png").to_string_lossy().as_ref()
        );
    }

    #[tokio::test]
    async fn library_build_metadata_names_the_final_output_not_staging() {
        let root = TempDir::new();
        let docs = root.0.join(".atelier/documents");
        std::fs::create_dir_all(&docs).unwrap();
        write_manifest(
            &root.0,
            "version=1\n[[exports]]\nname='all'\nop='all'\nout='dist/all'\nscale=1",
        );
        let atelier =
            Atelier::with_studio(Arc::new(Mutex::new(Studio::with_docs_dir(docs.clone()))));
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
            &docs,
        )
        .await
        .unwrap();
        let metadata: Value =
            serde_json::from_slice(&std::fs::read(root.0.join("dist/all/hero.json")).unwrap())
                .unwrap();
        assert_eq!(
            metadata["path"],
            root.0.join("dist/all/hero.png").to_string_lossy().as_ref()
        );
    }

    #[tokio::test]
    async fn a_failed_build_preserves_every_previous_output() {
        let root = TempDir::new();
        let docs = root.0.join(".atelier/documents");
        std::fs::create_dir_all(&docs).unwrap();
        write_manifest(
            &root.0,
            "version=1\n\
             [[exports]]\n\
             name='hero'\n\
             doc='hero'\n\
             op='sheet'\n\
             out='dist/hero.png'\n\
             scale=1\n\
             [[exports]]\n\
             name='missing'\n\
             doc='missing'\n\
             op='sheet'\n\
             out='dist/missing.png'\n\
             scale=1\n",
        );
        let atelier =
            Atelier::with_studio(Arc::new(Mutex::new(Studio::with_docs_dir(docs.clone()))));
        let result = atelier
            .dispatch(
                "doc_create",
                json!({"name": "hero", "width": 4, "height": 4}),
                "test",
            )
            .await
            .unwrap();
        assert!(!server::is_error_result(&result));
        std::fs::create_dir_all(root.0.join("dist")).unwrap();
        std::fs::write(root.0.join("dist/hero.png"), b"previous image").unwrap();
        std::fs::write(root.0.join("dist/hero.json"), b"previous metadata").unwrap();

        let project = Project::load(&root.0).unwrap();
        let error = execute(
            &project,
            &Options {
                only: None,
                dry_run: false,
            },
            &docs,
        )
        .await
        .unwrap_err();
        assert!(error.contains("missing"), "got: {error}");
        assert_eq!(
            std::fs::read(root.0.join("dist/hero.png")).unwrap(),
            b"previous image"
        );
        assert_eq!(
            std::fs::read(root.0.join("dist/hero.json")).unwrap(),
            b"previous metadata"
        );
        assert!(!root.0.join("dist/missing.png").exists());
    }
}
