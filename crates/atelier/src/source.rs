//! Source CLI: focused reads, validated edits and lossless recipe migration.

use atelier_studio::{
    Studio,
    source::{Edits, MigrationMode, Source},
};
use serde_json::Value;
use std::path::{Path, PathBuf};

const HELP: &str = "usage:
  atelier source inspect <source.toml|bundle> [--part NAME]
  atelier source check <source.toml|bundle>
  atelier source apply <source.toml|bundle> --file EDITS.json --expected-revision HASH
  atelier source migrate <recipe.jsonl|doc-id> <new-bundle-dir> [--mode auto|pixels|procedural] [--home DIR]

Rebuild: atelier replay <source.toml|bundle> [--home DIR]
Migrate never overwrites the destination or deletes the original. Auto uses
readable grids for small parts, PNGs for large pixels, or a smaller verified
construction graph. Procedural mode fails when recovery is unsupported.
Edits: {\"inks\":{\"name\":\"#RRGGBB\"},\"parts\":{\"name\":{...complete part...}}}
Source revisions cover the manifest and every referenced PNG. Inspect omits
bulk artwork unless --part is supplied. Check compiles without publishing.";

pub async fn run(args: &[String]) -> i32 {
    if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{HELP}");
        return 0;
    }
    match execute(args).await {
        Ok(value) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&value).unwrap_or_default()
            );
            0
        }
        Err(error) => {
            eprintln!("source: {error}");
            1
        }
    }
}

async fn execute(args: &[String]) -> Result<Value, String> {
    let command = args[0].as_str();
    let allowed: &[&str] = match command {
        "inspect" => &["--part"],
        "check" => &[],
        "apply" => &["--file", "--expected-revision"],
        "migrate" => &["--mode", "--home"],
        _ => return Err(format!("unknown command '{command}'\n{HELP}")),
    };
    let mut positional = vec![];
    let mut flags = std::collections::BTreeMap::new();
    let mut i = 1;
    while i < args.len() {
        let a = args[i].as_str();
        if a.starts_with('-') {
            if !allowed.contains(&a) {
                return Err(format!("unknown flag {a}"));
            }
            let value = args
                .get(i + 1)
                .filter(|v| !v.starts_with("--"))
                .ok_or_else(|| format!("{a} needs a value"))?;
            if flags.insert(a, value.as_str()).is_some() {
                return Err(format!("duplicate flag {a}"));
            }
            i += 2;
        } else {
            positional.push(a);
            i += 1;
        }
    }
    if positional.len() != if command == "migrate" { 2 } else { 1 } {
        return Err(HELP.into());
    }
    let input = Path::new(positional[0]);
    match command {
        "inspect" => Source::load(input)?.inspect(flags.get("--part").copied()),
        "check" => {
            let source = Source::load(input)?;
            source.compile()?;
            Ok(serde_json::json!({"ok":true,"revision":source.revision()}))
        }
        "apply" => {
            let file = flags.get("--file").ok_or("apply needs --file")?;
            let expected = flags
                .get("--expected-revision")
                .ok_or("apply needs --expected-revision")?;
            let text = crate::replay::read_source(Path::new(file), "edit batch")?;
            let edits: Edits = serde_json::from_str(&text).map_err(|e| e.to_string())?;
            Source::apply(input, expected, edits)
        }
        "migrate" => {
            let mode = match flags.get("--mode").copied().unwrap_or("auto") {
                "auto" => MigrationMode::Auto,
                "pixels" => MigrationMode::Pixels,
                "procedural" => MigrationMode::Procedural,
                _ => return Err("mode must be auto, pixels or procedural".into()),
            };
            let destination = Path::new(positional[1]);
            if std::fs::symlink_metadata(destination).is_ok() {
                return Err("migration destination already exists".into());
            }
            let (recipe, baseline) =
                crate::replay::load_recipe(positional[0], flags.get("--home").copied())?;
            let temp = Temporary::new()?;
            let studio = Studio::with_home(temp.0.clone());
            let id = crate::replay::rebuild(&recipe, &studio, baseline.as_ref(), true).await?;
            studio.migrate_source(&id, destination, mode)
        }
        _ => unreachable!(),
    }
}

struct Temporary(PathBuf);
impl Temporary {
    fn new() -> Result<Self, String> {
        let path = std::env::temp_dir().join(format!("atelier-migration-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).map_err(|e| e.to_string())?;
        Ok(Self(path))
    }
}
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
