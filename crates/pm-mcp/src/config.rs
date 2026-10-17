//! Server configuration shared by the CLI and the GUI's options panel.

use serde::{Deserialize, Serialize};

/// Default TCP port of the streamable HTTP transport (Autopsy's).
pub const DEFAULT_PORT: u16 = 8743;

/// What the assistant may do and see. Every switch defaults to the safe
/// setting (read-only, no pictures, Project VIC CAM content withheld).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct McpConfig {
    /// Enables the write tools (`add_tag`, `remove_tag`,
    /// `save_search_results`): "Allow the assistant to tag and comment".
    pub allow_writes: bool,
    /// Enables `get_image_preview` (pictures may leave the machine when the
    /// client uses a cloud model).
    pub image_previews: bool,
    /// Returns the bytes and text of files tagged "Child Abuse Material -
    /// (CAM)" (Project VIC). Off: those files are withheld from every
    /// content tool; their metadata is still listed.
    pub show_cam_content: bool,
    /// TCP port of the streamable HTTP transport (bound to 127.0.0.1).
    pub port: u16,
    /// Append every call to `<case>/Log/mcp-audit.jsonl`.
    pub audit: bool,
    /// The examiner's keyword lists (`keywords.xml`) for
    /// `run_keyword_list`; the built-in lists are always available.
    pub keyword_lists: Option<std::path::PathBuf>,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            allow_writes: false,
            image_previews: false,
            show_cam_content: false,
            port: DEFAULT_PORT,
            audit: true,
            keyword_lists: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_safe_and_round_trip() {
        let c = McpConfig::default();
        assert!(!c.allow_writes && !c.image_previews && !c.show_cam_content && c.audit);
        let back: McpConfig = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(back, c);
        let partial: McpConfig = serde_json::from_str(r#"{"allow_writes":true}"#).unwrap();
        assert!(partial.allow_writes);
        assert_eq!(partial.port, DEFAULT_PORT);
    }
}
