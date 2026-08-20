use std::sync::Arc;

use async_trait::async_trait;
use tjuaeui_api_types::{
    ChannelAssistantSettingRequest, ChannelAssistantSettingResponse, ChannelDefaultModelSetting,
    ChannelPlatformSettingsResponse,
};
use tjuaeui_common::ProviderWithModel;
use tjuaeui_db::IClientPreferenceRepository;
use tracing::debug;

use crate::error::ChannelError;
use crate::types::PluginType;

const DEFAULT_AGENT_TYPE: &str = "tjuaecli";

/// Per-plugin agent/model configuration read from `client_preferences`.
///
/// Channel settings persist only the activated assistant identity:
/// - `assistant.{platform}.agent`       → JSON `{"assistant_id":"tjuae-hub:official/tjuaeui-assistant"}`
/// - `assistant.{platform}.defaultModel` → JSON `{"id":"provider_id","use_model":"model_name"}`
pub struct ChannelSettingsService {
    pref_repo: Arc<dyn IClientPreferenceRepository>,
    assistant_catalog: Arc<dyn ChannelAssistantCatalogPort>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelAssistantCatalogEntry {
    pub assistant_id: String,
    pub name: String,
    pub agent_type: String,
    pub backend: Option<String>,
}

#[async_trait]
pub trait ChannelAssistantCatalogPort: Send + Sync {
    async fn list_runtime_assistants(&self) -> Result<Vec<ChannelAssistantCatalogEntry>, ChannelError>;

    async fn resolve_runtime_assistant(
        &self,
        assistant_id: &str,
    ) -> Result<Option<ChannelAssistantCatalogEntry>, ChannelError> {
        Ok(self
            .list_runtime_assistants()
            .await?
            .into_iter()
            .find(|assistant| assistant.assistant_id == assistant_id))
    }
}

/// Resolved agent configuration for a channel platform.
///
/// `backend` is only meaningful for ACP agents (claude, gemini, codex, …).
/// Non-ACP agent types (tjuae_cli, nanobot, remote, …) have `backend = None`.
#[derive(Debug, Clone)]
pub struct ResolvedAgentConfig {
    pub agent_type: String,
    pub backend: Option<String>,
}

/// Resolved model configuration for a channel platform.
#[derive(Debug, Clone)]
pub struct ResolvedModelConfig {
    pub provider_id: String,
    pub model: String,
    pub use_model: Option<String>,
}

impl ChannelSettingsService {
    pub fn new(
        pref_repo: Arc<dyn IClientPreferenceRepository>,
        assistant_catalog: Arc<dyn ChannelAssistantCatalogPort>,
    ) -> Self {
        Self {
            pref_repo,
            assistant_catalog,
        }
    }

    /// Reads the agent configuration for a platform from `client_preferences`.
    ///
    /// The saved identity is resolved through the unified activated-assistant
    /// catalog. When no binding exists, the activated Tjuae CLI assistant is used.
    pub async fn get_agent_config(&self, platform: PluginType) -> Result<ResolvedAgentConfig, ChannelError> {
        let key = agent_key(platform);
        let prefs = self.pref_repo.get_by_keys(&[&key]).await?;

        let Some(pref) = prefs.into_iter().next() else {
            return Ok(default_agent_config());
        };

        if let Some(setting) = parse_channel_assistant_setting(&pref.value)
            && let Some(assistant_id) = setting.assistant_id.as_deref()
        {
            if let Some(resolved) = self.resolve_assistant_agent_config(assistant_id).await? {
                debug!(
                    platform = %platform,
                    assistant_id,
                    agent_type = %resolved.agent_type,
                    backend = ?resolved.backend,
                    "resolved channel agent config from assistant identity"
                );
                return Ok(resolved);
            }

            return Err(ChannelError::InvalidConfig(format!(
                "Channel assistant binding references unresolved assistant identity: {assistant_id}"
            )));
        }

        Ok(default_agent_config())
    }

