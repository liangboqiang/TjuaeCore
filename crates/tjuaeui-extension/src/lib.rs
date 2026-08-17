#![warn(clippy::disallowed_types)]

//! Extension registry plus the single local skill-workspace model.

mod asset_paths;
pub mod classifier;
pub mod constants;
pub mod dependency;
pub mod error;
pub mod hub;
pub mod hub_routes;
pub mod lifecycle;
pub mod loader;
pub mod manifest;
pub mod permission;
pub mod registry;
mod registry_helpers;
pub mod resolvers;
pub mod routes;
pub mod skill_package;
pub mod skill_routes;
pub mod skill_storage;
pub mod state;
pub mod template;
pub mod types;
pub mod watcher;

pub use classifier::{AssistantClassifier, AssistantRuleDispatcher, DefaultUserClassifier};
pub use constants::*;
pub use dependency::{DependencyIssue, DependencyValidationResult, topological_sort, validate_dependencies};
pub use error::ExtensionError;
pub use lifecycle::{HookKind, execute_hook, needs_install_hook, resolve_hook_path};
pub use loader::{
    ScanPath, filter_by_engine_compatibility, load_all, resolve_install_target_dir_for_data_dir, resolve_scan_paths,
    resolve_scan_paths_for_data_dir,
};
pub use manifest::{parse_manifest, validate_manifest};
pub use permission::{build_permission_summary, calculate_risk_level};
pub use registry::{ExtensionRegistry, ExtensionSummary};
pub use resolvers::{resolve_all_contributions, resolve_extension_contributions, resolve_i18n_for_all};
pub use state::{ExtensionStateStore, load_states_from_file, resolve_state_file_path, save_states_to_file};
pub use template::{resolve_env_map, resolve_env_templates, resolve_file_reference};
pub use types::*;
pub use watcher::ExtensionWatcher;

pub use hub::{HubIndexManager, HubInstaller};
pub use hub_routes::{HubRouterState, hub_routes};
pub use routes::{ExtensionRouterState, extension_routes};
pub use skill_package::{
    InstalledSkill, MarketFileComparison, MarketIndex, MarketInfo, MarketSkillComparison, MarketSkillEntry,
    MarketSkillPublication, MarketSyncState, SkillManifest, SkillPreferences, SkillSource, clone_skill,
    compare_market_skill, copy_skill, create_skill, delete_installed_skill, ensure_skill_repositories, import_skill,
    initialize_skill_workspaces, install_market_skill, list_installed_skills, load_installed_skill, market_indexes,
    market_sync_state, publish_market_skill, resolve_installed_skill, update_skill_preferences,
};
pub use skill_routes::{SkillRouterState, skill_routes};
pub use skill_storage::{
    ResolvedAgentSkill, SkillPaths, delete_assistant_rule, link_workspace_skills, read_assistant_rule,
    resolve_skill_paths, write_assistant_rule,
};
