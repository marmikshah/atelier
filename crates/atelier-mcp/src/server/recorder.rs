use serde_json::{json, Value};

// --- session recorder ------------------------------------------------------

/// Records every tool call into a replayable recipe file (the inverse of
/// `atelier replay`): a good live session becomes a recipe for free. The shape
/// it writes matches `replay::Recipe` — `{name, description, steps:[{tool,args}]}`.
///
/// After each call it rewrites the whole file atomically (write `.tmp`, rename)
/// so a killed session still leaves a valid recipe up to the last completed step.
#[derive(Clone)]
pub(crate) struct Recorder {
    path: std::sync::Arc<std::path::PathBuf>,
    /// Accumulated steps, guarded so concurrent HTTP sessions append in order.
    steps: std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
}

impl Recorder {
    pub(crate) fn new(path: std::path::PathBuf) -> Self {
        // Create the recipe's parent dir once so `--record nested/dir/recipe.json`
        // works instead of every atomic write silently failing.
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!(
                    "atelier: failed to create recording dir {}: {e}",
                    parent.display()
                );
            }
        }
        Self {
            path: std::sync::Arc::new(path),
            steps: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    /// Append one `{tool, args}` step and rewrite the recipe file atomically. Only
    /// the caller's successful steps reach here (see `call_tool`), so the recipe
    /// stays replayable. Best-effort: a write failure is logged, never fails the call.
    pub(crate) fn record(&self, tool: &str, args: Value) {
        let recipe = {
            // Serialize under the lock, write after dropping it — concurrent
            // HTTP tool calls should queue behind the Vec push, not disk I/O.
            let mut steps = self.steps.lock().expect("recorder lock poisoned");
            steps.push(json!({"tool": tool, "args": args}));
            let name = self
                .path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("session");
            json!({
                "name": name,
                "description": format!("recorded session {}", iso_date()),
                "steps": &*steps,
            })
        };
        // Pretty so the recipe stays hand-editable, like the shipped examples.
        let body = serde_json::to_string_pretty(&recipe).unwrap_or_else(|_| "{}".into());
        let tmp = self.path.with_extension("json.tmp");
        if let Err(e) = std::fs::write(&tmp, body).and_then(|()| std::fs::rename(&tmp, &*self.path))
        {
            eprintln!(
                "atelier: failed to write recording {}: {e}",
                self.path.display()
            );
        }
    }
}

/// UTC calendar date as `YYYY-MM-DD`, computed from the wall clock without a
/// date-library dependency (civil-from-days, proleptic Gregorian).
pub(crate) fn iso_date() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}
