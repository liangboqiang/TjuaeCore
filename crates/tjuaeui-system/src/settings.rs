use std::sync::Arc;

use tjuaeui_api_types::{
    NetworkProxyMode, NetworkProxySettings, NetworkProxySource, NetworkProxyState, NetworkProxyStatusResponse,
    NetworkProxyWarning, SystemSettingsResponse, UpdateSettingsRequest,
};
use tjuaeui_db::{ISettingsRepository, UpsertSystemSettingsParams};

use crate::error::SystemError;

/// Supported BCP 47 language codes.
const SUPPORTED_LANGUAGES: &[&str] = &[
    "en-US", "zh-CN", "zh-TW", "ja-JP", "ko-KR", "fr-FR", "de-DE", "es-ES", "pt-BR", "ru-RU", "ar-SA", "it-IT",
    "nl-NL", "pl-PL", "tr-TR", "vi-VN", "th-TH", "id-ID",
];

/// Business logic for system settings (language, notifications, etc.).
#[derive(Clone)]
pub struct SettingsService {
    repo: Arc<dyn ISettingsRepository>,
}

impl SettingsService {
    pub fn new(repo: Arc<dyn ISettingsRepository>) -> Self {
        Self { repo }
    }

    /// Get current system settings, falling back to defaults if not yet persisted.
    pub async fn get_settings(&self) -> Result<SystemSettingsResponse, SystemError> {
        let row = self
            .repo
            .get_settings()
            .await
            .map_err(|e| SystemError::Internal(format!("Failed to get settings: {e}")))?;

        Ok(
            row.map_or_else(SystemSettingsResponse::default, |s| SystemSettingsResponse {
                language: s.language,
                notification_enabled: s.notification_enabled,
                cron_notification_enabled: s.cron_notification_enabled,
                command_queue_enabled: s.command_queue_enabled,
                save_upload_to_workspace: s.save_upload_to_workspace,
                network_proxy: NetworkProxySettings {
                    mode: network_proxy_mode_from_db(&s.network_proxy_mode),
                    proxy_url: s.network_proxy_url,
                    no_proxy: s.network_proxy_no_proxy,
                },
            }),
        )
    }

    /// 在应用启动早期将持久化配置安装为统一运行时策略。
    pub async fn initialize_network_proxy(&self) -> Result<NetworkProxyStatusResponse, SystemError> {
        let settings = self.get_settings().await?;
        apply_network_proxy(&settings.network_proxy).map_err(SystemError::Internal)
    }

    /// 返回当前实际解析到的代理状态，不执行网络探测。
    pub fn network_proxy_status(&self) -> NetworkProxyStatusResponse {
        proxy_status_from_runtime(tjuaeui_runtime::network_proxy_status())
    }

    /// Partially update system settings. Only fields present in the request are changed.
    pub async fn update_settings(&self, req: UpdateSettingsRequest) -> Result<SystemSettingsResponse, SystemError> {
        if let Some(ref lang) = req.language {
            validate_language(lang)?;
        }

        // Merge with current settings (or defaults)
        let current = self.get_settings().await?;

        let language = req.language.unwrap_or(current.language);
        let notification_enabled = req.notification_enabled.unwrap_or(current.notification_enabled);
        let cron_notification_enabled = req
            .cron_notification_enabled
            .unwrap_or(current.cron_notification_enabled);
        let command_queue_enabled = req.command_queue_enabled.unwrap_or(current.command_queue_enabled);
        let save_upload_to_workspace = req.save_upload_to_workspace.unwrap_or(current.save_upload_to_workspace);
        let network_proxy = validate_network_proxy(req.network_proxy.unwrap_or(current.network_proxy))?;
        let network_proxy_mode = network_proxy_mode_to_db(network_proxy.mode);

        let row = self
            .repo
            .upsert_settings(UpsertSystemSettingsParams {
                language: &language,
                notification_enabled,
                cron_notification_enabled,
                command_queue_enabled,
                save_upload_to_workspace,
                network_proxy_mode,
                network_proxy_url: network_proxy.proxy_url.as_deref(),
                network_proxy_no_proxy: &network_proxy.no_proxy,
            })
            .await
            .map_err(|e| SystemError::Internal(format!("Failed to update settings: {e}")))?;

        apply_network_proxy(&network_proxy).map_err(SystemError::Internal)?;

        Ok(SystemSettingsResponse {
            language: row.language,
            notification_enabled: row.notification_enabled,
            cron_notification_enabled: row.cron_notification_enabled,
            command_queue_enabled: row.command_queue_enabled,
            save_upload_to_workspace: row.save_upload_to_workspace,
            network_proxy,
        })
    }
}