    /// Reads the model configuration for a platform from `client_preferences`.
    ///
    /// Returns `None` when no model is configured (common for ACP agents).
    pub async fn get_model_config(&self, platform: PluginType) -> Result<Option<ResolvedModelConfig>, ChannelError> {
        let key = model_key(platform);
        let prefs = self.pref_repo.get_by_keys(&[&key]).await?;

        let Some(pref) = prefs.into_iter().next() else {
            return Ok(None);
        };

        let parsed: serde_json::Value = serde_json::from_str(&pref.value).unwrap_or_default();

        let provider_id = parsed["id"].as_str().unwrap_or_default().to_owned();
        let use_model = parsed["use_model"].as_str().map(|s| s.to_owned());

        if provider_id.is_empty() && use_model.is_none() {
            return Ok(None);
        }

        debug!(platform = %platform, provider_id = %provider_id, use_model = ?use_model, "resolved channel model config");

        Ok(Some(ResolvedModelConfig {
            provider_id: provider_id.clone(),
            model: use_model.clone().unwrap_or_default(),
            use_model,
        }))
    }

    pub async fn get_platform_settings(
        &self,
        platform: PluginType,
    ) -> Result<ChannelPlatformSettingsResponse, ChannelError> {
        let key_agent = agent_key(platform);
        let key_model = model_key(platform);
        let prefs = self.pref_repo.get_by_keys(&[&key_agent, &key_model]).await?;

        let mut assistant = None;
        let mut default_model = None;

        for pref in prefs {
            if pref.key == key_agent {
                if let Some(parsed) = parse_channel_assistant_setting(&pref.value) {
                    assistant = Some(self.normalize_channel_assistant_setting_for_response(parsed).await?);
                }
            } else if pref.key == key_model {
                default_model = parse_channel_model_setting(&pref.value);
            }
        }

        if assistant.is_none() {
            assistant = self.resolve_default_channel_assistant_setting().await?;
        }

        Ok(ChannelPlatformSettingsResponse {
            platform: platform.to_string(),
            assistant,
            default_model,
        })
    }

    pub async fn get_assistant_setting(
        &self,
        platform: PluginType,
    ) -> Result<Option<ChannelAssistantSettingResponse>, ChannelError> {
        let key = agent_key(platform);
        let prefs = self.pref_repo.get_by_keys(&[&key]).await?;

        let Some(pref) = prefs.into_iter().next() else {
            return self.resolve_default_channel_assistant_setting().await;
        };

        let parsed = if let Some(assistant) = parse_channel_assistant_setting(&pref.value) {
            Some(self.normalize_channel_assistant_setting_for_response(assistant).await?)
        } else {
            None
        };

        Ok(parsed)
    }

    pub async fn set_assistant_setting(
        &self,
        platform: PluginType,
        assistant: &ChannelAssistantSettingRequest,
    ) -> Result<(), ChannelError> {
        if self
            .assistant_catalog
            .resolve_runtime_assistant(assistant.assistant_id.trim())
            .await?
            .is_none()
        {
            return Err(ChannelError::InvalidConfig(format!(
                "Channel assistant binding references unavailable assistant identity: {}",
                assistant.assistant_id.trim()
            )));
        }
        let normalized = normalize_channel_assistant_setting_for_write(assistant);
        let payload = serde_json::to_string(&normalized).map_err(ChannelError::Json)?;
        let key = agent_key(platform);
        self.pref_repo.upsert_batch(&[(&key, payload.as_str())]).await?;
        Ok(())
    }

    pub async fn set_model_setting(
        &self,
        platform: PluginType,
        model: &ChannelDefaultModelSetting,
    ) -> Result<(), ChannelError> {
        let payload = serde_json::to_string(model).map_err(ChannelError::Json)?;
        let key = model_key(platform);
        self.pref_repo.upsert_batch(&[(&key, payload.as_str())]).await?;
        Ok(())
    }

    async fn resolve_assistant_agent_config(
        &self,
        assistant_id: &str,
    ) -> Result<Option<ResolvedAgentConfig>, ChannelError> {
        Ok(self
            .assistant_catalog
            .resolve_runtime_assistant(assistant_id)
            .await?
            .map(|assistant| ResolvedAgentConfig {
                agent_type: assistant.agent_type,
                backend: assistant.backend,
            }))
    }

