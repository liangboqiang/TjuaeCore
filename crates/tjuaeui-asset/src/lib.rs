#![warn(clippy::disallowed_types)]

mod assistant_rules;
mod definition;
mod error;
mod projection_id;
mod publish;
mod publish_error;
mod publish_provider;
mod publish_routes;
mod remote_market;
mod runtime;
mod service;
mod skill_routes;
mod skill_runtime;
mod store;
mod three_way;
mod typed_definition;

pub use assistant_rules::AssistantRuleDispatcher;
pub use definition::{
    AssetDefinitionFile, DefinitionManifestEntry, MAX_DEFINITION_FILE_BYTES, MAX_DEFINITION_FILES,
    MAX_DEFINITION_TOTAL_BYTES, ScannedDefinition, digest_bytes, load_definition, normalize_relative_path,
    prepare_definition, scan_definition,
};
pub use error::AssetError;
pub use projection_id::{
    PROJECTION_RUNTIME_ID_LENGTH, PROJECTION_RUNTIME_ID_PREFIX, derive_projection_runtime_id, is_projection_runtime_id,
};
pub use publish::{AssetTextFile, DisabledHubAssetPort, HubAssetPort, HubAssetService, LocalAssetMaterial};
pub use publish_error::AssetPublishError;
pub use publish_provider::{GitHubRestPublishProvider, HubPublishProvider};
pub use publish_routes::{HubRouterState, hub_routes};
pub use remote_market::{
    MARKET_INDEX_SCHEMA_URL, MarketError, MarketIndexManager, OFFLINE_RESOURCE_MANIFEST_SCHEMA,
    OFFLINE_SEED_SCHEMA_URL, TJUAE_ASSET_PROTOCOL_VERSION,
};
pub use runtime::{
    AssetRuntimeProjector, FailClosedRuntimeProjector, RuntimeAssetConfigurationResolver, RuntimeAssetDefinition,
    RuntimeProjectionTransaction, RuntimeResolvedConfiguration,
};
pub use service::{
    AssetCatalogService, BoundRuntimeAsset, LocalAssetInput, RuntimeAssetProvenance, TrackedAssetInput,
    calculate_sync_state,
};
pub use skill_routes::{SkillRouterState, skill_routes};
pub use skill_runtime::{
    ResolvedAgentSkill, SkillListItem, SkillPaths, SkillSource, link_workspace_skills, list_available_skills,
    list_available_skills_with_repo, materialize_skills_for_agent, materialize_skills_for_agent_with_repo,
    resolve_skill_paths, sync_generated_skill_projections,
};
pub use store::AssetContentStore;
pub use typed_definition::{
    ENGINE_ADAPTER_DEFINITION_SCHEMA_URL, MCP_DEFINITION_SCHEMA_URL, parse_engine_adapter_definition,
    parse_mcp_definition,
};
