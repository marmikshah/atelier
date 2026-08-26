//! `atelier library` — inspect, archive, and prune the document store.
//!
//! Documents are the user's ART, not a cache: `rm` is destructive and confirms
//! before deleting unless `--yes` is given (or there is no terminal to ask on,
//! in which case it refuses rather than guessing).

use atelier_mcp::server::{self, Atelier};
use atelier_studio::{IntegritySeverity, Studio, ToolName};
use serde_json::json;

use std::path::{Path, PathBuf};

/// Entry point for `atelier library <command>`. Returns a process exit code.
pub async fn run(args: &[String]) -> i32 {
    match args.first().map(|s| s.as_str()) {
        None => list(&[]),
        Some("verify") => verify(&args[1..]),
        Some("pack") => pack(&args[1..]),
        Some("unpack") => unpack(&args[1..]),
        Some("rm") => rm(&args[1..]).await,
        Some("--help" | "-h" | "help") => {
            println!("{USAGE}");
            0
        }
        // `atelier library --home DIR` lists that store; no subcommand needed.
        Some(option) if option.starts_with('-') => list(args),
        Some(other) => {
            eprintln!("atelier library: unknown subcommand '{other}'\n\n{USAGE}");
            2
        }
    }
}

const USAGE: &str = "usage:
  atelier library [--home DIR]
  atelier library verify [--json] [--home DIR]
  atelier library pack <doc-id> --out <file.atelierpack> [--home DIR]
  atelier library unpack <file.atelierpack> [--home DIR] [--replace --yes]
  atelier library rm <id>... [--yes] [--home DIR]
  atelier library rm --prefix <p> [--yes] [--home DIR]
  atelier library rm --all [--yes] [--home DIR]

  (no args)          list every document: id, size, frames, layers
  verify             validate every document's metadata, cels, and journal
    --json           emit a machine-readable verification report
  pack               write a portable archive without overwriting an existing file
    --out FILE       required archive destination
  unpack             restore an archive while preserving its document UUID
    --replace --yes  replace an existing UUID; both flags are required together
  --home DIR         use an isolated Atelier home; overrides ATELIER_HOME
  rm <id>...         delete the named documents
  rm --prefix <p>    delete every document whose id starts with <p>
  rm --all           delete every document
  --yes, -y          skip the confirmation prompt

Documents are your artwork, not a cache — deleting them cannot be undone.";

/// The store every `library` subcommand acts on: `--home` when given,
/// otherwise the ambient resolution (`ATELIER_HOME`, `./.atelier`, `~/.atelier`).
fn studio_at(home: Option<&Path>) -> Studio {
    home.map(|path| Studio::with_home(path.to_path_buf()))
        .unwrap_or_else(Studio::new)
}

/// Consume one `--home DIR` at `args[*index]`, rejecting a repeat. Shared by
/// every `library` subcommand so the flag means the same thing throughout.
fn take_home(args: &[String], index: &mut usize, home: &mut Option<PathBuf>) -> Result<(), String> {
    if home.is_some() {
        return Err("--home may only be passed once".into());
    }
    *home = Some(PathBuf::from(option_value(args, index, "--home")?));
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
struct PackOptions {
    doc_id: String,
    output: PathBuf,
    home: Option<PathBuf>,
}

fn option_value(args: &[String], index: &mut usize, option: &str) -> Result<String, String> {
    *index += 1;
    args.get(*index)
        .filter(|value| !value.starts_with('-'))
        .cloned()
        .ok_or_else(|| format!("{option} needs a value"))
}

fn parse_pack(args: &[String]) -> Result<PackOptions, String> {
    let mut doc_id = None;
    let mut output = None;
    let mut home = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--out" => {
                if output.is_some() {
                    return Err("--out may only be passed once".into());
                }
                output = Some(PathBuf::from(option_value(args, &mut index, "--out")?));
            }
            "--home" => {
                if home.is_some() {
                    return Err("--home may only be passed once".into());
                }
                home = Some(PathBuf::from(option_value(args, &mut index, "--home")?));
            }
            option if option.starts_with('-') => {
                return Err(format!("unknown pack option '{option}'"));
            }
            value => {
                if doc_id.is_some() {
                    return Err(format!("unexpected pack argument '{value}'"));
                }
                doc_id = Some(value.to_string());
            }
        }
        index += 1;
    }

    Ok(PackOptions {
        doc_id: doc_id.ok_or("pack needs one <doc-id>")?,
        output: output.ok_or("pack needs --out <file.atelierpack>")?,
        home,
    })
}

fn print_report(command: &str, report: &serde_json::Value) -> i32 {
    match serde_json::to_string_pretty(report) {
        Ok(output) => {
            println!("{output}");
            0
        }
        Err(error) => {
            eprintln!("atelier library {command}: cannot serialize report: {error}");
            1
        }
    }
}