    async fn normalize_channel_assistant_setting_for_response(
        &self,
        assistant: ChannelAssistantSettingResponse,
    ) -> Result<ChannelAssistantSettingResponse, ChannelError> {
        let assistant_id = assistant
            .assistant_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .map(ToOwned::to_owned)
            .or_else(|| {
                assistant
                    .custom_agent_id
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
                    .map(ToOwned::to_owned)
            });

        let canonical_assistant_id = assistant_id;

        if let Some(assistant_id) = canonical_assistant_id {
            if self
                .assistant_catalog
                .resolve_runtime_assistant(&assistant_id)
                .await?
                .is_none()
            {
                return Err(ChannelError::InvalidConfig(format!(
                    "Channel assistant binding references unavailable assistant identity: {assistant_id}"
                )));
            }
            Ok(ChannelAssistantSettingResponse {
                assistant_id: Some(assistant_id),
                custom_agent_id: None,
                backend: None,
                agent_type: None,
                name: assistant.name,
            })
        } else {
            Ok(assistant)
        }
    }

    async fn resolve_default_channel_assistant_setting(
        &self,
    ) -> Result<Option<ChannelAssistantSettingResponse>, ChannelError> {
        let Some(assistant_id) = self.resolve_default_assistant_identity().await? else {
            return Ok(None);
        };

        Ok(Some(ChannelAssistantSettingResponse {
            assistant_id: Some(assistant_id),
            custom_agent_id: None,
            backend: None,
            agent_type: None,
            name: None,
        }))
    }

