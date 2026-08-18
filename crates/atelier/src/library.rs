//! `atelier library` — inspect and prune the document store (`ATELIER_HOME`).
//!
//! Documents are the user's ART, not a cache: `rm` is destructive and confirms
//! before deleting unless `--yes` is given (or there is no terminal to ask on,
//! in which case it refuses rather than guessing).

use atelier_mcp::server::{self, Atelier};
use atelier_studio::{IntegritySeverity, Studio, ToolName};
use serde_json::json;

/// Entry point for `atelier library [rm ...]`. Returns a process exit code.
pub async fn run(args: &[String]) -> i32 {
    match args.first().map(|s| s.as_str()) {
        None => list(),
        Some("verify") => verify(&args[1..]),
        Some("rm") => rm(&args[1..]).await,
        Some("--help" | "-h" | "help") => {
            println!("{USAGE}");
            0
        }
        Some(other) => {
            eprintln!("atelier library: unknown subcommand '{other}'\n\n{USAGE}");
            2
        }
    }
}

const USAGE: &str =
    "usage: atelier library [verify [--json] | rm <id>... | rm --prefix <p> | rm --all] [--yes]

  (no args)          list every document: id, size, frames, layers
  verify             validate every document's metadata, cels, and journal
    --json           emit a machine-readable verification report
  rm <id>...         delete the named documents
  rm --prefix <p>    delete every document whose id starts with <p>
  rm --all           delete every document
  --yes, -y          skip the confirmation prompt

Documents are your artwork, not a cache — deleting them cannot be undone.";

fn studio() -> Studio {
    Studio::new()
}