fn validate_network_proxy(settings: NetworkProxySettings) -> Result<NetworkProxySettings, SystemError> {
    let config = proxy_config_to_runtime(&settings);
    let normalized = tjuaeui_runtime::validate_network_proxy_config(config)
        .map_err(|reason| SystemError::BadRequest(format!("代理设置无效：{reason}")))?;
    Ok(proxy_settings_from_runtime(normalized))
}

fn apply_network_proxy(settings: &NetworkProxySettings) -> Result<NetworkProxyStatusResponse, String> {
    tjuaeui_runtime::set_network_proxy_config(proxy_config_to_runtime(settings)).map(proxy_status_from_runtime)
}

fn proxy_config_to_runtime(settings: &NetworkProxySettings) -> tjuaeui_runtime::NetworkProxyConfig {
    tjuaeui_runtime::NetworkProxyConfig {
        mode: match settings.mode {
            NetworkProxyMode::FollowSystem => tjuaeui_runtime::NetworkProxyMode::FollowSystem,
            NetworkProxyMode::Manual => tjuaeui_runtime::NetworkProxyMode::Manual,
            NetworkProxyMode::Disabled => tjuaeui_runtime::NetworkProxyMode::Disabled,
        },
        proxy_url: settings.proxy_url.clone(),
        no_proxy: settings.no_proxy.clone(),
    }
}

fn proxy_settings_from_runtime(config: tjuaeui_runtime::NetworkProxyConfig) -> NetworkProxySettings {
    NetworkProxySettings {
        mode: match config.mode {
            tjuaeui_runtime::NetworkProxyMode::FollowSystem => NetworkProxyMode::FollowSystem,
            tjuaeui_runtime::NetworkProxyMode::Manual => NetworkProxyMode::Manual,
            tjuaeui_runtime::NetworkProxyMode::Disabled => NetworkProxyMode::Disabled,
        },
        proxy_url: config.proxy_url,
        no_proxy: config.no_proxy,
    }
}

fn proxy_status_from_runtime(status: tjuaeui_runtime::NetworkProxyStatus) -> NetworkProxyStatusResponse {
    NetworkProxyStatusResponse {
        mode: match status.mode {
            tjuaeui_runtime::NetworkProxyMode::FollowSystem => NetworkProxyMode::FollowSystem,
            tjuaeui_runtime::NetworkProxyMode::Manual => NetworkProxyMode::Manual,
            tjuaeui_runtime::NetworkProxyMode::Disabled => NetworkProxyMode::Disabled,
        },
        state: match status.state {
            tjuaeui_runtime::NetworkProxyState::Active => NetworkProxyState::Active,
            tjuaeui_runtime::NetworkProxyState::Direct => NetworkProxyState::Direct,
        },
        source: match status.source {
            tjuaeui_runtime::NetworkProxySource::Manual => NetworkProxySource::Manual,
            tjuaeui_runtime::NetworkProxySource::Environment => NetworkProxySource::Environment,
            tjuaeui_runtime::NetworkProxySource::WindowsSystem => NetworkProxySource::WindowsSystem,
            tjuaeui_runtime::NetworkProxySource::Disabled => NetworkProxySource::Disabled,
            tjuaeui_runtime::NetworkProxySource::None => NetworkProxySource::None,
        },
        proxy_url: status.proxy_url,
        no_proxy: status.no_proxy,
        warning: status.warning.map(|warning| match warning {
            tjuaeui_runtime::NetworkProxyWarning::PacUnsupported => NetworkProxyWarning::PacUnsupported,
            tjuaeui_runtime::NetworkProxyWarning::InvalidSystemProxy => NetworkProxyWarning::InvalidSystemProxy,
        }),
    }
}

fn network_proxy_mode_from_db(value: &str) -> NetworkProxyMode {
    match value {
        "manual" => NetworkProxyMode::Manual,
        "disabled" => NetworkProxyMode::Disabled,
        _ => NetworkProxyMode::FollowSystem,
    }
}

const fn network_proxy_mode_to_db(mode: NetworkProxyMode) -> &'static str {
    match mode {
        NetworkProxyMode::FollowSystem => "follow_system",
        NetworkProxyMode::Manual => "manual",
        NetworkProxyMode::Disabled => "disabled",
    }
}

