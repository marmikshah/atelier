use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use atelier_lab::{
    append_batch_record, bundle_episode_comparisons, completed_batch_keys,
    export_annotated_critic_jsonl, export_generator_sft, plan_batch_runs, read_tasks_jsonl, replay,
    run_policy_episode, AtelierEnv, BatchRecord, BatchSelection, CommandPolicy, RunnerConfig,
    BATCH_RESULTS_FILE,
};

const USAGE: &str = "atelier-lab data tools

usage:
  atelier-lab validate-tasks <tasks.jsonl>
  atelier-lab bundle <pairs.jsonl> <output-dir>
  atelier-lab export-critic <comparisons.jsonl> <annotations.jsonl> <output.jsonl>
  atelier-lab export-generator <episodes.jsonl> <output-dir>
  atelier-lab replay <episode-dir> <replay-root>
  atelier-lab run-policy <tasks.jsonl> <task-id> <episode-root> --policy <program> [options]
  atelier-lab run-batch <tasks.jsonl> <episode-root> --policy <program> [options]

policy options:
  --policy-arg <arg>       argument passed directly to the policy (repeatable)
  --name <name>            policy/model provenance label (default: program)
  --seed <n>               deterministic episode seed (default: 0)
  --max-turns <n>          model-call limit (default: 40)
  --max-policy-errors <n>  provider/protocol error limit (default: 3)
  --timeout-secs <n>       per-call command timeout (default: 300)

run-batch options:
  --split <name>           development, validation, or frozen_test (default: all)
  --limit <n>              fixed number of tasks selected before resume filtering
  --repeats <n>            seeds per selected task (default: 1)
  --no-resume              rerun completed task/policy/seed keys";

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}\n\n{USAGE}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [command, tasks] if command == "validate-tasks" => {
            let tasks = read_tasks_jsonl(Path::new(tasks))?;
            let mut splits = BTreeMap::new();
            for task in &tasks {
                *splits.entry(task.split.as_str()).or_insert(0usize) += 1;
            }
            println!("valid tasks: {}", tasks.len());
            for (split, count) in splits {
                println!("  {split}: {count}");
            }
            Ok(())
        }
        [command, manifest, output] if command == "bundle" => {
            let comparisons = bundle_episode_comparisons(Path::new(manifest), Path::new(output))?;
            println!("bundled {} comparisons in {}", comparisons.len(), output);
            Ok(())
        }
        [command, comparisons, annotations, output] if command == "export-critic" => {
            if let Some(parent) = Path::new(output).parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
            }
            let count = export_annotated_critic_jsonl(
                Path::new(comparisons),
                Path::new(annotations),
                Path::new(output),
            )?;
            println!("exported {count} critic examples to {output}");
            Ok(())
        }
        [command, manifest, output] if command == "export-generator" => {
            let examples = export_generator_sft(Path::new(manifest), Path::new(output))?;
            println!(
                "exported {} generator examples to {}",
                examples.len(),
                output
            );
            Ok(())
        }
        [command, episode, replay_root] if command == "replay" => {
            let report = replay(Path::new(episode), Path::new(replay_root))?;
            println!(
                "{}",
                serde_json::to_string(&report).map_err(|e| e.to_string())?
            );
            if report.matched {
                Ok(())
            } else {
                Err("episode replay diverged".into())
            }
        }
        [command, rest @ ..] if command == "run-policy" => run_policy(rest),
        [command, rest @ ..] if command == "run-batch" => run_batch(rest),
        _ => Err("invalid arguments".into()),
    }
}