/// The document ids the studio knows about, sorted.
fn ids(s: &Studio) -> Vec<String> {
    let v = s.list_docs();
    let mut out: Vec<String> = v
        .get("documents")
        .and_then(|d| d.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|d| d.get("doc_id").and_then(|i| i.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

fn list() -> i32 {
    let s = studio();
    let _lock = match s.lock_store_shared() {
        Ok(lock) => lock,
        Err(error) => {
            eprintln!("atelier library: {error}");
            return 1;
        }
    };
    let v = s.list_docs();
    let docs = v.get("documents").and_then(|d| d.as_array());
    let Some(docs) = docs else {
        println!("no documents");
        return 0;
    };
    if docs.is_empty() {
        println!("no documents in {}", home_display());
        return 0;
    }
    let width = docs
        .iter()
        .filter_map(|d| d.get("doc_id").and_then(|i| i.as_str()))
        .map(|i| i.len())
        .max()
        .unwrap_or(0);
    let num = |d: &serde_json::Value, k: &str| d.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
    let mut rows: Vec<&serde_json::Value> = docs.iter().collect();
    rows.sort_by_key(|d| {
        d.get("doc_id")
            .and_then(|i| i.as_str())
            .unwrap_or("")
            .to_string()
    });
    let s = studio();
    let mut replayable = 0usize;
    for d in &rows {
        let id = d.get("doc_id").and_then(|i| i.as_str()).unwrap_or("?");
        let name = d
            .get("name")
            .and_then(|value| value.as_str())
            .unwrap_or("?");
        if let Some(error) = d.get("error").and_then(|value| value.as_str()) {
            println!("  {:width$}  {error}", id, width = width);
            continue;
        }
        // The journal is the document's provenance — show it, so `replay <id>`
        // is discoverable from the listing rather than only from the docs.
        // A corrupt journal must not be listed as replayable steps — say so.
        let recipe = match s.journal(id) {
            Ok(j) if j.is_empty() => "  no recipe".to_string(),
            Ok(j) => {
                replayable += 1;
                format!("{:>4} steps", j.len())
            }
            Err(_) => "  corrupt journal".to_string(),
        };
        println!(
            "  {:width$}  {:>3}x{:<3}  {:>2} frames  {:>2} layers  {:<20}  {}",
            id,
            num(d, "w"),
            num(d, "h"),
            num(d, "frames"),
            num(d, "layers"),
            name,
            recipe,
            width = width
        );
    }
    println!("\n{} documents in {}", rows.len(), home_display());
    if replayable > 0 {
        println!("{replayable} replayable — atelier replay <id>");
    }
    0
}

fn home_display() -> String {
    std::env::var("ATELIER_HOME").unwrap_or_else(|_| "~/.atelier".into())
}

fn parse_verify(args: &[String]) -> Result<bool, String> {
    let mut json = false;
    for arg in args {
        match arg.as_str() {
            "--json" if !json => json = true,
            "--json" => return Err("--json may only be passed once".into()),
            option if option.starts_with('-') => {
                return Err(format!("unknown verify option '{option}'"));
            }
            value => return Err(format!("unexpected verify argument '{value}'")),
        }
    }
    Ok(json)
}

fn verify(args: &[String]) -> i32 {
    let json = match parse_verify(args) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("atelier library: {error}\n\n{USAGE}");
            return 2;
        }
    };
    let report = match studio().verify_store() {
        Ok(report) => report,
        Err(error) => {
            eprintln!("atelier library verify: {error}");
            return 1;
        }
    };
    if json {
        let output = match serde_json::to_string_pretty(&report) {
            Ok(output) => output,
            Err(error) => {
                eprintln!("atelier library verify: cannot serialize report: {error}");
                return 1;
            }
        };
        println!("{output}");
    } else {
        println!("document store: {}", report.documents_dir);
        for issue in &report.issues {
            let severity = match issue.severity {
                IntegritySeverity::Error => "error",
                IntegritySeverity::Warning => "warning",
            };
            let target = issue
                .document_id
                .as_deref()
                .map(|id| format!("{id}/{}", issue.component))
                .unwrap_or_else(|| issue.component.clone());
            println!("{severity}: {target}: {}", issue.message);
            println!("  action: {}", issue.action);
        }
        if report.issues_truncated {
            println!(
                "note: {} additional finding(s) omitted; summary totals include them",
                report.omitted_issues
            );
        }
        println!(
            "checked {} document(s), {} cel(s), and {} journal entr{}",
            report.documents,
            report.cels,
            report.journal_entries,
            if report.journal_entries == 1 {
                "y"
            } else {
                "ies"
            }
        );
        if report.ok {
            println!("verification passed with {} warning(s)", report.warnings);
        } else {
            println!(
                "verification failed with {} error(s) and {} warning(s)",
                report.errors, report.warnings
            );
        }
    }
    i32::from(!report.ok)
}

#[derive(Debug, PartialEq, Eq)]
enum Selection {
    All,
    Prefix(String),
    Named(Vec<String>),
}

#[derive(Debug, PartialEq, Eq)]
struct RmOptions {
    yes: bool,
    selection: Selection,
}

/// Parse deletion arguments without silently accepting options. This command
/// destroys artwork, so every token must have one unambiguous meaning.
fn parse_rm(args: &[String]) -> Result<RmOptions, String> {
    let mut yes = false;
    let mut all = false;
    let mut prefix = None;
    let mut named = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--yes" | "-y" => yes = true,
            "--all" => {
                if all {
                    return Err("--all may only be passed once".into());
                }
                all = true;
            }
            "--prefix" => {
                if prefix.is_some() {
                    return Err("--prefix may only be passed once".into());
                }
                i += 1;
                let value = args
                    .get(i)
                    .filter(|value| !value.starts_with('-'))
                    .ok_or("--prefix needs a value")?;
                prefix = Some(value.clone());
            }
            option if option.starts_with('-') => {
                return Err(format!("unknown option '{option}'"));
            }
            id => named.push(id.to_string()),
        }
        i += 1;
    }

    if all as usize + prefix.is_some() as usize + !named.is_empty() as usize != 1 {
        return Err("give exactly one of <id>..., --prefix <p>, or --all".into());
    }
    let selection = if all {
        Selection::All
    } else if let Some(prefix) = prefix {
        Selection::Prefix(prefix)
    } else {
        Selection::Named(named)
    };
    Ok(RmOptions { yes, selection })
}