    async fn resolve_default_assistant_identity(&self) -> Result<Option<String>, ChannelError> {
        Ok(self
            .assistant_catalog
            .list_runtime_assistants()
            .await?
            .into_iter()
            .find(|assistant| {
                assistant.agent_type == DEFAULT_AGENT_TYPE || assistant.backend.as_deref() == Some(DEFAULT_AGENT_TYPE)
            })
            .map(|assistant| assistant.assistant_id))
    }
}

fn agent_key(platform: PluginType) -> String {
    format!("assistant.{platform}.agent")
}

fn model_key(platform: PluginType) -> String {
    format!("assistant.{platform}.defaultModel")
}

fn default_agent_config() -> ResolvedAgentConfig {
    ResolvedAgentConfig {
        agent_type: DEFAULT_AGENT_TYPE.to_owned(),
        backend: None,
    }
}

fn parse_channel_assistant_setting(value: &str) -> Option<ChannelAssistantSettingResponse> {
    let parsed: serde_json::Value = serde_json::from_str(value).ok()?;
    let assistant_id = parsed["assistant_id"].as_str()?.trim();
    if assistant_id.is_empty() {
        return None;
    }

    Some(ChannelAssistantSettingResponse {
        assistant_id: Some(assistant_id.to_owned()),
        custom_agent_id: None,
        backend: None,
        agent_type: None,
        name: parsed["name"].as_str().map(|s| s.to_owned()),
    })
}

fn normalize_channel_assistant_setting_for_write(
    assistant: &ChannelAssistantSettingRequest,
) -> ChannelAssistantSettingResponse {
    ChannelAssistantSettingResponse {
        assistant_id: Some(assistant.assistant_id.trim().to_owned()),
        custom_agent_id: None,
        backend: None,
        agent_type: None,
        name: assistant.name.clone(),
    }
}

fn parse_channel_model_setting(value: &str) -> Option<ChannelDefaultModelSetting> {
    let parsed: serde_json::Value = serde_json::from_str(value).ok()?;
    let id = parsed["id"].as_str()?.to_owned();
    let use_model = parsed["use_model"].as_str()?.to_owned();
    Some(ChannelDefaultModelSetting { id, use_model })
}

/// Builds a `ProviderWithModel` from the resolved config, or returns
/// the empty default when no model is configured.
pub fn resolved_model_to_provider(model: Option<&ResolvedModelConfig>) -> ProviderWithModel {
    match model {
        Some(m) => ProviderWithModel {
            provider_id: m.provider_id.clone(),
            model: m.model.clone(),
            use_model: m.use_model.clone(),
        },
        None => ProviderWithModel {
            provider_id: String::new(),
            model: String::new(),
            use_model: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tjuaeui_db::DbError;
    use tjuaeui_db::models::ClientPreference;

    struct MockPrefRepo {
        data: Mutex<Vec<(String, String)>>,
    }

    impl MockPrefRepo {
        fn new() -> Self {
            Self {
                data: Mutex::new(Vec::new()),
            }
        }

        fn with_data(entries: Vec<(&str, &str)>) -> Self {
            Self {
                data: Mutex::new(entries.into_iter().map(|(k, v)| (k.to_owned(), v.to_owned())).collect()),
            }
        }
    }

    #[async_trait::async_trait]
    impl IClientPreferenceRepository for MockPrefRepo {
        async fn get_all(&self) -> Result<Vec<ClientPreference>, DbError> {
            let data = self.data.lock().unwrap();
            Ok(data
                .iter()
                .map(|(k, v)| ClientPreference {
                    key: k.clone(),
                    value: v.clone(),
                    updated_at: 0,
                })
                .collect())
        }

        async fn get_by_keys(&self, keys: &[&str]) -> Result<Vec<ClientPreference>, DbError> {
            let data = self.data.lock().unwrap();
            Ok(data
                .iter()
                .filter(|(k, _)| keys.contains(&k.as_str()))
                .map(|(k, v)| ClientPreference {
                    key: k.clone(),
                    value: v.clone(),
                    updated_at: 0,
                })
                .collect())
        }

        async fn upsert_batch(&self, entries: &[(&str, &str)]) -> Result<(), DbError> {
            let mut data = self.data.lock().unwrap();
            for (key, value) in entries {
                if let Some(existing) = data.iter_mut().find(|(k, _)| k == key) {
                    existing.1 = value.to_string();
                } else {
                    data.push((key.to_string(), value.to_string()));
                }
            }
            Ok(())
        }

        async fn delete_keys(&self, keys: &[&str]) -> Result<(), DbError> {
            let mut data = self.data.lock().unwrap();
            data.retain(|(k, _)| !keys.contains(&k.as_str()));
            Ok(())
        }
    }

    struct MockAssistantCatalog {
        rows: Vec<ChannelAssistantCatalogEntry>,
    }

    #[async_trait::async_trait]
    impl ChannelAssistantCatalogPort for MockAssistantCatalog {
        async fn list_runtime_assistants(&self) -> Result<Vec<ChannelAssistantCatalogEntry>, ChannelError> {
            Ok(self.rows.clone())
        }
    }

    fn catalog(rows: Vec<ChannelAssistantCatalogEntry>) -> Arc<dyn ChannelAssistantCatalogPort> {
        Arc::new(MockAssistantCatalog { rows })
    }

    fn empty_catalog() -> Arc<dyn ChannelAssistantCatalogPort> {
        catalog(Vec::new())
    }

    fn make_assistant(assistant_id: &str, agent_type: &str, backend: Option<&str>) -> ChannelAssistantCatalogEntry {
        ChannelAssistantCatalogEntry {
            assistant_id: assistant_id.to_owned(),
            name: assistant_id.to_owned(),
            agent_type: agent_type.to_owned(),
            backend: backend.map(ToOwned::to_owned),
        }
    }

    // ── get_agent_config ──────────────────────────────────────────────

    #[tokio::test]
    async fn agent_config_returns_default_when_no_pref() {
        let repo = Arc::new(MockPrefRepo::new());
        let svc = ChannelSettingsService::new(repo, empty_catalog());

        let config = svc.get_agent_config(PluginType::Telegram).await.unwrap();
        assert_eq!(config.agent_type, "tjuaecli");
        assert!(config.backend.is_none());
    }

    #[tokio::test]
    async fn agent_config_ignores_removed_backend_only_payload() {
        let repo = Arc::new(MockPrefRepo::with_data(vec![(
            "assistant.telegram.agent",
            r#"{"backend":"codex","name":"Codex"}"#,
        )]));
        let svc = ChannelSettingsService::new(repo, empty_catalog());

        let config = svc.get_agent_config(PluginType::Telegram).await.unwrap();
        assert_eq!(config.agent_type, "tjuaecli");
        assert!(config.backend.is_none());
    }

    #[tokio::test]
    async fn agent_config_resolves_backend_from_assistant_identity() {
        let repo = Arc::new(MockPrefRepo::with_data(vec![(
            "assistant.telegram.agent",
            r#"{"assistant_id":"bare-claude","name":"Claude"}"#,
        )]));
        let svc = ChannelSettingsService::new(
            repo,
            catalog(vec![make_assistant("bare-claude", "acp", Some("claude"))]),
        );

        let config = svc.get_agent_config(PluginType::Telegram).await.unwrap();
        assert_eq!(config.agent_type, "acp");
        assert_eq!(config.backend.as_deref(), Some("claude"));
    }

    #[tokio::test]
    async fn agent_config_uses_catalog_runtime_backend_for_assistant_identity() {
        let repo = Arc::new(MockPrefRepo::with_data(vec![(
            "assistant.telegram.agent",
            r#"{"assistant_id":"bare-claude","name":"Claude"}"#,
        )]));
        let svc = ChannelSettingsService::new(repo, catalog(vec![make_assistant("bare-claude", "acp", Some("codex"))]));

        let config = svc.get_agent_config(PluginType::Telegram).await.unwrap();
        assert_eq!(config.agent_type, "acp");
        assert_eq!(config.backend.as_deref(), Some("codex"));
    }

    #[tokio::test]
    async fn agent_config_errors_when_assistant_identity_cannot_resolve() {
        let repo = Arc::new(MockPrefRepo::with_data(vec![(
            "assistant.telegram.agent",
            r#"{"assistant_id":"missing-assistant","name":"Missing"}"#,
        )]));
        let svc = ChannelSettingsService::new(repo, empty_catalog());

        let err = svc.get_agent_config(PluginType::Telegram).await.unwrap_err();
        assert!(matches!(err, ChannelError::InvalidConfig(_)));
        assert!(
            err.to_string().contains("missing-assistant"),
            "error should name the unresolved assistant identity"
        );
    }

    // ── get_model_config ──────────────────────────────────────────────

    #[tokio::test]
    async fn model_config_returns_none_when_no_pref() {
        let repo = Arc::new(MockPrefRepo::new());
        let svc = ChannelSettingsService::new(repo, empty_catalog());

        let config = svc.get_model_config(PluginType::Telegram).await.unwrap();
        assert!(config.is_none());
    }

    #[tokio::test]
    async fn model_config_reads_from_preferences() {
        let repo = Arc::new(MockPrefRepo::with_data(vec![(
            "assistant.weixin.defaultModel",
            r#"{"id":"490fdb4e","use_model":"global.anthropic.claude-opus-4-6-v1"}"#,
        )]));
        let svc = ChannelSettingsService::new(repo, empty_catalog());

        let config = svc.get_model_config(PluginType::Weixin).await.unwrap().unwrap();
        assert_eq!(config.provider_id, "490fdb4e");
        assert_eq!(config.use_model.as_deref(), Some("global.anthropic.claude-opus-4-6-v1"));
    }

    #[tokio::test]
    async fn model_config_returns_none_for_empty_values() {
        let repo = Arc::new(MockPrefRepo::with_data(vec![(
            "assistant.telegram.defaultModel",
            r#"{"id":"","use_model":null}"#,
        )]));
        let svc = ChannelSettingsService::new(repo, empty_catalog());

        let config = svc.get_model_config(PluginType::Telegram).await.unwrap();
        assert!(config.is_none());
    }

    #[tokio::test]
    async fn set_assistant_setting_persists_assistant_only_payload() {
        let repo = Arc::new(MockPrefRepo::new());
        let svc = ChannelSettingsService::new(
            repo.clone(),
            catalog(vec![make_assistant("assistant-1", "acp", Some("claude"))]),
        );

        svc.set_assistant_setting(
            PluginType::Telegram,
            &ChannelAssistantSettingRequest {
                assistant_id: "assistant-1".into(),
                name: Some("Claude".into()),
            },
        )
        .await
        .unwrap();

        let stored = repo.get_by_keys(&["assistant.telegram.agent"]).await.unwrap();
        let payload = serde_json::from_str::<serde_json::Value>(&stored[0].value).unwrap();

        assert_eq!(payload["assistant_id"], "assistant-1");
        assert_eq!(payload["name"], "Claude");
        assert!(payload.get("custom_agent_id").is_none());
        assert!(payload.get("backend").is_none());
        assert!(payload.get("agent_type").is_none());
    }

    #[tokio::test]
    async fn set_assistant_setting_trims_assistant_id_before_persisting() {
        let repo = Arc::new(MockPrefRepo::new());
        let svc = ChannelSettingsService::new(
            repo.clone(),
            catalog(vec![make_assistant("legacy-custom", "acp", Some("codex"))]),
        );

        svc.set_assistant_setting(
            PluginType::Lark,
            &ChannelAssistantSettingRequest {
                assistant_id: "  legacy-custom  ".into(),
                name: Some("Codex".into()),
            },
        )
        .await
        .unwrap();

        let stored = repo.get_by_keys(&["assistant.lark.agent"]).await.unwrap();
        let payload = serde_json::from_str::<serde_json::Value>(&stored[0].value).unwrap();

        assert_eq!(payload["assistant_id"], "legacy-custom");
        assert_eq!(payload["name"], "Codex");
        assert!(payload.get("custom_agent_id").is_none());
        assert!(payload.get("backend").is_none());
        assert!(payload.get("agent_type").is_none());
    }

    #[tokio::test]
    async fn get_assistant_setting_defaults_to_runtime_tjuae_cli_assistant() {
        let repo = Arc::new(MockPrefRepo::new());
        let svc = ChannelSettingsService::new(
            repo,
            catalog(vec![
                make_assistant("bare-claude", "acp", Some("claude")),
                make_assistant("bare-tjuaecli", "tjuaecli", None),
            ]),
        );

        let setting = svc.get_assistant_setting(PluginType::Telegram).await.unwrap().unwrap();

        assert_eq!(setting.assistant_id.as_deref(), Some("bare-tjuaecli"));
        assert!(setting.custom_agent_id.is_none());
        assert!(setting.backend.is_none());
        assert!(setting.agent_type.is_none());
        assert!(setting.name.is_none());
    }

    #[tokio::test]
    async fn get_assistant_setting_does_not_restore_removed_legacy_payload() {
        let repo = Arc::new(MockPrefRepo::with_data(vec![(
            "assistant.lark.agent",
            r#"{"backend":"codex","name":"Codex"}"#,
        )]));
        let svc = ChannelSettingsService::new(repo, empty_catalog());

        assert!(svc.get_assistant_setting(PluginType::Lark).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn get_platform_settings_defaults_to_runtime_tjuae_cli_assistant() {
        let repo = Arc::new(MockPrefRepo::new());
        let svc = ChannelSettingsService::new(repo, catalog(vec![make_assistant("bare-tjuaecli", "tjuaecli", None)]));

        let settings = svc.get_platform_settings(PluginType::Telegram).await.unwrap();
        let assistant = settings.assistant.expect("assistant settings");

        assert_eq!(assistant.assistant_id.as_deref(), Some("bare-tjuaecli"));
        assert!(assistant.custom_agent_id.is_none());
        assert!(assistant.backend.is_none());
        assert!(assistant.agent_type.is_none());
    }

    // ── resolved_model_to_provider ────────────────────────────────────

    #[test]
    fn resolved_model_converts_to_provider() {
        let model = ResolvedModelConfig {
            provider_id: "abc".into(),
            model: "gpt-5".into(),
            use_model: Some("gpt-5".into()),
        };
        let p = resolved_model_to_provider(Some(&model));
        assert_eq!(p.provider_id, "abc");
        assert_eq!(p.model, "gpt-5");
        assert_eq!(p.use_model.as_deref(), Some("gpt-5"));
    }

    #[test]
    fn none_model_produces_empty_provider() {
        let p = resolved_model_to_provider(None);
        assert!(p.provider_id.is_empty());
        assert!(p.model.is_empty());
        assert!(p.use_model.is_none());
    }
}
