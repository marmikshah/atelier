use serde_json::Value;

use crate::recipe::{CompactEncoder, Step};

// --- session recorder ------------------------------------------------------

/// Records a session's tool calls into a replayable recipe file (the inverse of
/// `atelier replay`): a good live sitting becomes a recipe for free.
///
/// This is the CROSS-DOCUMENT capture, opt-in via `--record`. Each document also
/// journals itself unconditionally (see `Studio::journal_append`); the recorder
/// earns its keep when a session spans several documents and you want the whole
/// sitting in one file.
///
/// Writes compact JSONL v2, appended: one call per line after the versioned
/// header, O(1) per call instead of rewriting the whole recipe every time, and
/// a killed session still leaves every completed line intact.
#[derive(Clone)]
pub(crate) struct Recorder {
    path: std::sync::Arc<std::path::PathBuf>,
    /// A failed header write disables this recorder permanently. Recreating the
    /// path later and appending a call without its v2 header would produce a
    /// corrupt recipe.
    active: bool,
    /// Serialises appends so concurrent HTTP sessions cannot interleave a line,
    /// and carries the context that makes subsequent lines smaller.
    encoder: std::sync::Arc<std::sync::Mutex<CompactEncoder>>,
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
        let encoder = CompactEncoder::recording();
        // `--record` names THIS session's output: truncate whatever was there,
        // then write the v2 header. Reusing a filename must never append a
        // second sitting after the first.
        let started = encoder.recording_header().and_then(|mut header| {
            use std::io::Write;
            header.push('\n');
            let mut file = std::fs::File::create(&path)
                .map_err(|error| format!("cannot create {}: {error}", path.display()))?;
            file.write_all(header.as_bytes())
                .map_err(|error| format!("cannot write {}: {error}", path.display()))
        });
        let active = match started {
            Ok(()) => true,
            Err(error) => {
                eprintln!("atelier: failed to start recording: {error}");
                false
            }
        };
        Self {
            path: std::sync::Arc::new(path),
            active,
            encoder: std::sync::Arc::new(std::sync::Mutex::new(encoder)),
        }
    }

    /// Append one compact call line. Only successful, non-read calls reach here,
    /// so the recipe stays replayable and carries no `doc_look` noise.
    /// Best-effort: a write failure is logged, never fails the call that was
    /// otherwise fine.
    pub(crate) fn record(&self, tool: &str, args: Value) {
        if !self.active {
            return;
        }
        let mut encoder = self
            .encoder
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Advance the sticky context only after the corresponding line lands
        // on disk; otherwise one failed append could make the next line depend
        // on context the file never received.
        let mut next = encoder.clone();
        let mut line = match next.encode(&Step {
            tool: tool.to_string(),
            args,
            note: None,
        }) {
            Ok(line) => line,
            Err(error) => {
                eprintln!("atelier: failed to encode recording step {tool}: {error}");
                return;
            }
        };
        line.push('\n');
        let appended = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&*self.path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
        match appended {
            Ok(()) => *encoder = next,
            Err(error) => {
                eprintln!(
                    "atelier: failed to write recording {}: {error}",
                    self.path.display()
                );
            }
        }
    }
}