fn validate_language(lang: &str) -> Result<(), SystemError> {
    if SUPPORTED_LANGUAGES.contains(&lang) {
        Ok(())
    } else {
        Err(SystemError::BadRequest(format!("不支持的语言代码：'{lang}'")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tjuaeui_db::{SqliteSettingsRepository, init_database_memory};

    async fn setup() -> SettingsService {
        let db = init_database_memory().await.unwrap();
        let repo = Arc::new(SqliteSettingsRepository::new(db.pool().clone()));
        // Leak the db handle so the pool stays alive for the test
        std::mem::forget(db);
        SettingsService::new(repo)
    }

    #[test]
    fn validate_language_accepts_supported() {
        assert!(validate_language("en-US").is_ok());
        assert!(validate_language("zh-CN").is_ok());
        assert!(validate_language("ja-JP").is_ok());
    }

    #[test]
    fn validate_language_rejects_unsupported() {
        assert!(validate_language("invalid").is_err());
        assert!(validate_language("").is_err());
        assert!(validate_language("xx-YY").is_err());
    }

    #[tokio::test]
    async fn get_settings_returns_defaults_when_empty() {
        let svc = setup().await;
        let settings = svc.get_settings().await.unwrap();
        assert_eq!(settings, SystemSettingsResponse::default());
    }

    #[tokio::test]
    async fn update_single_field() {
        let svc = setup().await;
        let req = UpdateSettingsRequest {
            language: Some("zh-CN".into()),
            ..Default::default()
        };
        let result = svc.update_settings(req).await.unwrap();
        assert_eq!(result.language, "zh-CN");
        // Other fields stay at defaults
        assert!(result.notification_enabled);
        assert!(!result.cron_notification_enabled);
    }

    #[tokio::test]
    async fn update_multiple_fields() {
        let svc = setup().await;
        let req = UpdateSettingsRequest {
            notification_enabled: Some(false),
            command_queue_enabled: Some(true),
            ..Default::default()
        };
        let result = svc.update_settings(req).await.unwrap();
        assert!(!result.notification_enabled);
        assert!(result.command_queue_enabled);
        assert_eq!(result.language, "en-US");
    }

    #[tokio::test]
    async fn update_empty_request_returns_current() {
        let svc = setup().await;
        let result = svc.update_settings(UpdateSettingsRequest::default()).await.unwrap();
        assert_eq!(result, SystemSettingsResponse::default());
    }

    #[tokio::test]
    async fn update_invalid_language_rejected() {
        let svc = setup().await;
        let req = UpdateSettingsRequest {
            language: Some("invalid-lang".into()),
            ..Default::default()
        };
        let err = svc.update_settings(req).await.unwrap_err();
        assert!(matches!(err, SystemError::BadRequest(_)));
    }

    #[tokio::test]
    async fn update_then_get_reflects_changes() {
        let svc = setup().await;
        svc.update_settings(UpdateSettingsRequest {
            language: Some("ja-JP".into()),
            save_upload_to_workspace: Some(true),
            ..Default::default()
        })
        .await
        .unwrap();

        let settings = svc.get_settings().await.unwrap();
        assert_eq!(settings.language, "ja-JP");
        assert!(settings.save_upload_to_workspace);
    }

    #[tokio::test]
    async fn manual_proxy_is_normalized_and_persisted() {
        let svc = setup().await;
        let result = svc
            .update_settings(UpdateSettingsRequest {
                network_proxy: Some(NetworkProxySettings {
                    mode: NetworkProxyMode::Manual,
                    proxy_url: Some("127.0.0.1:7897".to_owned()),
                    no_proxy: "internal.example".to_owned(),
                }),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(result.network_proxy.mode, NetworkProxyMode::Manual);
        assert_eq!(result.network_proxy.proxy_url.as_deref(), Some("http://127.0.0.1:7897"));
        assert!(result.network_proxy.no_proxy.contains("localhost"));
        assert!(result.network_proxy.no_proxy.contains("internal.example"));
        tjuaeui_runtime::set_network_proxy_config(tjuaeui_runtime::NetworkProxyConfig::default()).unwrap();
    }

    #[tokio::test]
    async fn invalid_manual_proxy_is_rejected_without_persisting() {
        let svc = setup().await;
        let error = svc
            .update_settings(UpdateSettingsRequest {
                network_proxy: Some(NetworkProxySettings {
                    mode: NetworkProxyMode::Manual,
                    proxy_url: Some("socks5://127.0.0.1:1080".to_owned()),
                    no_proxy: String::new(),
                }),
                ..Default::default()
            })
            .await
            .unwrap_err();

        assert!(matches!(error, SystemError::BadRequest(_)));
        assert_eq!(svc.get_settings().await.unwrap(), SystemSettingsResponse::default());
    }
}