fn pack(args: &[String]) -> i32 {
    let options = match parse_pack(args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("atelier library: {error}\n\n{USAGE}");
            return 2;
        }
    };
    match studio_at(options.home.as_deref()).pack_document(&options.doc_id, &options.output) {
        Ok(report) => print_report("pack", &report),
        Err(error) => {
            eprintln!("atelier library pack: {error}");
            1
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct UnpackOptions {
    input: PathBuf,
    home: Option<PathBuf>,
    replace: bool,
}

fn parse_unpack(args: &[String]) -> Result<UnpackOptions, String> {
    let mut input = None;
    let mut home = None;
    let mut replace = false;
    let mut yes = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--home" => {
                if home.is_some() {
                    return Err("--home may only be passed once".into());
                }
                home = Some(PathBuf::from(option_value(args, &mut index, "--home")?));
            }
            "--replace" if !replace => replace = true,
            "--replace" => return Err("--replace may only be passed once".into()),
            "--yes" if !yes => yes = true,
            "--yes" => return Err("--yes may only be passed once".into()),
            option if option.starts_with('-') => {
                return Err(format!("unknown unpack option '{option}'"));
            }
            value => {
                if input.is_some() {
                    return Err(format!("unexpected unpack argument '{value}'"));
                }
                input = Some(PathBuf::from(value));
            }
        }
        index += 1;
    }
    if replace != yes {
        return Err("--replace and --yes must be passed together".into());
    }

    Ok(UnpackOptions {
        input: input.ok_or("unpack needs one <file.atelierpack>")?,
        home,
        replace,
    })
}

fn unpack(args: &[String]) -> i32 {
    let options = match parse_unpack(args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("atelier library: {error}\n\n{USAGE}");
            return 2;
        }
    };
    match studio_at(options.home.as_deref()).unpack_document(&options.input, options.replace) {
        Ok(report) => print_report("unpack", &report),
        Err(error) => {
            eprintln!("atelier library unpack: {error}");
            1
        }
    }
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

fn parse_list(args: &[String]) -> Result<Option<PathBuf>, String> {
    let mut home = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--home" => take_home(args, &mut index, &mut home)?,
            option if option.starts_with('-') => {
                return Err(format!("unknown option '{option}'"));
            }
            value => return Err(format!("unexpected argument '{value}'")),
        }
        index += 1;
    }
    Ok(home)
}

fn list(args: &[String]) -> i32 {
    let home = match parse_list(args) {
        Ok(home) => home,
        Err(error) => {
            eprintln!("atelier library: {error}\n\n{USAGE}");
            return 2;
        }
    };
    let s = studio_at(home.as_deref());
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
        println!("no documents in {}", home_display(home.as_deref()));
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
    println!(
        "\n{} documents in {}",
        rows.len(),
        home_display(home.as_deref())
    );
    if replayable > 0 {
        println!("{replayable} replayable — atelier replay <id>");
    }
    0
}

/// The store path to name in listing output: the `--home` the caller gave,
/// then `ATELIER_HOME`, then the default. Must agree with `studio_at`.
fn home_display(home: Option<&Path>) -> String {
    match home {
        Some(path) => path.display().to_string(),
        None => std::env::var("ATELIER_HOME").unwrap_or_else(|_| "~/.atelier".into()),
    }
}

#[derive(Debug, PartialEq, Eq)]
struct VerifyOptions {
    json: bool,
    home: Option<PathBuf>,
}

fn parse_verify(args: &[String]) -> Result<VerifyOptions, String> {
    let mut json = false;
    let mut home = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--json" if !json => json = true,
            "--json" => return Err("--json may only be passed once".into()),
            "--home" => take_home(args, &mut index, &mut home)?,
            option if option.starts_with('-') => {
                return Err(format!("unknown verify option '{option}'"));
            }
            value => return Err(format!("unexpected verify argument '{value}'")),
        }
        index += 1;
    }
    Ok(VerifyOptions { json, home })
}

fn verify(args: &[String]) -> i32 {
    let options = match parse_verify(args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("atelier library: {error}\n\n{USAGE}");
            return 2;
        }
    };
    let json = options.json;
    let report = match studio_at(options.home.as_deref()).verify_store() {
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
    home: Option<PathBuf>,
}

/// Parse deletion arguments without silently accepting options. This command
/// destroys artwork, so every token must have one unambiguous meaning.
fn parse_rm(args: &[String]) -> Result<RmOptions, String> {
    let mut yes = false;
    let mut all = false;
    let mut prefix = None;
    let mut home = None;
    let mut named = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--yes" | "-y" => yes = true,
            "--home" => take_home(args, &mut i, &mut home)?,
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
    Ok(RmOptions {
        yes,
        selection,
        home,
    })
}

