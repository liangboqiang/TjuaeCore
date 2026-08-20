#![warn(clippy::disallowed_types)]

//! SQLite database layer: init, migrations, repository traits, and implementations.
mod agent_binding;
mod database;
mod error;
mod instance_lock;
pub mod models;
mod repository;

pub use agent_binding::{
    AgentBindingResolution, binding_resolution_for_agent, resolve_agent_binding, resolve_agent_binding_from_rows,
    runtime_backend_for_agent,
};
pub use database::{
    Database, DatabaseInitError, DatabaseInitOptions, init_database, init_database_memory, init_database_staged,
    init_database_staged_with_options, init_database_with_options,
};
pub use error::{
    DbError, SQLITE_BUSY_MESSAGE_MARKERS, SQLITE_UNIQUE_VIOLATION_MARKER, message_indicates_busy,
    message_indicates_unique_violation,
};
pub use instance_lock::{DataDirInstanceGuard, instance_lock_path};
pub use models::{
    AgentMetadataRow, AssistantUserPreferenceRow, ConversationArtifactRow, ConversationAssistantSnapshotRow, FolderRow,
    ProjectExplorerRow, ProjectKind, ProjectRow, Role, SkillUserPreferenceRow, UpdateAgentAvailabilitySnapshotParams,
    UpdateAgentHandshakeParams, UpsertAgentMetadataParams, UpsertConversationAssistantSnapshotParams,
};
pub use repository::channel::UpdatePluginStatusParams;
pub use repository::conversation::{
    ConversationFilters, ConversationRowUpdate, MessagePageCursor, MessagePageDirection, MessagePageParams,
    MessagePageResult, MessageRowUpdate, MessageSearchRow,
};
pub use repository::cron::{
    ClaimCronRunParams, CronRunClaimResult, FinishCronRunParams, RecoverableCronRun, UpdateCronJobParams,
};
pub use repository::mcp_server::{CreateMcpServerParams, UpdateMcpServerParams};
pub use repository::oauth_token::UpsertOAuthTokenParams;
pub use repository::provider::{CreateProviderParams, UpdateProviderParams};
pub use repository::remote_agent::{CreateRemoteAgentParams, UpdateRemoteAgentParams};
pub use repository::team::{UpdateTaskParams, UpdateTeamParams};
pub use repository::{
    CreateAcpSessionParams, FeedbackDiagnosticsDbContext, FeedbackDiagnosticsProfile, FeedbackDiagnosticsProfileResult,
    FeedbackDiagnosticsRequest, FeedbackDiagnosticsResult, IAcpSessionRepository, IAgentMetadataRepository,
    IAssistantUserPreferenceRepository, IChannelRepository, IClientPreferenceRepository, IConversationRepository,
    ICronRepository, IFeedbackDiagnosticsRepository, IMcpServerRepository, IOAuthTokenRepository, IProjectStore,
    IProviderRepository, IRemoteAgentRepository, ISettingsRepository, ISkillUserPreferenceRepository, ITeamRepository,
    IUserRepository, PersistedSessionState, SaveRuntimeStateParams, SqliteAcpSessionRepository,
    SqliteAgentMetadataRepository, SqliteAssistantUserPreferenceRepository, SqliteChannelRepository,
    SqliteClientPreferenceRepository, SqliteConversationRepository, SqliteCronRepository,
    SqliteFeedbackDiagnosticsRepository, SqliteMcpServerRepository, SqliteOAuthTokenRepository, SqliteProjectStore,
    SqliteProviderRepository, SqliteRemoteAgentRepository, SqliteSettingsRepository,
    SqliteSkillUserPreferenceRepository, SqliteTeamRepository, SqliteUserRepository,
    UpsertAssistantUserPreferenceParams, UpsertSkillUserPreferenceParams, UpsertSystemSettingsParams,
};

// Re-export sqlx pool type for downstream crates
pub use sqlx::SqlitePool;
