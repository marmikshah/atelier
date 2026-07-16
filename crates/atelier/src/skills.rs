//! The atelier drawing skills as data.
//!
//! A skill is a document, so its prose lives as markdown in `skills/*.md`. Rust
//! owns the *metadata* (name, description) and the *renderers* that wrap that
//! prose for each consumer — a Claude Code `SKILL.md`, or the `atelier agent`
//! system prompt. One source, compile-checked, and adding a target (Cursor, …)
//! is one method, not another copy of the text.

/// One skill: typed metadata plus a pure-prose markdown body (no frontmatter).
pub struct Skill {
    /// The Claude Code skill name, e.g. `atelier-sprite`.
    pub name: &'static str,
    /// The short selector used on the command line, e.g. `sprite`.
    pub short: &'static str,
    /// One-line summary — the Claude frontmatter `description`.
    pub description: &'static str,
    /// The markdown body, authored in `skills/<short>.md`.
    pub body: &'static str,
}

pub const SPRITE: Skill = Skill {
    name: "atelier-sprite",
    short: "sprite",
    description: "Draw a single pixel-art subject — a character, creature, vehicle, prop, item or effect — as a layered, optionally animated atelier document. Use when asked to make a sprite, icon, or any one discrete object, still or animated. Builds it in parts on separate layers, looks at every pass, and fixes the specific thing that is wrong rather than redrawing. Requires the atelier MCP server. For backgrounds and full scenes use atelier-scene; to judge finished art use atelier-review.",
    body: include_str!("../skills/sprite.md"),
};

pub const SCENE: Skill = Skill {
    name: "atelier-scene",
    short: "scene",
    description: "Draw a whole pixel-art picture — a background, environment, interior, landscape or composed scene — as a layered atelier document. Use when the subject is a place rather than an object, or when several elements must read together as one image. Builds it in depth bands on separate layers, looks at every pass, and fixes one band at a time rather than repainting the frame. Requires the atelier MCP server. For a single object use atelier-sprite; to judge finished art use atelier-review.",
    body: include_str!("../skills/scene.md"),
};

pub const REVIEW: Skill = Skill {
    name: "atelier-review",
    short: "review",
    description: "Review finished or in-progress pixel art in an atelier document and report what is wrong with it. Use to judge a sprite, scene, animation or document set — an art-director pass that measures rather than guesses, and names a localised fix for every finding. Read-only by default: it reports, it does not repaint. Requires the atelier MCP server. To make the art in the first place use atelier-sprite or atelier-scene.",
    body: include_str!("../skills/review.md"),
};

/// Every shipped skill.
pub const ALL: &[&Skill] = &[&SPRITE, &SCENE, &REVIEW];

/// The ground rules the `agent` loop needs on top of a skill: drive the tools,
/// look, and export at the end. Skill-agnostic, so it lives with the renderer,
/// not in the prose.
#[cfg(feature = "agent")]
const AGENT_GROUND_RULES: &str = "You are drawing through the atelier tools. Call them to \
    build the art. After a burst of edits, call doc_look to SEE the frame before continuing — \
    it returns the image. When the piece is finished, call the export tool, then reply with a \
    one-line summary and stop. Do not ask the user questions; you have everything you need.";

impl Skill {
    /// Resolve a short selector (`sprite`/`scene`/`review`) to a skill.
    pub fn by_short(short: &str) -> Option<&'static Skill> {
        ALL.iter().copied().find(|s| s.short == short)
    }

    /// A Claude Code `SKILL.md`: YAML frontmatter over the prose body. This is
    /// the file `~/.claude/skills/<name>/SKILL.md` the loader reads.
    pub fn claude_skill_md(&self) -> String {
        format!(
            "---\nname: {}\ndescription: {}\n---\n\n{}\n",
            self.name,
            self.description,
            self.body.trim()
        )
    }

    /// The `atelier agent` system prompt: the prose plus the drive-the-tools
    /// ground rules. `out` names the file the model must export to, if any.
    #[cfg(feature = "agent")]
    pub fn agent_prompt(&self, out: Option<&str>) -> String {
        agent_prompt(self.body, out)
    }
}

/// Render an agent system prompt from any prose `body` — used for a built-in
/// skill or a caller's `--skill-file`, so both take the same shape.
#[cfg(feature = "agent")]
pub fn agent_prompt(body: &str, out: Option<&str>) -> String {
    let mut s = format!("{}\n\n{}", body.trim(), AGENT_GROUND_RULES);
    if let Some(path) = out {
        s.push_str(&format!(
            " Export the finished piece to exactly this path: {path}"
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bodies_are_pure_prose_no_frontmatter() {
        for sk in ALL {
            assert!(
                !sk.body.trim_start().starts_with("---"),
                "{} body still has frontmatter — metadata belongs in Rust",
                sk.name
            );
            assert!(sk.body.contains("#"), "{} body looks empty", sk.name);
        }
    }

    #[test]
    fn claude_skill_md_round_trips_name_and_body() {
        let md = SPRITE.claude_skill_md();
        assert!(md.starts_with("---\nname: atelier-sprite\n"));
        assert!(md.contains("description: Draw a single"));
        assert!(md.contains("# Drawing one subject"), "prose body missing");
    }

    #[cfg(feature = "agent")]
    #[test]
    fn agent_prompt_has_prose_rules_and_out_path() {
        let p = SPRITE.agent_prompt(Some("/tmp/x.gif"));
        assert!(p.contains("# Drawing one subject"), "prose missing");
        assert!(p.contains("call doc_look"), "ground rules missing");
        assert!(p.contains("/tmp/x.gif"), "out path missing");
        assert!(!p.starts_with("---"), "frontmatter leaked into the prompt");
        assert!(!SPRITE
            .agent_prompt(None)
            .contains("Export the finished piece"));
    }

    #[test]
    fn short_selectors_resolve() {
        assert_eq!(Skill::by_short("scene").unwrap().name, "atelier-scene");
        assert!(Skill::by_short("nope").is_none());
        assert_eq!(ALL.len(), 3);
    }
}
