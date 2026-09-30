//! Import old command logs, or export a working document as a replay bundle.
use atelier_studio::Studio;
use serde_json::Value;
use std::path::{Path, PathBuf};
const HELP: &str = "usage: atelier migrate <legacy.jsonl|doc-id> <new.atelier|new-directory> [--home DIR]\nWrites a verified editable recipe; the original is untouched.\n       atelier migrate --store [--home DIR]\nConverts every live document in the selected store, preserving ids and checkpoints.";
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
    let mut store = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--store" if !store => store = true,
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
    if store {
        if !positional.is_empty() {
            return Err(HELP.into());
        }
        return migrate_store(home).await;
    }
    if positional.len() != 2 {
        return Err(HELP.into());
    }
    let input = positional[0];
    let destination = Path::new(positional[1]);
    if std::fs::symlink_metadata(destination).is_ok() {
        return Err("destination already exists".into());
    }
    if !Path::new(input).is_file() && input.parse::<atelier_studio::DocumentId>().is_ok() {
        let existing = home.map_or_else(Studio::new, |h| Studio::with_home(h.into()));
        return existing.export_recipe(input, destination);
    }
    let recipe = crate::legacy::load_recipe(input, home)?;
    let temp = Temporary::new()?;
    let studio = Studio::with_home(temp.0.clone());
    let id = crate::legacy::rebuild(&recipe, &studio, true).await?;
    studio.compact_import(&id)?;
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

async fn migrate_store(home: Option<&str>) -> Result<Value, String> {
    let studio = home.map_or_else(Studio::new, |h| Studio::with_home(h.into()));
    let docs = studio.list_docs();
    let mut results = vec![];
    for doc in docs["documents"]
        .as_array()
        .ok_or("cannot list documents")?
    {
        if let Some(error) = doc.get("error") {
            return Err(format!("cannot migrate an invalid document: {error}"));
        }
        let id = doc["doc_id"].as_str().ok_or("document has no id")?;
        let file = studio.recipe_path(id)?;
        let native = file
            .parent()
            .and_then(Path::parent)
            .ok_or("invalid store path")?;
        if let Some(result) = migrate_state(&studio, id, native, None, home).await? {
            results.push(result);
        }
        let checkpoints = native.join(".checkpoints");
        if checkpoints.exists() {
            if !std::fs::symlink_metadata(&checkpoints)
                .map_err(|e| e.to_string())?
                .is_dir()
            {
                return Err("checkpoints must be a real directory".into());
            }
            let mut entries = std::fs::read_dir(&checkpoints)
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            entries.sort_by_key(|e| e.file_name());
            for entry in entries {
                if !entry.file_type().map_err(|e| e.to_string())?.is_dir() {
                    return Err("checkpoint must be a real directory".into());
                }
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| "invalid checkpoint name")?;
                if let Some(result) =
                    migrate_state(&studio, id, &entry.path(), Some(&name), home).await?
                {
                    results.push(result);
                }
            }
        }
    }
    Ok(serde_json::json!({"ok":true,"migrated":results.len(),"documents":results}))
}
async fn migrate_state(
    studio: &Studio,
    id: &str,
    native: &Path,
    checkpoint: Option<&str>,
    home: Option<&str>,
) -> Result<Option<Value>, String> {
    let file = native.join(atelier_studio::source::RECIPE_PATH);
    let legacy = native.join(atelier_studio::JOURNAL_FILE);
    let temp = Temporary::new()?;
    let source = if file.is_file() {
        let source = atelier_studio::source::Source::load(&file)?;
        if !legacy.exists() {
            return Ok(None);
        }
        source
    } else if legacy.is_file() {
        let recipe = crate::legacy::load_recipe(legacy.to_str().ok_or("invalid path")?, home)?;
        let importer = Studio::with_home(temp.0.join("store"));
        let rebuilt = crate::legacy::rebuild(&recipe, &importer, true).await?;
        importer.compact_import(&rebuilt)?;
        let out = temp.0.join("migrated");
        importer.export_recipe(&rebuilt, &out)?;
        atelier_studio::source::Source::load(&out)?
    } else {
        atelier_studio::source::Source::from_native(native)?
    };
    let result = match checkpoint {
        Some(cp) => studio.install_checkpoint_recipe(id, cp, &source)?,
        None => studio.install_recipe(id, &source)?,
    };
    Ok(Some(result))
}