async fn rm(args: &[String]) -> i32 {
    let options = match parse_rm(args) {
        Ok(options) => options,
        Err(e) => {
            eprintln!("atelier library: {e}\n\n{USAGE}");
            return 2;
        }
    };

    let s = studio();
    let targets: Vec<String> = match &options.selection {
        Selection::All => ids(&s),
        Selection::Prefix(prefix) => ids(&s)
            .into_iter()
            .filter(|id| id.starts_with(prefix))
            .collect(),
        Selection::Named(ids) => ids.clone(),
    };

    if targets.is_empty() {
        println!("nothing to delete");
        return 0;
    }

    if !options.yes {
        eprintln!("About to permanently delete {} document(s):", targets.len());
        for t in targets.iter().take(10) {
            eprintln!("  {t}");
        }
        if targets.len() > 10 {
            eprintln!("  … and {} more", targets.len() - 10);
        }
        if !confirm() {
            eprintln!("aborted.");
            return 1;
        }
    }

    let (mut ok, mut failed) = (0, 0);
    let atelier = Atelier::with_studio(s);
    for id in &targets {
        match atelier
            .dispatch(ToolName::DeleteDoc, json!({"doc_id": id}), "cli")
            .await
        {
            Ok(result) if !server::is_error_result(&result) => ok += 1,
            Ok(result) => {
                let error = server::result_json(&result)
                    .and_then(|value| {
                        value
                            .get("error")
                            .and_then(|e| e.as_str())
                            .map(str::to_string)
                    })
                    .unwrap_or_else(|| "delete failed".into());
                eprintln!("  {id}: {error}");
                failed += 1;
            }
            Err(error) => {
                eprintln!("  {id}: {error}");
                failed += 1;
            }
        }
    }
    println!(
        "deleted {ok} document(s){}",
        if failed > 0 {
            format!(", {failed} failed")
        } else {
            String::new()
        }
    );
    i32::from(failed > 0)
}

/// Ask on the terminal. No terminal (piped/CI) = refuse rather than assume yes:
/// a destructive default is how someone loses their artwork.
fn confirm() -> bool {
    use std::io::{BufRead, IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        eprintln!("not a terminal — re-run with --yes to confirm");
        return false;
    }
    eprint!("Type 'yes' to confirm: ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() {
        return false;
    }
    line.trim().eq_ignore_ascii_case("yes")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn deletion_arguments_are_explicit_and_reject_unknown_options() {
        assert!(parse_rm(&v(&["--prefix"])).is_err());
        assert!(parse_rm(&v(&["--prefix", "--yes"])).is_err());
        assert_eq!(
            parse_rm(&v(&["--prefix", "hero-", "--yes"])).unwrap(),
            RmOptions {
                yes: true,
                selection: Selection::Prefix("hero-".into())
            }
        );
        assert_eq!(
            parse_rm(&v(&["a", "--yes", "b"])).unwrap(),
            RmOptions {
                yes: true,
                selection: Selection::Named(v(&["a", "b"]))
            }
        );
        assert!(parse_rm(&v(&["--all", "--force"])).is_err());
        assert!(parse_rm(&v(&["--all", "a"])).is_err());
        assert!(parse_rm(&v(&["--prefix", "a", "b"])).is_err());
    }

    #[test]
    fn verification_arguments_are_strict() {
        assert!(!parse_verify(&v(&[])).unwrap());
        assert!(parse_verify(&v(&["--json"])).unwrap());
        assert!(parse_verify(&v(&["--json", "--json"])).is_err());
        assert!(parse_verify(&v(&["--pretty"])).is_err());
        assert!(parse_verify(&v(&["document-id"])).is_err());
    }
}