async fn rm(args: &[String]) -> i32 {
    let options = match parse_rm(args) {
        Ok(options) => options,
        Err(e) => {
            eprintln!("atelier library: {e}\n\n{USAGE}");
            return 2;
        }
    };

    let s = studio_at(options.home.as_deref());
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
                selection: Selection::Prefix("hero-".into()),
                home: None
            }
        );
        assert_eq!(
            parse_rm(&v(&["a", "--yes", "b"])).unwrap(),
            RmOptions {
                yes: true,
                selection: Selection::Named(v(&["a", "b"])),
                home: None
            }
        );
        assert_eq!(
            parse_rm(&v(&["--all", "--yes", "--home", "/tmp/store"])).unwrap(),
            RmOptions {
                yes: true,
                selection: Selection::All,
                home: Some(PathBuf::from("/tmp/store"))
            }
        );
        assert!(parse_rm(&v(&["--all", "--force"])).is_err());
        assert!(parse_rm(&v(&["--all", "a"])).is_err());
        assert!(parse_rm(&v(&["--prefix", "a", "b"])).is_err());
        assert!(parse_rm(&v(&["--all", "--home"])).is_err());
        assert!(
            parse_rm(&v(&["--all", "--home", "a", "--home", "b"])).is_err(),
            "--home may only be passed once"
        );
    }

    #[test]
    fn verification_arguments_are_strict() {
        assert_eq!(
            parse_verify(&v(&[])).unwrap(),
            VerifyOptions {
                json: false,
                home: None
            }
        );
        assert_eq!(
            parse_verify(&v(&["--json", "--home", "/tmp/store"])).unwrap(),
            VerifyOptions {
                json: true,
                home: Some(PathBuf::from("/tmp/store"))
            }
        );
        assert!(parse_verify(&v(&["--json", "--json"])).is_err());
        assert!(parse_verify(&v(&["--pretty"])).is_err());
        assert!(parse_verify(&v(&["document-id"])).is_err());
        assert!(parse_verify(&v(&["--home"])).is_err());
    }

    #[test]
    fn listing_accepts_only_an_optional_home() {
        assert_eq!(parse_list(&v(&[])).unwrap(), None);
        assert_eq!(
            parse_list(&v(&["--home", "/tmp/store"])).unwrap(),
            Some(PathBuf::from("/tmp/store"))
        );
        assert!(parse_list(&v(&["--home"])).is_err());
        assert!(parse_list(&v(&["--json"])).is_err());
        assert!(parse_list(&v(&["document-id"])).is_err());
    }

    #[test]
    fn pack_arguments_require_one_id_output_and_optional_home() {
        assert_eq!(
            parse_pack(&v(&[
                "123e4567-e89b-42d3-a456-426614174000",
                "--out",
                "art.atelierpack",
                "--home",
                "/tmp/atelier-test",
            ]))
            .unwrap(),
            PackOptions {
                doc_id: "123e4567-e89b-42d3-a456-426614174000".into(),
                output: PathBuf::from("art.atelierpack"),
                home: Some(PathBuf::from("/tmp/atelier-test")),
            }
        );
        assert!(parse_pack(&v(&[])).is_err());
        assert!(parse_pack(&v(&["id", "--out"])).is_err());
        assert!(parse_pack(&v(&["id", "--out", "one", "--out", "two"])).is_err());
        assert!(parse_pack(&v(&["id", "other", "--out", "one"])).is_err());
        assert!(parse_pack(&v(&["id", "--out", "one", "--unknown"])).is_err());
        assert!(parse_pack(&v(&["id", "--out", "one", "--home"])).is_err());
    }

    #[test]
    fn unpack_arguments_require_paired_replacement_confirmation() {
        assert_eq!(
            parse_unpack(&v(&[
                "art.atelierpack",
                "--yes",
                "--home",
                "/tmp/atelier-test",
                "--replace",
            ]))
            .unwrap(),
            UnpackOptions {
                input: PathBuf::from("art.atelierpack"),
                home: Some(PathBuf::from("/tmp/atelier-test")),
                replace: true,
            }
        );
        assert!(parse_unpack(&v(&[])).is_err());
        assert!(parse_unpack(&v(&["art.atelierpack", "--replace"])).is_err());
        assert!(parse_unpack(&v(&["art.atelierpack", "--yes"])).is_err());
        assert!(
            parse_unpack(&v(&["art.atelierpack", "--replace", "--replace", "--yes",])).is_err()
        );
        assert!(parse_unpack(&v(&["art.atelierpack", "--replace", "--yes", "--yes",])).is_err());
        assert!(parse_unpack(&v(&["one", "two"])).is_err());
        assert!(parse_unpack(&v(&["art.atelierpack", "--force"])).is_err());
    }
}
