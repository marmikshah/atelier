//! Import old command logs, or export a working document as a replay bundle.
use atelier_studio::Studio;
use serde_json::Value;
use std::path::{Path, PathBuf};
const HELP: &str = "usage: atelier migrate <legacy.jsonl|doc-id> <new-directory> [--home DIR]\nWrites a verified structured recipe; the original is untouched.";
pub async fn run(args: &[String]) -> i32 {
    if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{HELP}");
        return 0;
    }
    match execute(args).await {
        Ok(v) => {
            println!("{v}");
            0
        }
        Err(e) => {
            eprintln!("migrate: {e}");
            1
        }
    }
}
async fn execute(args: &[String]) -> Result<Value, String> {
    let mut positional = vec![];
    let mut home = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--home" if home.is_none() => {
                i += 1;
                home = Some(
                    args.get(i)
                        .filter(|v| !v.starts_with('-'))
                        .ok_or("--home needs a directory")?
                        .as_str(),
                );
            }
            v if v.starts_with('-') => return Err(format!("unknown flag {v}")),
            v => positional.push(v),
        }
        i += 1;
    }
    if positional.len() != 2 {
        return Err(HELP.into());
    }
    let input = positional[0];
    let destination = Path::new(positional[1]);
    if std::fs::symlink_metadata(destination).is_ok() {
        return Err("destination already exists".into());
    }
    let existing = home.map_or_else(Studio::new, |h| Studio::with_home(h.into()));
    if !Path::new(input).is_file() && input.parse::<atelier_studio::DocumentId>().is_ok() {
        return existing.export_recipe(input, destination);
    }
    let recipe = crate::legacy::load_recipe(input, home)?;
    let temp = Temporary::new()?;
    let studio = Studio::with_home(temp.0.clone());
    let id = crate::legacy::rebuild(&recipe, &studio, true).await?;
    studio.export_recipe(&id, destination)
}
struct Temporary(PathBuf);
impl Temporary {
    fn new() -> Result<Self, String> {
        let p = std::env::temp_dir().join(format!("atelier-import-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&p).map_err(|e| e.to_string())?;
        Ok(Self(p))
    }
}
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
