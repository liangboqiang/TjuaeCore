/// Manifest filename that identifies an extension directory.
pub const EXTENSION_MANIFEST_FILE: &str = "tjuae-extension.json";

/// Default subdirectory name for extensions.
pub const EXTENSIONS_DIR_NAME: &str = "extensions";

/// Current extension API version.
pub const EXTENSION_API_VERSION: &str = "1.0.0";

/// Cache TTL for agent activity snapshots (milliseconds).
pub const ACTIVITY_SNAPSHOT_TTL_MS: u64 = 3000;

/// Debounce delay for hot-reload file watching (milliseconds).
pub const DEBOUNCE_MS: u64 = 1000;

/// Debounce delay for state persistence writes (milliseconds).
pub const STATE_PERSIST_DEBOUNCE_MS: u64 = 500;

/// Reserved extension name prefixes that third-party extensions cannot use.
pub const RESERVED_NAME_PREFIXES: &[&str] = &["tjuae-", "internal-", "builtin-", "system-"];

/// Preset agent type identifiers.
pub const PRESET_AGENT_TYPES: &[&str] = &["gemini", "claude", "codex", "codebuddy", "opencode"];

// ---------------------------------------------------------------------------
// Reserved WebUI route prefixes
// ---------------------------------------------------------------------------

/// Route prefixes reserved for internal use — extensions cannot register these.
pub const RESERVED_ROUTE_PREFIXES: &[&str] = &["/api/", "/auth/", "/ws/"];

// ---------------------------------------------------------------------------
// Skill & rule management
// ---------------------------------------------------------------------------

/// Default subdirectory name for user-created skills.
pub const SKILLS_DIR_NAME: &str = "skills";

/// Default subdirectory name for per-job cron skills under the data dir.
pub const CRON_SKILLS_DIR_NAME: &str = "cron/skills";

/// Filename that identifies a skill directory.
pub const SKILL_MANIFEST_FILE: &str = "SKILL.md";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_manifest_file_name() {
        assert_eq!(EXTENSION_MANIFEST_FILE, "tjuae-extension.json");
    }

    #[test]
    fn test_reserved_prefixes_contains_expected() {
        assert!(RESERVED_NAME_PREFIXES.contains(&"tjuae-"));
        assert!(RESERVED_NAME_PREFIXES.contains(&"internal-"));
        assert!(RESERVED_NAME_PREFIXES.contains(&"builtin-"));
        assert!(RESERVED_NAME_PREFIXES.contains(&"system-"));
    }

    #[test]
    fn test_agent_ids_non_empty() {
        assert!(!PRESET_AGENT_TYPES.is_empty());
        assert!(PRESET_AGENT_TYPES.contains(&"claude"));
    }

    #[test]
    fn test_reserved_route_prefixes() {
        assert!(RESERVED_ROUTE_PREFIXES.contains(&"/api/"));
        assert!(RESERVED_ROUTE_PREFIXES.contains(&"/auth/"));
        assert!(RESERVED_ROUTE_PREFIXES.contains(&"/ws/"));
    }

    #[test]
    fn test_debounce_values_positive() {
        const {
            assert!(DEBOUNCE_MS > 0);
            assert!(STATE_PERSIST_DEBOUNCE_MS > 0);
            assert!(ACTIVITY_SNAPSHOT_TTL_MS > 0);
        }
    }
}
