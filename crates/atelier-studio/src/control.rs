//! Closed control vocabularies shared by Studio, MCP schemas, recipes, and the
//! CLI. User content (names, paths, labels, drawing text) remains ordinary
//! strings; values that select code paths do not.

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

pub(crate) const DOC_ID_PREFIX: &str = "d_";
pub(crate) const DOC_ID_CHARS: usize = 16;
pub(crate) const DOC_ID_ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

/// Opaque, validated document identity.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct DocumentId(#[schemars(regex(pattern = "^d_[0-9abcdefghjkmnpqrstvwxyz]{16}$"))] String);

impl DocumentId {
    pub fn parse(value: impl Into<String>) -> Result<Self, String> {
        let value = value.into();
        if Self::is_valid(&value) {
            Ok(Self(value))
        } else {
            Err(format!(
                "invalid document id '{value}' — expected d_ plus 16 lowercase Crockford Base32 characters"
            ))
        }
    }

    pub fn is_valid(value: &str) -> bool {
        value.len() == DOC_ID_PREFIX.len() + DOC_ID_CHARS
            && value.starts_with(DOC_ID_PREFIX)
            && value[DOC_ID_PREFIX.len()..]
                .bytes()
                .all(|byte| DOC_ID_ALPHABET.contains(&byte))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for DocumentId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

impl std::str::FromStr for DocumentId {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl std::ops::Deref for DocumentId {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl AsRef<str> for DocumentId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::fmt::Display for DocumentId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

// Three deliberately small tiers keep generated API surface honest:
// `control_enum` is serde/schema-only, `named_control_enum` also exposes its
// wire spelling, and `parsed_control_enum` adds parsing plus an `ALL` registry.
macro_rules! named_control_impl {
    ($name:ident { $($variant:ident => $value:literal),+ $(,)? }) => {
        impl $name {
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $value),+
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(self.as_str())
            }
        }
    };
}

macro_rules! parsed_control_impl {
    ($label:literal, $name:ident { $($variant:ident => $value:literal),+ $(,)? }) => {
        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
        }

        impl std::str::FromStr for $name {
            type Err = String;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                match value {
                    $($value => Ok(Self::$variant)),+,
                    other => Err(format!(
                        "unknown {} '{other}' — expected one of: {}",
                        $label,
                        Self::ALL
                            .iter()
                            .map(|item| item.as_str())
                            .collect::<Vec<_>>()
                            .join(" | ")
                    )),
                }
            }
        }

        named_control_impl!($name { $($variant => $value),+ });
    };
}

