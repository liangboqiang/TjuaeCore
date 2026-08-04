#![warn(clippy::disallowed_types)]

//! All HTTP request/response DTOs shared across the API surface.
mod a2a;
mod acp;
mod acp_prompt_hook;
mod agent_build_extra;
mod agent_discovery;
mod agent_error;
mod asset;
mod asset_definition;
mod asset_runtime;
mod assistant;
mod auth;
mod channel;
mod confirmation;
mod connection_test;
mod conversation;
mod cron;
mod extension;
mod file;
mod hub;
mod lifecycle;
mod market;
mod mcp;
mod office;
mod provider;
mod response;
mod runtime;
mod shell;
mod skill;
mod system;
mod team;
mod team_mcp;
mod team_tools;
mod websocket;

pub use a2a::{
    A2aAgentCardSummary, A2aAgentInterfaceSummary, A2aAgentResponse, A2aAgentSkillSummary, A2aAuditEventResponse,
    A2aAuthKind, A2aBinding, A2aCompatibilityMode, A2aConfiguredCredentialSummary, A2aCredentialInput,
    A2aCredentialLocation, A2aDelegationGraphResponse, A2aDelegationPermissionResponse, A2aDelegationResponse,
    A2aDelegationTaskNode, A2aOAuthFlowKind, A2aPushSubscriptionResponse, CompleteA2aOAuthRequest,
    CreateA2aAgentRequest, DelegateA2aTaskRequest, DiscoverA2aAgentRequest, DiscoverA2aAgentResponse,
    RegisterA2aPushRequest, RequestA2aDelegationPermission, StartA2aOAuthRequest, StartA2aOAuthResponse,
    UpdateA2aAgentRequest,
};
pub use acp::{
    AcpConfigOptionDto, AcpConfigSelectOptionDto, AcpEnvResponse, AgentModeResponse, ConfigOptionConfirmation,
    DetectCliRequest, DetectCliResponse, EngineAdapterProbeResponse, GetConfigOptionsResponse, GetModelInfoResponse,
    ModelInfoEntry, ModelInfoPayload, ProbeModelRequest, SetConfigOptionRequest, SetConfigOptionResponse,
    SetModeRequest, SetModelRequest, SideQuestionRequest, SideQuestionResponse, WorkspaceBrowseQuery, WorkspaceEntry,
};
pub use acp_prompt_hook::AcpPromptHookWarningPayload;
pub use agent_build_extra::{
    AcpBuildExtra, AcpModelInfo, SessionMcpServer, SessionMcpTransport, SlashCommandCompletionBehavior,
    SlashCommandItem, TjuaeCliBuildExtra,
};
pub use agent_discovery::{
    AgentDiagnosticRun, AgentDiagnosticRunState, AgentDiagnosticsChangedPayload, AgentEnvEntry, AgentHandshake,
    AgentLogoEntry, AgentManagementRow, AgentManagementStatus, AgentMetadata, AgentOverridesResponse,
    AgentSnapshotCheckKind, AgentSnapshotCheckStatus, AgentSource, AgentSourceInfo, BehaviorPolicy,
    StartAgentDiagnosticsRequest,
};
pub use agent_error::{
    AgentErrorCode, AgentErrorOwnership, AgentErrorResolution, AgentErrorResolutionKind, AgentErrorResolutionTarget,
    AgentStreamErrorData,
};
pub use asset::{
    AssetAction, AssetCollaborationCapability, AssetCollaborationProtocolResponse, AssetContentSource,
    AssetDetailResponse, AssetDiffFileResponse, AssetDiffFileStatus, AssetDiffResponse, AssetEditability,
    AssetFileEntryResponse, AssetFileResponse, AssetKind, AssetOperationKind, AssetOperationRequest,
    AssetOperationResponse, AssetOperationState, AssetOrigin, AssetResolveResponse, AssetResolveStrategy,
    AssetRestoreResponse, AssetScope, AssetSummaryResponse, AssetSyncState, AssetTrackingMode, AssetTrust,
    AssetUpstreamResponse, CreateAssetRequest, DuplicateAssetRequest, GetAssetQuery, ListAssetsQuery,
    ReadAssetFileQuery, ResolveAssetRequest, RestoreAssetRequest, WriteAssetFileRequest,
};
pub use asset_definition::{
    AssetConfigurationBindingTarget, AssetConfigurationFieldBindingDefinition, AssetConfigurationFieldDefinition,
    AssetConfigurationSchemaDefinition, AssetConfigurationValueType, EngineAdapterCapabilitiesDefinition,
    EngineAdapterDefinition, EngineAdapterProtocolDefinition, EngineAdapterProtocolType, EngineAdapterTransport,
    McpCapabilitiesDefinition, McpDefinition, McpTransportDefinition, PortableNpmPackageDefinition,
    PortablePackageEcosystem, PortablePackageRunner, PortableRuntimeDefinition,
};
pub use asset_runtime::{
    AssetConfigurationValue, AssetKeyedSecretSlot, AssetNamedSecretSlot, AssetOverlayResponse, AssetPrimitiveValue,
    AssetPublicConfiguration, AssetRuntimeBindingResponse, AssetRuntimeCommandRequest, AssetRuntimeHealthStatus,
    AssetRuntimeProjectionKind, AssetRuntimeState, AssetRuntimeStatusResponse, AssetSecretSlotResponse,
    AssetSecretUpdate, AssistantAssetConfiguration, ConfigureAssetRequest, EngineAdapterAssetConfiguration,
    McpAssetConfiguration, McpAssetTransport, SkillAssetConfiguration,
};
pub use assistant::{
    AssistantCapabilitiesResponse, AssistantDefaultListResponse, AssistantDefaultScalarResponse,
    AssistantDefaultsResponse, AssistantDetailResponse, AssistantEngineDescriptor, AssistantEngineResponse,
    AssistantPreferencesResponse, AssistantProfileResponse, AssistantPromptsResponse, AssistantResponse,
    AssistantRulesResponse, AssistantSource, AssistantStateResponse, assistant_avatar_response_value,
    assistant_avatar_response_value_with_version, is_local_avatar_value,
};
pub use auth::{
    AuthStatusResponse, ChangePasswordRequest, LoginRequest, LoginResponse, PublicUser, QrLoginRequest,
    RefreshResponse, RefreshTokenRequest, UserInfoResponse, WebuiChangePasswordRequest, WebuiChangeUsernameRequest,
    WebuiChangeUsernameResponse, WebuiGenerateQrTokenResponse, WebuiResetPasswordResponse, WsTokenResponse,
};
pub use channel::{
    ApprovePairingRequest, BridgeResponse, ChannelAssistantSettingRequest, ChannelAssistantSettingResponse,
    ChannelDefaultModelSetting, ChannelPlatformSettingsResponse, ChannelSessionResponse, ChannelUserResponse,
    DisablePluginRequest, EnablePluginRequest, PairingRequestResponse, PairingRequestedPayload,
    PluginStatusChangedPayload, PluginStatusResponse, RejectPairingRequest, RevokeUserRequest,
    SyncChannelSettingsRequest, TestPluginExtraConfig, TestPluginRequest, TestPluginResponse, UserAuthorizedPayload,
};
pub use confirmation::{ApprovalCheckQuery, ApprovalCheckResponse, ConfirmRequest, ConfirmationListResponse};
pub use connection_test::TestBedrockConnectionRequest;
pub use conversation::{
    ActiveCountResponse, AssistantConversationOverridesRequest, AssistantConversationRequest,
    CancelConversationRequest, CancelConversationResponse, CloneConversationRequest, ConversationArtifactKind,
    ConversationArtifactListResponse, ConversationArtifactResponse, ConversationArtifactStatus,
    ConversationAssistantIdentityResponse, ConversationListResponse, ConversationMcpStatus, ConversationMcpStatusKind,
    ConversationResponse, ConversationRuntimeStateKind, ConversationRuntimeSummary, ConversationTraceDetailResponse,
    ConversationTraceListResponse, ConversationTraceRuntimeAssetRef, ConversationTraceRuntimeAssetSnapshot,
    ConversationTraceSpan, ConversationTraceSpanKind, ConversationTraceSpanStatus, ConversationTraceStatus,
    ConversationTraceSummary, ConversationTraceUpdateKind, ConversationTraceUpdatedPayload, CreateConversationRequest,
    EnsureConversationRuntimeResponse, ListConversationTracesQuery, ListConversationsQuery, ListMessagesQuery,
    MessageListResponse, MessageResponse, MessageSearchItem, MessageSearchResponse, SearchMessagesQuery,
    SendMessageRequest, SendMessageResponse, UpdateConversationArtifactRequest, UpdateConversationRequest,
};
pub use cron::{
    CreateConversationCronRequest, CreateConversationCronResponse, CreateCronJobRequest, CronAgentConfigReadDto,
    CronAgentConfigWriteDto, CronJobExecutedEvent, CronJobMetadataDto, CronJobPayloadDto, CronJobRemovedPayload,
    CronJobResponse, CronJobStateDto, CronJobTargetDto, CronScheduleDto, HasSkillResponse, ListCronJobsQuery,
    RunNowResponse, SaveCronSkillRequest, UpdateConversationCronRequest, UpdateCronJobRequest,
};
pub use extension::{
    DisableExtensionRequest, EnableExtensionRequest, ExtensionSummaryResponse, GetI18nRequest, GetPermissionsRequest,
    GetRiskLevelRequest, PermissionDetailResponse, PermissionSummaryResponse,
};
pub use file::{
    BrowseDirectoryQuery, BrowseDirectoryResponse, BrowseEntry, CancelZipRequest, CopyFilesRequest, CopyFilesResponse,
    CreateTempFileRequest, DirOrFileResponse, FetchRemoteImageRequest, FileChangeInfoResponse, FileMetadataResponse,
    FileWatchRequest, GetFileMetadataRequest, GetFilesByDirRequest, GetImageBase64Request, ListWorkspaceFilesRequest,
    ReadFileBufferRequest, ReadFileRequest, RemoveEntryRequest, RenameRequest, RenameResponse, SnapshotBaselineRequest,
    SnapshotCompareResponse, SnapshotDiscardRequest, SnapshotInfoResponse, SnapshotMode, SnapshotStageRequest,
    SnapshotWorkspaceRequest, WorkspaceFlatFileResponse, WorkspaceOfficeWatchRequest, WriteFileRequest, ZipFileEntry,
    ZipRequest,
};
pub use hub::{
    CanonicalAssetFile, CanonicalAssetPackage, HubAssetKind, HubAssetPublishPreparation, HubAssetPublishRequest,
    HubAssetPublishResponse, HubAssetPublishWarningCode, HubPublishConnectionState, HubPublishConnectionStatus,
};
pub use lifecycle::{GitHubReleaseAsset, SystemInfoResponse, UpdateCheckRequest, UpdateCheckResult, UpdateReleaseInfo};
pub use market::{
    InstallMarketAssetRequest, ListMarketAssetsQuery, MarketAssetDescriptor, MarketAssetFileResponse,
    MarketAssetResponse, MarketAssetStatus, MarketCacheResponse, MarketCompatibilityResponse, MarketIndexResponse,
    MarketLocalRelationResponse, MarketPackageDescriptor, MarketPackageReviewStatus, MarketPresenceState,
    ReadMarketAssetFileQuery, RefreshMarketRequest,
};
pub use mcp::{
    DetectedMcpServerEntry, DetectedMcpServerResponse, McpAuthMethod, McpConnectionTestErrorCode,
    McpConnectionTestResult, McpServerResponse, McpToolResponse, McpTransport,
};
pub use office::{
    CellCoord, CellRange, ConversionResultDto, ConversionTarget, DocumentConversionRequest, DocumentConversionResponse,
    ExcelSheetData, ExcelSheetImage, ExcelWorkbookData, GetSnapshotContentRequest, ListSnapshotsRequest,
    PreviewHistoryTargetDto, PreviewSnapshotInfoDto, SaveSnapshotRequest, SnapshotContentResponse,
};
pub use provider::{
    BedrockAuthMethod, BedrockConfig, CreateProviderRequest, DetectProtocolRequest, DetectionSuggestion,
    FetchModelsAnonymousRequest, FetchModelsRequest, FetchModelsResponse, HealthStatus, KeyTestResult, ModelCapability,
    ModelHealthStatus, ModelImageInputCapability, ModelInfo, ModelOpenAiApiMode, ModelSettings, ModelType,
    MultiKeyResult, ProtocolDetectionResponse, ProviderHealthCheckErrorKind, ProviderHealthCheckRequest,
    ProviderHealthCheckResponse, ProviderResponse, SuggestionType, UpdateProviderRequest,
};
pub use response::{ApiResponse, ErrorResponse};
pub use runtime::{
    EnsureNodeRuntimeRequest, EnsureNodeRuntimeResponse, RuntimeFailureKind, RuntimeResourceKind, RuntimeStatusPayload,
    RuntimeStatusPhase, RuntimeStatusScope, RuntimeStatusScopeKind,
};
pub use shell::{
    CheckToolInstalledRequest, CheckToolInstalledResponse, DeepgramSpeechToTextConfig, OpenAISpeechToTextConfig,
    OpenExternalRequest, OpenFileRequest, OpenFolderWithRequest, ShowItemInFolderRequest, SpeechToTextConfig,
    SpeechToTextProvider, SpeechToTextResult, SttStreamClientMessage, SttStreamServerMessage, ToolType,
};
pub use skill::{
    MaterializeSkillsRequest, MaterializeSkillsResponse, MaterializedSkillRef, ReadAssistantRuleRequest,
    SkillListItemResponse, SkillSourceResponse,
};
pub use system::{
    ClientPreferencesResponse, DEFAULT_NETWORK_PROXY_BYPASS, FeedbackDiagnosticsContextResponse,
    FeedbackDiagnosticsPrivacyResponse, FeedbackDiagnosticsProfileResponse, FeedbackDiagnosticsQuery,
    FeedbackDiagnosticsResponse, NetworkProxyMode, NetworkProxySettings, NetworkProxySource, NetworkProxyState,
    NetworkProxyStatusResponse, NetworkProxyWarning, SystemSettingsResponse, UpdateClientPreferencesRequest,
    UpdateSettingsRequest,
};
pub use team::{
    AddAgentRequest, CancelTeamChildTurnRequest, CancelTeamRunRequest, CreateTeamRequest, PauseTeamSlotRequest,
    RenameAgentRequest, RenameTeamRequest, SendAgentMessageRequest, SendTeamMessageRequest, TeamAgentInput,
    TeamAgentRemovedPayload, TeamAgentRenamedPayload, TeamAgentResponse, TeamAgentRuntimeStatus,
    TeamAgentRuntimeStatusPayload, TeamAgentSpawnedPayload, TeamAgentStatusPayload, TeamChildTurnPayload,
    TeamListResponse, TeamMcpRuntimeConfig, TeamMessageEnqueueStatus, TeamResponse, TeamRunAckResponse, TeamRunPayload,
    TeamRunSource, TeamRunStateResponse, TeamRunStatus, TeamRunTargetRole, TeamRuntimeSeed,
    TeamSendMessageQueuedResponse, TeamSessionBinding, TeamSessionPhase, TeamSessionStatus, TeamSessionStatusPayload,
    TeamSlotBlockedReason, TeamSlotWorkChangedPayload, TeamSlotWorkPayload, TeamSlotWorkState, TeammateMessagePayload,
};
pub use team_mcp::{TEAM_MCP_SERVER_NAME, TeamMcpStdioConfig};
pub use team_tools::{
    TEAM_DESCRIBE_ASSISTANT_DESCRIPTION, TEAM_LIST_ASSISTANTS_DESCRIPTION, TEAM_SPAWN_AGENT_DESCRIPTION,
    TEAM_TOOLS_SCHEMA_VERSION, TeamToolCall, TeamToolCliEnvelope, TeamToolCliMeta, TeamToolContextResponse,
    TeamToolDescriptor, TeamToolErrorCode, TeamToolErrorPayload, TeamToolName, TeamToolPermission, TeamToolRole,
    TeamToolRuntimeCallRequest, TeamToolRuntimeCallResponse, TeamToolTransport, cli_command_for_tool,
    team_tool_descriptor, team_tool_descriptors, team_tool_descriptors_for_role, tool_name_for_cli_path,
};
pub use websocket::WebSocketMessage;

#[cfg(test)]
mod public_contract_tests {
    use super::{AgentErrorResolution, AgentErrorResolutionKind, AgentErrorResolutionTarget};

    #[test]
    fn error_resolution_types_are_exported_from_crate_root() {
        let resolution = AgentErrorResolution::new(
            AgentErrorResolutionKind::Retry,
            Some(AgentErrorResolutionTarget::Feedback),
        );

        assert_eq!(resolution.kind, AgentErrorResolutionKind::Retry);
        assert_eq!(resolution.target, Some(AgentErrorResolutionTarget::Feedback));
    }
}
