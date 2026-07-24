use serde_json::{Value, json};

// --- session recorder ------------------------------------------------------

/// Records a session's tool calls into a replayable recipe file (the inverse of
/// `atelier replay`): a good live sitting becomes a recipe for free.
///
/// This is the CROSS-DOCUMENT capture, opt-in via `--record`. Each document also
/// journals itself unconditionally (see `Studio::journal_append`); the recorder
/// earns its keep when a session spans several documents and you want the whole
/// sitting in one file.
///
/// Writes JSON Lines, appended: one call per line, O(1) per call instead of
/// rewriting the whole recipe every time, and a killed session still leaves every
/// completed line intact.
#[derive(Clone)]
pub(crate) struct Recorder {
    path: std::sync::Arc<std::path::PathBuf>,
    /// Serialises appends so concurrent HTTP sessions cannot interleave a line.
    write: std::sync::Arc<std::sync::Mutex<()>>,
}

impl Recorder {
    pub(crate) fn new(path: std::path::PathBuf) -> Self {
        // Create the recipe's parent dir once so `--record nested/dir/recipe.jsonl`
        // works instead of every write silently failing.
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty())
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            eprintln!(
                "atelier: failed to create recording dir {}: {e}",
                parent.display()
            );
        }
        // `--record` names THIS session's output: truncate whatever was there,
        // or reusing a filename would append a second sitting after the first
        // and the concatenation replays as one corrupted recipe.
        if let Err(e) = std::fs::File::create(&path) {
            eprintln!("atelier: failed to start recording {}: {e}", path.display());
        }
        Self {
            path: std::sync::Arc::new(path),
            write: std::sync::Arc::new(std::sync::Mutex::new(())),
        }
    }

    /// Append one `{tool, args}` call. Only successful, non-read calls reach
    /// here (see `call_tool`), so the recipe stays replayable and carries no
    /// `doc_look` noise. Best-effort: a write failure is logged, never fails the
    /// call that was otherwise fine.
    pub(crate) fn record(&self, tool: &str, args: Value) {
        let Ok(mut line) = serde_json::to_string(&json!({"tool": tool, "args": args})) else {
            return;
        };
        line.push('\n');
        let _guard = self
            .write
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let appended = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&*self.path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
        if let Err(e) = appended {
            eprintln!(
                "atelier: failed to write recording {}: {e}",
                self.path.display()
            );
        }
    }
}