macro_rules! parsed_control_enum {
    (
        $label:literal,
        $(#[$meta:meta])*
        pub enum $name:ident {
            $($variant:ident => $value:literal),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
        pub enum $name {
            $(#[serde(rename = $value)] $variant),+
        }

        parsed_control_impl!($label, $name { $($variant => $value),+ });
    };
}

macro_rules! control_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident {
            $($variant:ident => $value:literal),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
        pub enum $name {
            $(#[serde(rename = $value)] $variant),+
        }
    };
}

macro_rules! named_control_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident {
            $($variant:ident => $value:literal),+ $(,)?
        }
    ) => {
        control_enum! {
            $(#[$meta])*
            pub enum $name {
                $($variant => $value),+
            }
        }
        named_control_impl!($name { $($variant => $value),+ });
    };
}

macro_rules! default_control_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident {
            $default_variant:ident => $default_value:literal,
            $($variant:ident => $value:literal),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(
            Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema,
        )]
        pub enum $name {
            #[default]
            #[serde(rename = $default_value)]
            $default_variant,
            $(#[serde(rename = $value)] $variant),+
        }
    };
}

parsed_control_enum! {
    "tool",
    /// The complete public tool surface. This is the single source used by
    /// dispatch, recipes, journaling, mutation classification, and tests.
    pub enum ToolName {
        DeleteDoc => "delete_doc",
        DocAddTag => "doc_add_tag",
        DocAnimAudit => "doc_anim_audit",
        DocBatch => "doc_batch",
        DocCheckpoint => "doc_checkpoint",
        DocComponents => "doc_components",
        DocContactSheet => "doc_contact_sheet",
        DocCritique => "doc_critique",
        DocDitherRamp => "doc_dither_ramp",
        DocDraw => "doc_draw",
        DocDumpRegion => "doc_dump_region",
        DocExport => "doc_export",
        DocFrame => "doc_frame",
        DocFrameDiff => "doc_frame_diff",
        DocFx => "doc_fx",
        DocInfo => "doc_info",
        DocLayer => "doc_layer",
        DocLook => "doc_look",
        DocNew => "doc_new",
        DocPaintGrid => "doc_paint_grid",
        DocPalette => "doc_palette",
        DocRef => "doc_ref",
        DocRegion => "doc_region",
        DocSeamReport => "doc_seam_report",
        DocSilhouette => "doc_silhouette",
        ListDocs => "list_docs"
    }
}

impl ToolName {
    pub const fn is_read_only(self) -> bool {
        match self {
            Self::DocLook
            | Self::DocInfo
            | Self::DocCritique
            | Self::DocSilhouette
            | Self::DocDumpRegion
            | Self::DocComponents
            | Self::DocAnimAudit
            | Self::DocFrameDiff
            | Self::DocSeamReport
            | Self::DocContactSheet
            | Self::ListDocs
            | Self::DocExport => true,
            Self::DeleteDoc
            | Self::DocAddTag
            | Self::DocBatch
            | Self::DocCheckpoint
            | Self::DocDitherRamp
            | Self::DocDraw
            | Self::DocFrame
            | Self::DocFx
            | Self::DocLayer
            | Self::DocNew
            | Self::DocPaintGrid
            | Self::DocPalette
            | Self::DocRef
            | Self::DocRegion => false,
        }
    }

    pub const fn is_recipe_step(self) -> bool {
        match self {
            Self::DocNew
            | Self::DocAddTag
            | Self::DocBatch
            | Self::DocDraw
            | Self::DocDitherRamp
            | Self::DocFrame
            | Self::DocFx
            | Self::DocLayer
            | Self::DocPaintGrid
            | Self::DocPalette
            | Self::DocRegion => true,
            Self::DeleteDoc
            | Self::DocAnimAudit
            | Self::DocCheckpoint
            | Self::DocComponents
            | Self::DocContactSheet
            | Self::DocCritique
            | Self::DocDumpRegion
            | Self::DocExport
            | Self::DocFrameDiff
            | Self::DocInfo
            | Self::DocLook
            | Self::DocRef
            | Self::DocSeamReport
            | Self::DocSilhouette
            | Self::ListDocs => false,
        }
    }
}

named_control_enum! {
    pub enum LayerOp {
        Add => "add",
        Set => "set",
        Move => "move",
        Insert => "insert",
        Delete => "delete",
        Rename => "rename",
        Duplicate => "duplicate",
        MergeDown => "merge_down"
    }
}

named_control_enum! {
    pub enum FrameOp {
        Add => "add",
        Duration => "duration",
        Delete => "delete",
        Insert => "insert",
        Duplicate => "duplicate",
        Move => "move"
    }
}

control_enum! {
    pub enum CheckpointAction {
        Save => "save",
        List => "list",
        Restore => "restore",
        Prune => "prune"
    }
}

default_control_enum! {
    pub enum PaletteOp {
        Generate => "generate",
        Set => "set",
        Snap => "snap",
        Swap => "swap",
        Report => "report"
    }
}

default_control_enum! {
    pub enum PaletteScheme {
        Mono => "mono",
        Complementary => "complementary",
        Triadic => "triadic",
        Analogous => "analogous",
        Split => "split",
        Tetradic => "tetradic"
    }
}

named_control_enum! {
    pub enum RegionOp {
        Clear => "clear",
        Move => "move"
    }
}

control_enum! {
    pub enum ReferenceOp {
        Set => "set",
        Analyze => "analyze",
        Compare => "compare",
        Diff => "diff"
    }
}

default_control_enum! {
    pub enum CompareMode {
        SideBySide => "side_by_side",
        Overlay => "overlay"
    }
}

control_enum! {
    pub enum ExportOp {
        Sheet => "sheet",
        Anim => "anim"
    }
}

default_control_enum! {
    pub enum SheetMeta {
        Atelier => "atelier",
        Standard => "standard"
    }
}

default_control_enum! {
    pub enum AnimationFormat {
        Gif => "gif",
        Apng => "apng"
    }
}

default_control_enum! {
    pub enum DumpMode {
        Symbol => "symbol",
        Hex => "hex"
    }
}

default_control_enum! {
    pub enum DiffRender {
        None => "none",
        Overlay => "overlay"
    }
}

default_control_enum! {
    pub enum SeamAxis {
        Both => "both",
        Horizontal => "horizontal",
        Vertical => "vertical"
    }
}

control_enum! {
    pub enum AnimAuditMode {
        Seam => "seam",
        Spacing => "spacing",
        Arc => "arc",
        Timing => "timing"
    }
}

default_control_enum! {
    pub enum LookMode {
        Render => "render",
        Value => "value",
        Bands => "bands",
        Saturation => "sat",
        Hue => "hue",
        Notan => "notan"
    }
}

control_enum! {
    pub enum LookBackground {
        Checker => "checker",
        Dark => "dark",
        White => "white"
    }
}

default_control_enum! {
    pub enum AlphaMode {
        Preserve => "preserve",
        Opaque => "opaque",
        Flatten => "flatten"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names_are_unique_and_round_trip_as_protocol_strings() {
        let names: std::collections::HashSet<&str> =
            ToolName::ALL.iter().map(|tool| tool.as_str()).collect();
        assert_eq!(names.len(), ToolName::ALL.len());
        for tool in ToolName::ALL {
            assert_eq!(tool.as_str().parse::<ToolName>().unwrap(), *tool);
            assert_eq!(
                serde_json::to_value(tool).unwrap(),
                serde_json::Value::String(tool.as_str().to_string())
            );
        }
    }

    #[test]
    fn document_ids_reject_paths_and_noncanonical_characters() {
        assert!(DocumentId::parse("d_0000000000000000").is_ok());
        for invalid in [
            "hero",
            "../d_0000000000000000",
            "d_000000000000000",
            "d_000000000000000i",
            "d_000000000000000O",
        ] {
            assert!(DocumentId::parse(invalid).is_err(), "accepted {invalid}");
        }
    }
}
