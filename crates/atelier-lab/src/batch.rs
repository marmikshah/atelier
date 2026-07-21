//! Resumable orchestration records for running one policy across a task pack.
//! The model process remains provider-neutral; this module only plans stable
//! task/seed keys and verifies completed episode provenance before skipping it.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::policy::PolicyUsage;
use crate::recorder::{EventKind, Recorder, EPISODE_LOG_FILE};
use crate::runner::{RunnerReport, RunnerTermination};
use crate::task::Task;

pub const BATCH_FORMAT_VERSION: u32 = 1;
pub const BATCH_RESULTS_FILE: &str = "batch-results.jsonl";

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BatchRunKey {
    pub task_id: String,
    pub policy: String,
    pub seed: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchRunSpec {
    pub task_id: String,
    pub seed: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchSelection {
    pub split: Option<String>,
    pub limit: Option<usize>,
    pub repeats: usize,
    pub base_seed: u64,
}

impl Default for BatchSelection {
    fn default() -> Self {
        BatchSelection {
            split: None,
            limit: None,
            repeats: 1,
            base_seed: 0,
        }
    }
}

impl BatchSelection {
    pub fn validate(&self) -> Result<(), String> {
        if self.repeats == 0 {
            return Err("repeats must be at least 1".into());
        }
        if self.limit == Some(0) {
            return Err("limit must be at least 1".into());
        }
        if let Some(split) = &self.split {
            if !["development", "validation", "frozen_test"].contains(&split.as_str()) {
                return Err(format!(
                    "unsupported split '{split}' — use development|validation|frozen_test"
                ));
            }
        }
        self.base_seed
            .checked_add((self.repeats - 1) as u64)
            .ok_or("base seed plus repeats overflows u64")?;
        Ok(())
    }
}

/// One closed attempt. Only records whose `completed` bit is true are resume
/// candidates; bounded/incomplete attempts remain visible and are retried.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchRecord {
    pub format_version: u32,
    pub task_id: String,
    pub policy: String,
    pub seed: u64,
    /// One direct child of the batch root, kept relative for portability.
    pub episode_dir: PathBuf,
    pub termination: RunnerTermination,
    pub completed: bool,
    pub turns: usize,
    pub accepted_actions: usize,
    pub rejected_actions: usize,
    pub policy_errors: usize,
    pub usage: PolicyUsage,
}

impl BatchRecord {
    pub fn from_report(
        batch_root: &Path,
        episode_dir: &Path,
        report: &RunnerReport,
    ) -> Result<Self, String> {
        let relative = episode_dir.strip_prefix(batch_root).map_err(|_| {
            format!(
                "episode {} is not inside batch root {}",
                episode_dir.display(),
                batch_root.display()
            )
        })?;
        let record = BatchRecord {
            format_version: BATCH_FORMAT_VERSION,
            task_id: report.result.task_id.clone(),
            policy: report.policy.clone(),
            seed: report.result.seed,
            episode_dir: relative.to_path_buf(),
            termination: report.termination,
            completed: report.result.completed,
            turns: report.turns,
            accepted_actions: report.accepted_actions,
            rejected_actions: report.rejected_actions,
            policy_errors: report.policy_errors,
            usage: report.usage.clone(),
        };
        record.validate()?;
        Ok(record)
    }

    pub fn key(&self) -> BatchRunKey {
        BatchRunKey {
            task_id: self.task_id.clone(),
            policy: self.policy.clone(),
            seed: self.seed,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.format_version != BATCH_FORMAT_VERSION {
            return Err(format!(
                "batch record format version {} does not match {}",
                self.format_version, BATCH_FORMAT_VERSION
            ));
        }
        if self.task_id.trim().is_empty() || self.policy.trim().is_empty() {
            return Err("batch task id and policy cannot be empty".into());
        }
        let mut components = self.episode_dir.components();
        if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
            return Err(format!(
                "batch episode_dir must be one relative directory, got {}",
                self.episode_dir.display()
            ));
        }
        if self.turns == 0 {
            return Err("batch record turns must be at least 1".into());
        }
        if self.completed != (self.termination == RunnerTermination::Completed) {
            return Err("batch completed flag disagrees with termination".into());
        }
        Ok(())
    }
}

/// Plan task-major, seed-minor work. `limit` freezes the task subset before
/// resume filtering, so rerunning a 20-task baseline never spills into task 21.
pub fn plan_batch_runs(
    tasks: &[Task],
    policy: &str,
    selection: &BatchSelection,
    completed: &HashSet<BatchRunKey>,
) -> Result<Vec<BatchRunSpec>, String> {
    selection.validate()?;
    if policy.trim().is_empty() {
        return Err("policy name cannot be empty".into());
    }
    let matching: Vec<&Task> = tasks
        .iter()
        .filter(|task| {
            selection
                .split
                .as_ref()
                .is_none_or(|split| &task.split == split)
        })
        .take(selection.limit.unwrap_or(usize::MAX))
        .collect();
    if matching.is_empty() {
        return Err("no tasks match the requested batch selection".into());
    }

    let mut planned = Vec::new();
    for task in matching {
        for repeat in 0..selection.repeats {
            let seed = selection.base_seed + repeat as u64;
            let key = BatchRunKey {
                task_id: task.id.clone(),
                policy: policy.into(),
                seed,
            };
            if !completed.contains(&key) {
                planned.push(BatchRunSpec {
                    task_id: task.id.clone(),
                    seed,
                });
            }
        }
    }
    Ok(planned)
}

pub fn append_batch_record(batch_root: &Path, record: &BatchRecord) -> Result<(), String> {
    record.validate()?;
    std::fs::create_dir_all(batch_root)
        .map_err(|e| format!("cannot create {}: {e}", batch_root.display()))?;
    let path = batch_root.join(BATCH_RESULTS_FILE);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    serde_json::to_writer(&mut file, record).map_err(|e| e.to_string())?;
    file.write_all(b"\n")
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

pub fn read_batch_records(batch_root: &Path) -> Result<Vec<BatchRecord>, String> {
    let path = batch_root.join(BATCH_RESULTS_FILE);
    let body = match std::fs::read_to_string(&path) {
        Ok(body) => body,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    let nonempty: Vec<(usize, &str)> = body
        .lines()
        .enumerate()
        .map(|(line, value)| (line, value.trim()))
        .filter(|(_, value)| !value.is_empty())
        .collect();
    let last = nonempty.len().saturating_sub(1);
    let mut records = Vec::with_capacity(nonempty.len());
    for (index, (line_no, line)) in nonempty.iter().enumerate() {
        let record: BatchRecord = match serde_json::from_str(line) {
            Ok(record) => record,
            Err(_) if index == last => break,
            Err(error) => return Err(format!("{} line {}: {error}", path.display(), line_no + 1)),
        };
        record
            .validate()
            .map_err(|error| format!("{} line {}: {error}", path.display(), line_no + 1))?;
        records.push(record);
    }
    Ok(records)
}

/// Return only verified completed keys. A stale or tampered manifest fails
/// loudly instead of causing the batch to skip a mismatched episode.
pub fn completed_batch_keys(batch_root: &Path) -> Result<HashSet<BatchRunKey>, String> {
    let records = read_batch_records(batch_root)?;
    let mut completed = HashSet::new();
    for record in records.iter().filter(|record| record.completed) {
        verify_completed_episode(batch_root, record)?;
        completed.insert(record.key());
    }
    Ok(completed)
}

fn verify_completed_episode(batch_root: &Path, record: &BatchRecord) -> Result<(), String> {
    let episode_dir = batch_root.join(&record.episode_dir);
    let events = Recorder::read(&episode_dir.join(EPISODE_LOG_FILE))?;
    let mut reset_task = None;
    let mut policy_calls = 0usize;
    let mut finish = None;
    for event in events {
        match event.event {
            EventKind::Reset { task, .. } => reset_task = Some(task.id),
            EventKind::PolicyCall { policy, .. } => {
                if policy != record.policy {
                    return Err(format!(
                        "episode {} policy '{}' does not match batch policy '{}'",
                        episode_dir.display(),
                        policy,
                        record.policy
                    ));
                }
                policy_calls += 1;
            }
            EventKind::Finish { result, .. } => finish = Some((event.session_id, result)),
            _ => {}
        }
    }
    let task_id = reset_task
        .ok_or_else(|| format!("episode {} has no reset event", episode_dir.display()))?;
    let (session_id, result) =
        finish.ok_or_else(|| format!("episode {} has no finish event", episode_dir.display()))?;
    if task_id != record.task_id
        || result.task_id != record.task_id
        || result.seed != record.seed
        || !result.completed
        || policy_calls == 0
        || session_id != record.episode_dir.to_string_lossy()
    {
        return Err(format!(
            "episode {} provenance does not match its completed batch record",
            episode_dir.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::StyleSpec;

    fn task(id: &str, split: &str) -> Task {
        Task {
            id: id.into(),
            prompt: format!("Draw {id}"),
            category: "item".into(),
            width: 32,
            height: 32,
            max_colors: 8,
            must_include: vec![],
            must_avoid: vec![],
            style: StyleSpec {
                outline: "selective".into(),
                lighting: "upper-left".into(),
                detail: "medium".into(),
            },
            split: split.into(),
        }
    }

    #[test]
    fn planning_limits_before_resume_and_expands_stable_seeds() {
        let tasks = vec![
            task("item-1", "development"),
            task("item-2", "development"),
            task("item-3", "development"),
            task("item-test", "frozen_test"),
        ];
        let selection = BatchSelection {
            split: Some("development".into()),
            limit: Some(2),
            repeats: 2,
            base_seed: 7,
        };
        let completed = HashSet::from([BatchRunKey {
            task_id: "item-1".into(),
            policy: "model-a".into(),
            seed: 7,
        }]);
        let planned = plan_batch_runs(&tasks, "model-a", &selection, &completed).unwrap();
        assert_eq!(
            planned,
            vec![
                BatchRunSpec {
                    task_id: "item-1".into(),
                    seed: 8,
                },
                BatchRunSpec {
                    task_id: "item-2".into(),
                    seed: 7,
                },
                BatchRunSpec {
                    task_id: "item-2".into(),
                    seed: 8,
                },
            ]
        );
    }

    #[test]
    fn records_reject_non_portable_episode_paths() {
        let record = BatchRecord {
            format_version: BATCH_FORMAT_VERSION,
            task_id: "item-1".into(),
            policy: "model-a".into(),
            seed: 0,
            episode_dir: PathBuf::from("../escape"),
            termination: RunnerTermination::Completed,
            completed: true,
            turns: 1,
            accepted_actions: 1,
            rejected_actions: 0,
            policy_errors: 0,
            usage: PolicyUsage::default(),
        };
        assert!(record.validate().is_err());
    }
}
