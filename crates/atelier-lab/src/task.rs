//! The task record: one labelled pixel-art assignment (lab.md item 8).
//!
//! Tasks are JSON records consumed from dataset files, so the struct mirrors
//! the record exactly — serde field names are the frozen on-disk contract.

use std::collections::HashSet;
use std::io::{BufRead, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Style constraints a task imposes on the artwork. Values are free-form
/// vocabulary ("selective", "upper-left", "medium") rather than enums: the
/// label set is dataset content, not env behaviour — the env never branches
/// on them, the prompts and the critic do.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StyleSpec {
    pub outline: String,
    pub lighting: String,
    pub detail: String,
}

/// One pixel-art assignment: prompt + hard constraints + provenance split.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub prompt: String,
    pub category: String,
    pub width: u32,
    pub height: u32,
    pub max_colors: u32,
    #[serde(default)]
    pub must_include: Vec<String>,
    #[serde(default)]
    pub must_avoid: Vec<String>,
    pub style: StyleSpec,
    pub split: String,
}

impl Task {
    /// Validate the deliberately narrow first research domain. Widening this
    /// is a dataset-version decision, not something an individual episode may
    /// silently opt into.
    pub fn validate(&self) -> Result<(), String> {
        if self.id.trim().is_empty() || self.prompt.trim().is_empty() {
            return Err("task id and prompt must be non-empty".into());
        }
        if (self.width, self.height) != (32, 32) {
            return Err(format!(
                "atelier-lab v1 tasks must use a 32x32 canvas, got {}x{}",
                self.width, self.height
            ));
        }
        if !(1..=16).contains(&self.max_colors) {
            return Err(format!(
                "atelier-lab v1 tasks require 1..=16 colors, got {}",
                self.max_colors
            ));
        }
        if !["character", "creature", "item", "prop"].contains(&self.category.as_str()) {
            return Err(format!(
                "unsupported category '{}' — use character|creature|item|prop",
                self.category
            ));
        }
        if !["development", "validation", "frozen_test"].contains(&self.split.as_str()) {
            return Err(format!(
                "unsupported split '{}' — use development|validation|frozen_test",
                self.split
            ));
        }
        Ok(())
    }
}

/// Write the frozen task records as newline-delimited JSON. JSONL keeps large
/// benchmark splits diffable and lets callers stream them later.
pub fn write_tasks_jsonl(path: &Path, tasks: &[Task]) -> Result<(), String> {
    validate_task_set(tasks)?;
    let file = std::fs::File::create(path)
        .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    let mut writer = std::io::BufWriter::new(file);
    for task in tasks {
        serde_json::to_writer(&mut writer, task).map_err(|e| e.to_string())?;
        writer
            .write_all(b"\n")
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    }
    writer
        .flush()
        .map_err(|e| format!("cannot flush {}: {e}", path.display()))
}

/// Read and validate a task JSONL file, rejecting duplicate ids across the
/// complete benchmark rather than allowing the later record to shadow one.
pub fn read_tasks_jsonl(path: &Path) -> Result<Vec<Task>, String> {
    let file =
        std::fs::File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let mut tasks = Vec::new();
    for (line_no, line) in std::io::BufReader::new(file).lines().enumerate() {
        let line = line.map_err(|e| format!("{} line {}: {e}", path.display(), line_no + 1))?;
        if line.trim().is_empty() {
            continue;
        }
        let task: Task = serde_json::from_str(&line)
            .map_err(|e| format!("{} line {}: {e}", path.display(), line_no + 1))?;
        task.validate()
            .map_err(|e| format!("{} line {}: {e}", path.display(), line_no + 1))?;
        tasks.push(task);
    }
    validate_task_set(&tasks)?;
    Ok(tasks)
}

fn validate_task_set(tasks: &[Task]) -> Result<(), String> {
    let mut ids = HashSet::new();
    for task in tasks {
        task.validate()?;
        if !ids.insert(task.id.as_str()) {
            return Err(format!("duplicate task id '{}'", task.id));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lab.md item-8 example, verbatim — the record the datasets write.
    const LAB_MD_EXAMPLE: &str = r#"{
      "id": "character-001",
      "prompt": "A tired knight carrying a chipped red shield",
      "category": "character",
      "width": 32,
      "height": 32,
      "max_colors": 16,
      "must_include": ["knight", "red shield", "visible damage"],
      "must_avoid": [],
      "style": {
        "outline": "selective",
        "lighting": "upper-left",
        "detail": "medium"
      },
      "split": "development"
    }"#;

    #[test]
    fn task_json_roundtrips_the_lab_md_record() {
        let task: Task = serde_json::from_str(LAB_MD_EXAMPLE).unwrap();
        assert_eq!(task.id, "character-001");
        assert_eq!(task.width, 32);
        assert_eq!(task.max_colors, 16);
        assert_eq!(task.style.lighting, "upper-left");
        assert_eq!(task.must_include.len(), 3);
        assert!(task.must_avoid.is_empty());
        let v = serde_json::to_value(&task).unwrap();
        let back: Task = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(back, task);
        // Field names are the frozen record contract — guard the shape.
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(LAB_MD_EXAMPLE).unwrap(),
            v
        );
        task.validate().unwrap();
    }

    #[test]
    fn v1_scope_is_enforced() {
        let mut task: Task = serde_json::from_str(LAB_MD_EXAMPLE).unwrap();
        task.width = 64;
        assert!(task.validate().is_err());
        task.width = 32;
        task.category = "scene".into();
        assert!(task.validate().is_err());
    }

    #[test]
    fn task_jsonl_roundtrips_and_rejects_duplicate_ids() {
        let task: Task = serde_json::from_str(LAB_MD_EXAMPLE).unwrap();
        let path =
            std::env::temp_dir().join(format!("atelier-lab-tasks-{}.jsonl", std::process::id()));
        write_tasks_jsonl(&path, std::slice::from_ref(&task)).unwrap();
        assert_eq!(read_tasks_jsonl(&path).unwrap(), vec![task.clone()]);
        assert!(write_tasks_jsonl(&path, &[task.clone(), task]).is_err());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn shipped_development_pack_is_valid_and_balanced() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tasks/development.jsonl");
        let tasks = read_tasks_jsonl(&path).unwrap();
        assert_eq!(tasks.len(), 40);
        assert!(tasks.iter().all(|task| task.split == "development"));

        let mut categories = std::collections::BTreeMap::new();
        for task in tasks {
            *categories.entry(task.category).or_insert(0usize) += 1;
        }
        assert_eq!(categories.get("character"), Some(&10));
        assert_eq!(categories.get("creature"), Some(&10));
        assert_eq!(categories.get("item"), Some(&10));
        assert_eq!(categories.get("prop"), Some(&10));
    }
}
