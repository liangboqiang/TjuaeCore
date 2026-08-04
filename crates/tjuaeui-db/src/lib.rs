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
    A2aAgentProfileRow, A2aAuditEventRow, A2aCredentialRow, A2aDelegationPermissionRow, A2aDelegationRow,
    A2aPushSubscriptionRow, A2aTaskRow, AgentMetadataRow, AssetCredentialRow, AssetOperationRow, AssetOverlayRow,
    AssetRecordRow, AssetRuntimeBindingRow, AssetRuntimeStateRow, AssetSnapshotRow, AssetTryRunReceiptRow,
    AssetUpstreamRow, AssistantDefinitionRow, AssistantOverlayRow, AssistantPreferenceRow, ConversationArtifactRow,
    ConversationAssistantSnapshotRow, ConversationTraceRow, ConversationTraceRuntimeAssetRefRow,
    ConversationTraceRuntimeAssetSnapshotRow, ConversationTraceRuntimeAssetSnapshotSummaryRow,
    ConversationTraceSpanRow, FolderRow, GithubPublishCredentialRow, GithubPublishOperationRow, ProjectExplorerRow,
    ProjectKind, ProjectRow, Role, SkillRow, UpdateAgentAvailabilitySnapshotParams, UpdateAgentHandshakeParams,
    UpsertAgentMetadataParams, UpsertAssistantDefinitionParams, UpsertAssistantOverlayParams,
    UpsertAssistantPreferenceParams, UpsertConversationAssistantSnapshotParams,
};
pub use repository::UpsertSystemSettingsParams;
pub use repository::channel::UpdatePluginStatusParams;
pub use repository::conversation::{
    ConversationFilters, ConversationRowUpdate, MessagePageCursor, MessagePageDirection, MessagePageParams,
    MessagePageResult, MessageRowUpdate, MessageSearchRow,
};
pub use repository::conversation_trace::{
    CONVERSATION_TRACE_MAX_PER_CONVERSATION, CONVERSATION_TRACE_MAX_RUNTIME_ASSETS,
    CONVERSATION_TRACE_MAX_SAFE_ATTRIBUTES_BYTES, CONVERSATION_TRACE_MAX_SPANS, CONVERSATION_TRACE_RETENTION_DAYS,
    CompleteConversationTraceParams, ConversationTraceObservation, ConversationTraceSpanWriteResult,
};
pub use repository::cron::{
    ClaimCronRunParams, CronRunClaimResult, FinishCronRunParams, RecoverableCronRun, UpdateCronJobParams,
};
pub use repository::mcp_server::{CreateMcpServerParams, UpdateMcpServerParams};
pub use repository::oauth_token::UpsertOAuthTokenParams;
pub use repository::provider::{CreateProviderParams, UpdateProviderParams};
pub use repository::skill::UpsertSkillParams;
pub use repository::team::{UpdateTaskParams, UpdateTeamParams};
pub use repository::{
    CommitAssetRuntimeBindingParams, CommitResolvedAssetParams, CommitTrackedAssetParams, ConfigureAssetOverlayParams,
    CreateA2aDelegationParams, CreateA2aDelegationPermissionParams, CreateAcpSessionParams, CreateAssetSnapshotParams,
    CreateAssetTryRunReceiptParams, EncryptedAssetSecretUpdate, FeedbackDiagnosticsDbContext,
    FeedbackDiagnosticsProfile, FeedbackDiagnosticsProfileResult, FeedbackDiagnosticsRequest,
    FeedbackDiagnosticsResult, IA2aRepository, IAcpSessionRepository, IAgentMetadataRepository, IAssetRepository,
    IAssistantDefinitionRepository, IAssistantOverlayRepository, IAssistantPreferenceRepository, IChannelRepository,
    IClientPreferenceRepository, IConversationRepository, IConversationTraceRepository, ICronRepository,
    IFeedbackDiagnosticsRepository, IGithubPublishCredentialRepository, IGithubPublishOperationRepository,
    IMcpServerRepository, IOAuthTokenRepository, IProjectStore, IProviderRepository, ISettingsRepository,
    ISkillRepository, ITeamRepository, IUserRepository, PersistedSessionState, RecordA2aAuditParams,
    RecordA2aPushDeliveryParams, RecordA2aPushDeliveryResult, SaveRuntimeStateParams, SetAssetRuntimeStateParams,
    SqliteA2aRepository, SqliteAcpSessionRepository, SqliteAgentMetadataRepository, SqliteAssetRepository,
    SqliteAssistantDefinitionRepository, SqliteAssistantOverlayRepository, SqliteAssistantPreferenceRepository,
    SqliteChannelRepository, SqliteClientPreferenceRepository, SqliteConversationRepository,
    SqliteConversationTraceRepository, SqliteCronRepository, SqliteFeedbackDiagnosticsRepository,
    SqliteGithubPublishCredentialRepository, SqliteGithubPublishOperationRepository, SqliteMcpServerRepository,
    SqliteOAuthTokenRepository, SqliteProjectStore, SqliteProviderRepository, SqliteSettingsRepository,
    SqliteSkillRepository, SqliteTeamRepository, SqliteUserRepository, StartAssetOperationParams,
    StartGithubPublishOperationParams, UpdateA2aDelegationParams, UpdateAssetOperationParams,
    UpdateGithubPublishOperationParams, UpsertA2aAgentProfileParams, UpsertA2aCredentialParams,
    UpsertA2aPushSubscriptionParams, UpsertA2aTaskParams, UpsertAssetRecordParams, UpsertAssetUpstreamParams,
    UpsertGithubPublishCredentialParams,
};

// Re-export sqlx pool type for downstream crates
pub use sqlx::SqlitePool;
