use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;

use atelier_core::source::Part;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::load::manifest_path;
use super::{Source, format};

/// One guarded source edit. Replacing a grid replaces its entire definition:
/// dots clear previous coverage on rebuild. Unrelated TOML comments survive.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edits {
    #[serde(default)]
    pub inks: BTreeMap<String, String>,
    #[serde(default)]
    pub parts: BTreeMap<String, Part>,
}

impl Source {
    /// Validate and compile a batch before atomically replacing the manifest.
    /// Resources are immutable for this operation; external image edits change
    /// the revision and cause a conflict. A sidecar coordinates CLI writers.
    pub fn apply(path: &Path, expected: &str, edits: Edits) -> Result<Value, String> {
        let path = manifest_path(path);
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let lock_path = parent.join(".atelier-source.lock");
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(0o400000);
        }
        let lock = options.open(lock_path).map_err(|e| e.to_string())?;
        fs4::FileExt::lock(&lock).map_err(|e| e.to_string())?;
        let before = Self::load(&path)?;
        if before.revision != expected {
            return Err(format!(
                "source revision conflict: expected {expected}, current {}",
                before.revision
            ));
        }
        let mut text: toml_edit::DocumentMut = before
            .text
            .parse()
            .map_err(|e| format!("invalid source: {e}"))?;
        for (name, value) in &edits.inks {
            if !before.asset.inks.contains_key(name) {
                return Err(format!("unknown ink '{name}'"));
            }
            let decor = text["inks"][name].as_value().map(|v| v.decor().clone());
            let mut replacement = toml_edit::Value::from(value);
            if let Some(decor) = decor {
                *replacement.decor_mut() = decor;
            }
            text["inks"][name] = toml_edit::Item::Value(replacement);
        }
        for (name, part) in &edits.parts {
            let serialized = serde_json::to_value(part).map_err(|e| e.to_string())?;
            let wrapper: toml_edit::DocumentMut = format!("part = {}", format::inline(&serialized))
                .parse()
                .map_err(|e| format!("invalid part: {e}"))?;
            text["parts"][name] = wrapper["part"].clone();
        }
        let after = Self::from_text(text.to_string(), parent)?;
        after.compile()?;
        let temp = parent.join(format!(".atelier-source-{}.tmp", Uuid::new_v4()));
        let result = (|| {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temp)
                .map_err(|e| e.to_string())?;
            file.set_permissions(
                fs::metadata(&path)
                    .map_err(|e| e.to_string())?
                    .permissions(),
            )
            .map_err(|e| e.to_string())?;
            file.write_all(after.text.as_bytes())
                .and_then(|_| file.sync_all())
                .map_err(|e| e.to_string())?;
            if Self::load(&path)?.revision != before.revision {
                return Err("source changed during the edit; nothing published".into());
            }
            fs::rename(&temp, &path).map_err(|e| e.to_string())?;
            let mut result = json!({"ok": true, "revision": after.revision, "parts": edits.parts.keys().collect::<Vec<_>>(), "inks": edits.inks.keys().collect::<Vec<_>>()});
            if let Err(error) = File::open(parent).and_then(|f| f.sync_all()) {
                result["warning"] =
                    json!(format!("edit published, directory sync failed: {error}"));
            }
            Ok(result)
        })();
        let _ = fs::remove_file(temp);
        result
    }
}