fn run_policy(args: &[String]) -> Result<(), String> {
    if args.len() < 3 {
        return Err("run-policy needs <tasks.jsonl> <task-id> <episode-root>".into());
    }
    let tasks_path = &args[0];
    let task_id = &args[1];
    let episode_root = &args[2];
    let mut program = None;
    let mut policy_args = Vec::new();
    let mut name = None;
    let mut seed = 0u64;
    let mut config = RunnerConfig::default();
    let mut timeout_secs = 300u64;

    let mut i = 3;
    while i < args.len() {
        let value = |index: usize| -> Result<&String, String> {
            args.get(index + 1)
                .ok_or_else(|| format!("{} needs a value", args[index]))
        };
        match args[i].as_str() {
            "--policy" => {
                program = Some(value(i)?.clone());
                i += 2;
            }
            "--policy-arg" => {
                policy_args.push(value(i)?.clone());
                i += 2;
            }
            "--name" => {
                name = Some(value(i)?.clone());
                i += 2;
            }
            "--seed" => {
                seed = value(i)?
                    .parse()
                    .map_err(|_| "--seed must be an unsigned integer".to_string())?;
                i += 2;
            }
            "--max-turns" => {
                config.max_turns = value(i)?
                    .parse()
                    .map_err(|_| "--max-turns must be an integer".to_string())?;
                i += 2;
            }
            "--max-policy-errors" => {
                config.max_policy_errors = value(i)?
                    .parse()
                    .map_err(|_| "--max-policy-errors must be an integer".to_string())?;
                i += 2;
            }
            "--timeout-secs" => {
                timeout_secs = value(i)?
                    .parse()
                    .map_err(|_| "--timeout-secs must be an unsigned integer".to_string())?;
                i += 2;
            }
            other => return Err(format!("unknown run-policy argument '{other}'")),
        }
    }
    config.validate()?;
    if timeout_secs == 0 {
        return Err("--timeout-secs must be at least 1".into());
    }
    let program = program.ok_or("run-policy requires --policy <program>")?;
    let policy_name = name.unwrap_or_else(|| program.clone());
    let tasks = read_tasks_jsonl(Path::new(tasks_path))?;
    let task = tasks
        .iter()
        .find(|task| &task.id == task_id)
        .ok_or_else(|| format!("task '{task_id}' not found in {tasks_path}"))?;
    let mut env = AtelierEnv::new(episode_root, seed)?;
    let mut policy = CommandPolicy::new(
        policy_name,
        &program,
        policy_args,
        Duration::from_secs(timeout_secs),
    )?;
    let report = run_policy_episode(&mut env, task, &mut policy, &config)?;
    eprintln!(
        "atelier-lab: {:?} after {} turn(s); episode {}",
        report.termination,
        report.turns,
        env.episode_dir().display()
    );
    let summary = serde_json::json!({
        "policy": report.policy,
        "turns": report.turns,
        "accepted_actions": report.accepted_actions,
        "rejected_actions": report.rejected_actions,
        "policy_errors": report.policy_errors,
        "usage": report.usage,
        "termination": report.termination,
        "episode_id": report.result.episode_id,
        "episode_dir": env.episode_dir(),
        "task_id": report.result.task_id,
        "steps": report.result.steps,
        "stage": report.result.stage,
        "completed": report.result.completed,
    });
    println!(
        "{}",
        serde_json::to_string(&summary).map_err(|e| e.to_string())?
    );
    Ok(())
}

