/// Manifest filename that identifies an extension directory.
pub const EXTENSION_MANIFEST_FILE: &str = "tjuae-extension.json";

/// Default subdirectory name for extensions.
pub const EXTENSIONS_DIR_NAME: &str = "extensions";

/// Current extension API version.
pub const EXTENSION_API_VERSION: &str = "1.0.0";

/// Hub index schema version we support.
pub const HUB_SUPPORTED_SCHEMA_VERSION: u32 = 1;

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
// Lifecycle hook timeouts (seconds)
// ---------------------------------------------------------------------------

/// Timeout for `onInstall` hook — may involve downloading dependencies.
pub const LIFECYCLE_ON_INSTALL_TIMEOUT_SECS: u64 = 120;

/// Timeout for `onUninstall` hook — cleanup operations.
pub const LIFECYCLE_ON_UNINSTALL_TIMEOUT_SECS: u64 = 60;

/// Timeout for `onActivate` hook — runs every activation.
pub const LIFECYCLE_ON_ACTIVATE_TIMEOUT_SECS: u64 = 30;

/// Timeout for `onDeactivate` hook — runs every deactivation.
pub const LIFECYCLE_ON_DEACTIVATE_TIMEOUT_SECS: u64 = 30;

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

/// Default subdirectory name for built-in rules.
pub const BUILTIN_RULES_DIR_NAME: &str = "builtin-rules";

/// Default subdirectory name for assistant-level rules.
pub const ASSISTANT_RULES_DIR_NAME: &str = "assistant-rules";

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
    fn test_lifecycle_timeouts_ordering() {
        // onInstall should have the longest timeout
        const {
            assert!(LIFECYCLE_ON_INSTALL_TIMEOUT_SECS >= LIFECYCLE_ON_ACTIVATE_TIMEOUT_SECS);
            assert!(LIFECYCLE_ON_INSTALL_TIMEOUT_SECS >= LIFECYCLE_ON_DEACTIVATE_TIMEOUT_SECS);
            assert!(LIFECYCLE_ON_UNINSTALL_TIMEOUT_SECS >= LIFECYCLE_ON_DEACTIVATE_TIMEOUT_SECS);
        }
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
