//! The task record: one labelled pixel-art assignment (lab.md item 8).
//!
//! Tasks are JSON records consumed from dataset files, so the struct mirrors
//! the record exactly — serde field names are the frozen on-disk contract.

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
    }
}