fn run_batch(args: &[String]) -> Result<(), String> {
    if args.len() < 2 {
        return Err("run-batch needs <tasks.jsonl> <episode-root>".into());
    }
    let tasks_path = &args[0];
    let episode_root = Path::new(&args[1]);
    let mut program = None;
    let mut policy_args = Vec::new();
    let mut name = None;
    let mut selection = BatchSelection::default();
    let mut config = RunnerConfig::default();
    let mut timeout_secs = 300u64;
    let mut resume = true;

    let mut i = 2;
    while i < args.len() {
        let value = |index: usize| -> Result<&String, String> {
            args.get(index + 1)
                .ok_or_else(|| format!("{} needs a value", args[index]))
        };
        match args[i].as_str() {
            "--policy" => {
                program = Some(value(i)?.clone());
                i += 2;
            }
            "--policy-arg" => {
                policy_args.push(value(i)?.clone());
                i += 2;
            }
            "--name" => {
                name = Some(value(i)?.clone());
                i += 2;
            }
            "--seed" => {
                selection.base_seed = value(i)?
                    .parse()
                    .map_err(|_| "--seed must be an unsigned integer".to_string())?;
                i += 2;
            }
            "--max-turns" => {
                config.max_turns = value(i)?
                    .parse()
                    .map_err(|_| "--max-turns must be an integer".to_string())?;
                i += 2;
            }
            "--max-policy-errors" => {
                config.max_policy_errors = value(i)?
                    .parse()
                    .map_err(|_| "--max-policy-errors must be an integer".to_string())?;
                i += 2;
            }
            "--timeout-secs" => {
                timeout_secs = value(i)?
                    .parse()
                    .map_err(|_| "--timeout-secs must be an unsigned integer".to_string())?;
                i += 2;
            }
            "--split" => {
                selection.split = Some(value(i)?.clone());
                i += 2;
            }
            "--limit" => {
                selection.limit = Some(
                    value(i)?
                        .parse()
                        .map_err(|_| "--limit must be an integer".to_string())?,
                );
                i += 2;
            }
            "--repeats" => {
                selection.repeats = value(i)?
                    .parse()
                    .map_err(|_| "--repeats must be an integer".to_string())?;
                i += 2;
            }
            "--no-resume" => {
                resume = false;
                i += 1;
            }
            other => return Err(format!("unknown run-batch argument '{other}'")),
        }
    }
    config.validate()?;
    selection.validate()?;
    if timeout_secs == 0 {
        return Err("--timeout-secs must be at least 1".into());
    }
    let program = program.ok_or("run-batch requires --policy <program>")?;
    let policy_name = name.unwrap_or_else(|| program.clone());
    let tasks = read_tasks_jsonl(Path::new(tasks_path))?;
    std::fs::create_dir_all(episode_root)
        .map_err(|e| format!("cannot create {}: {e}", episode_root.display()))?;

    let empty = std::collections::HashSet::new();
    let all = plan_batch_runs(&tasks, &policy_name, &selection, &empty)?;
    let completed_keys = if resume {
        completed_batch_keys(episode_root)?
    } else {
        empty
    };
    let planned = plan_batch_runs(&tasks, &policy_name, &selection, &completed_keys)?;
    let skipped_completed = all.len() - planned.len();
    let mut completed = 0usize;
    let mut incomplete = 0usize;

    for spec in &planned {
        let task = tasks
            .iter()
            .find(|task| task.id == spec.task_id)
            .ok_or_else(|| format!("planned task '{}' disappeared", spec.task_id))?;
        let mut env = AtelierEnv::new(episode_root, spec.seed)?;
        let mut policy = CommandPolicy::new(
            policy_name.clone(),
            &program,
            policy_args.clone(),
            Duration::from_secs(timeout_secs),
        )?;
        let report = run_policy_episode(&mut env, task, &mut policy, &config)
            .map_err(|e| format!("task '{}' seed {}: {e}", task.id, spec.seed))?;
        let record = BatchRecord::from_report(episode_root, env.episode_dir(), &report)?;
        append_batch_record(episode_root, &record)?;
        if record.completed {
            completed += 1;
        } else {
            incomplete += 1;
        }
        println!(
            "{}",
            serde_json::to_string(&record).map_err(|e| e.to_string())?
        );
    }

    let summary = serde_json::json!({
        "format_version": 1,
        "kind": "batch_summary",
        "policy": policy_name,
        "selected": all.len(),
        "skipped_completed": skipped_completed,
        "attempted": planned.len(),
        "completed": completed,
        "incomplete": incomplete,
        "results": episode_root.join(BATCH_RESULTS_FILE),
    });
    println!(
        "{}",
        serde_json::to_string(&summary).map_err(|e| e.to_string())?
    );
    Ok(())
}
