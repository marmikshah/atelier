//! `atelier library` — inspect and prune the document store (`ATELIER_HOME`).
//!
//! Documents are the user's ART, not a cache: `rm` is destructive and confirms
//! before deleting unless `--yes` is given (or there is no terminal to ask on,
//! in which case it refuses rather than guessing).

use atelier_studio::Studio;

/// Entry point for `atelier library [rm ...]`. Returns a process exit code.
pub fn run(args: &[String]) -> i32 {
    match args.first().map(|s| s.as_str()) {
        None => list(),
        Some("rm") => rm(&args[1..]),
        Some(other) => {
            eprintln!("atelier library: unknown subcommand '{other}'\n\n{USAGE}");
            2
        }
    }
}

const USAGE: &str = "usage: atelier library [rm <id>... | rm --prefix <p> | rm --all] [--yes]

  (no args)          list every document: id, size, frames, layers
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
                .filter_map(|d| d.get("id").and_then(|i| i.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

fn list() -> i32 {
    let s = studio();
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
        .filter_map(|d| d.get("id").and_then(|i| i.as_str()))
        .map(|i| i.len())
        .max()
        .unwrap_or(0);
    let num = |d: &serde_json::Value, k: &str| d.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
    let mut rows: Vec<&serde_json::Value> = docs.iter().collect();
    rows.sort_by_key(|d| {
        d.get("id")
            .and_then(|i| i.as_str())
            .unwrap_or("")
            .to_string()
    });
    let s = studio();
    let mut replayable = 0usize;
    for d in &rows {
        let id = d.get("id").and_then(|i| i.as_str()).unwrap_or("?");
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
            "  {:width$}  {:>3}x{:<3}  {:>2} frames  {:>2} layers  {}",
            id,
            num(d, "w"),
            num(d, "h"),
            num(d, "frames"),
            num(d, "layers"),
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

fn flag(args: &[String], f: &str) -> bool {
    args.iter().any(|a| a == f)
}

/// Value of `--prefix`; an error when the flag is present without a usable value
/// (a bare `--prefix` must never be read as "match everything").
fn prefix_value(args: &[String]) -> Result<Option<String>, String> {
    match args.iter().position(|a| a == "--prefix") {
        None => Ok(None),
        Some(i) => match args.get(i + 1).filter(|a| !a.starts_with('-')) {
            Some(v) => Ok(Some(v.clone())),
            None => Err("--prefix needs a value".into()),
        },
    }
}

fn rm(args: &[String]) -> i32 {
    let yes = flag(args, "--yes") || flag(args, "-y");
    let all = flag(args, "--all");
    let prefix = match prefix_value(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("atelier library: {e}");
            return 2;
        }
    };
    let named: Vec<String> = args
        .iter()
        .filter(|a| !a.starts_with('-'))
        .cloned()
        .collect();
    // Skip the value that belongs to --prefix.
    let named: Vec<String> = match &prefix {
        Some(p) => named.into_iter().filter(|a| a != p).collect(),
        None => named,
    };

    if all as usize + prefix.is_some() as usize + !named.is_empty() as usize != 1 {
        eprintln!(
            "atelier library: give exactly one of <id>..., --prefix <p>, or --all\n\n{USAGE}"
        );
        return 2;
    }

    let s = studio();
    let targets: Vec<String> = if all {
        ids(&s)
    } else if let Some(p) = &prefix {
        ids(&s).into_iter().filter(|i| i.starts_with(p)).collect()
    } else {
        named
    };

    if targets.is_empty() {
        println!("nothing to delete");
        return 0;
    }

    if !yes {
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
    for id in &targets {
        match s.delete_doc(id) {
            Ok(_) => ok += 1,
            Err(e) => {
                eprintln!("  {id}: {e}");
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

    /// A bare `--prefix` must error, never fall through to "match everything" —
    /// that would silently select the whole library for deletion.
    #[test]
    fn bare_prefix_is_an_error_not_a_wildcard() {
        assert!(prefix_value(&v(&["rm", "--prefix"])).is_err());
        assert!(prefix_value(&v(&["rm", "--prefix", "--yes"])).is_err());
        assert_eq!(
            prefix_value(&v(&["rm", "--prefix", "hero-"])).unwrap(),
            Some("hero-".into())
        );
        assert_eq!(prefix_value(&v(&["rm", "a", "b"])).unwrap(), None);
    }

    #[test]
    fn flags_are_recognised_in_any_position() {
        assert!(flag(&v(&["rm", "--all", "--yes"]), "--yes"));
        assert!(flag(&v(&["rm", "-y", "--all"]), "-y"));
        assert!(!flag(&v(&["rm", "--all"]), "--yes"));
    }
}
