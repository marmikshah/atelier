//! Tool reference, generated from the live tool registry so it can never drift
//! from the actual `#[tool]` descriptions. Emitted by the `atelier tools`
//! subcommand and written to `docs/tools.md` by `make docs`. No hand-maintained
//! tool list to keep in sync.

use super::Atelier;

/// Render the tool surface as a plain-text listing for the terminal:
/// `name — first sentence`, one per line.
pub fn tools_text() -> String {
    let mut tools = Atelier::registry_tools();
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    let width = tools.iter().map(|t| t.name.len()).max().unwrap_or(0);

    let mut list = String::new();
    for t in &tools {
        let summary = summarize(t.description.as_deref().unwrap_or(""));
        list.push_str(&format!(
            "  {:width$}  {}\n",
            t.name,
            summary,
            width = width
        ));
    }

    format!(
        "atelier tools — {} tools\n\n{}\n\
         Full reference: atelier tools --markdown  (or `make docs`)\n",
        tools.len(),
        list,
    )
}

/// Render the full tool surface as one Markdown document. Committed as
/// `docs/tools.md` so the reference is browsable in the repository itself.
pub fn tools_markdown() -> String {
    let mut tools = Atelier::registry_tools();
    tools.sort_by(|a, b| a.name.cmp(&b.name));

    let mut body = String::new();
    for t in &tools {
        body.push_str(&format!("## `{}`\n\n", t.name));
        body.push_str(t.description.as_deref().unwrap_or("").trim());
        body.push_str("\n\n");
        let params: Vec<String> = t
            .input_schema
            .get("properties")
            .and_then(|v| v.as_object())
            .map(|m| m.keys().map(|k| format!("`{k}`")).collect())
            .unwrap_or_default();
        if !params.is_empty() {
            body.push_str(&format!("Parameters: {}\n\n", params.join(", ")));
        }
    }

    let header = format!(
        "# atelier tool reference\n\n**{}** tools — every one advertised, no profiles to pick.\n\n",
        tools.len()
    );
    let note = "Generated from the live registry by `atelier tools --markdown`; regenerate with `make docs`. Do not edit by hand.\n\n";

    format!("{header}{note}{body}")
}

/// First clause of a tool description for the terminal listing: cut at the first
/// sentence/clause break (". ", "·", newline), then cap the length so the list
/// stays scannable. Avoids cutting on periods inside `[r,g,b]` or `#/`.
fn summarize(desc: &str) -> String {
    const CAP: usize = 96;
    let mut end = desc.len();
    for (i, _) in desc.char_indices() {
        let rest = &desc[i..];
        if rest.starts_with(". ") || rest.starts_with('·') || rest.starts_with('\n') {
            end = i;
            break;
        }
    }
    let clause = desc[..end].trim();
    if clause.chars().count() > CAP {
        let cut: String = clause.chars().take(CAP).collect();
        format!("{}…", cut.trim_end())
    } else {
        clause.to_string()
    }
}
