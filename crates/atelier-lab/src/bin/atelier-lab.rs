use std::collections::BTreeMap;
use std::path::Path;

use atelier_lab::{bundle_episode_comparisons, export_annotated_critic_jsonl, read_tasks_jsonl};

const USAGE: &str = "atelier-lab data tools

usage:
  atelier-lab validate-tasks <tasks.jsonl>
  atelier-lab bundle <pairs.jsonl> <output-dir>
  atelier-lab export-critic <comparisons.jsonl> <annotations.jsonl> <output.jsonl>";

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
        _ => Err("invalid arguments".into()),
    }
}
