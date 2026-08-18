//! The atelier drawing skills as data.
//!
//! A skill is a document, so its prose lives as markdown in `skills/*.md`. Rust
//! owns the *metadata* (name, description) and the *renderers* that wrap that
//! prose for each consumer — the standard `SKILL.md` Claude Code, Codex, Kimi
//! Code and Cursor all load. One source, compile-checked, and adding a consumer
//! is one method or one install dir, not another copy of the text.

/// One skill: typed metadata plus a pure-prose markdown body (no frontmatter).
pub struct Skill {
    /// The agent skill name, e.g. `atelier-sprite`.
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
    description: "Draw a single pixel-art subject — a character, creature, vehicle, prop, item or effect — as a layered, optionally animated atelier document. Use when asked to make a sprite, icon, or any one discrete object, still or animated. Builds it in parts on separate layers, looks at every pass, and fixes the specific thing that is wrong rather than redrawing. Driven through the atelier CLI (`atelier call`) or over MCP. For backgrounds and full scenes use atelier-scene; to judge finished art use atelier-review.",
    body: include_str!("../skills/sprite.md"),
};

pub const SCENE: Skill = Skill {
    name: "atelier-scene",
    short: "scene",
    description: "Draw a whole pixel-art picture — a background, environment, interior, landscape or composed scene — as a layered atelier document. Use when the subject is a place rather than an object, or when several elements must read together as one image. Builds it in depth bands on separate layers, looks at every pass, and fixes one band at a time rather than repainting the frame. Driven through the atelier CLI (`atelier call`) or over MCP. For a single object use atelier-sprite; to judge finished art use atelier-review.",
    body: include_str!("../skills/scene.md"),
};

pub const REVIEW: Skill = Skill {
    name: "atelier-review",
    short: "review",
    description: "Review finished or in-progress pixel art in an atelier document and report what is wrong with it. Use to judge a sprite, scene, animation or document set — an art-director pass that measures rather than guesses, and names a localised fix for every finding. Read-only by default: it reports, it does not repaint. Driven through the atelier CLI (`atelier call`) or over MCP. To make the art in the first place use atelier-sprite or atelier-scene.",
    body: include_str!("../skills/review.md"),
};

/// Every shipped skill.
pub const ALL: &[&Skill] = &[&SPRITE, &SCENE, &REVIEW];

impl Skill {
    /// Resolve a short selector (`sprite`/`scene`/`review`) to a skill.
    pub fn by_short(short: &str) -> Option<&'static Skill> {
        ALL.iter().copied().find(|s| s.short == short)
    }

    /// The standard `SKILL.md`: YAML frontmatter over the prose body — the one
    /// file Claude Code, Codex, Kimi Code and Cursor each load from their skills dir.
    pub fn skill_md(&self) -> String {
        format!(
            "---\nname: {}\ndescription: {}\n---\n\n{}\n",
            self.name,
            self.description,
            self.body.trim()
        )
    }
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
    fn skill_md_round_trips_name_and_body() {
        let md = SPRITE.skill_md();
        assert!(md.starts_with("---\nname: atelier-sprite\n"));
        assert!(md.contains("description: Draw a single"));
        assert!(md.contains("# Drawing one subject"), "prose body missing");
    }

    #[test]
    fn short_selectors_resolve() {
        assert_eq!(Skill::by_short("scene").unwrap().name, "atelier-scene");
        assert!(Skill::by_short("nope").is_none());
        assert_eq!(ALL.len(), 3);
    }

    #[test]
    fn checkpoint_examples_use_the_advertised_action_field() {
        for skill in ALL {
            assert!(
                !skill.body.contains("doc_checkpoint op="),
                "{} uses the unsupported checkpoint op field",
                skill.name
            );
            assert!(
                skill.body.contains("doc_checkpoint action=save"),
                "{} should show the safe checkpoint form",
                skill.name
            );
        }
    }
}
