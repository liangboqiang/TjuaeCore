//! Custom Agent business logic.
//!
//! Extends `AgentService` with CRUD for `agent_source = 'custom'` rows
//! in the `agent_metadata` catalog. Mirrors the frontend PRD
//! F-CAGENT-04 / -05 / -12 / -13 / -14 (create, edit, save, delete,
//! toggle enable).
//!
//! Test-on-save: create / update run `try_connect_custom_agent`
//! before hitting the DB. Failures become `AgentError::BadRequest` with
//! a prefixed marker (`cli_not_found:` / `acp_init_failed:`) that the
//! frontend maps back to the same three Alert states it shows for the
//! manual "Test connection" button.

use std::collections::HashMap;

use crate::error::AgentError;
use tjuaeui_api_types::{
    AgentEnvEntry, AgentMetadata, CustomAgentProtocol, CustomAgentUpsertRequest, TryConnectA2aAgentRequest,
    TryConnectA2aAgentResponse, TryConnectCustomAgentRequest, TryConnectCustomAgentResponse,
};
use tjuaeui_common::generate_short_id;
use tjuaeui_db::UpsertAgentMetadataParams;
use tracing::warn;

use super::AgentService;
use crate::protocol::custom_agent_probe::try_connect_custom_agent as probe;
use crate::runtime_status::custom_agent_runtime_reporter;

const CUSTOM_SORT_ORDER_DEFAULT: i64 = 1500;

impl AgentService {
    /// Public accessor for the probe — powers both
    /// `POST /api/agents/custom/try-connect` and the test-on-save path
    /// below.
    pub async fn try_connect_custom_agent(
        &self,
        req: TryConnectCustomAgentRequest,
    ) -> Result<TryConnectCustomAgentResponse, AgentError> {
        if req.command.trim().is_empty() {
            return Err(AgentError::bad_request("command 不能为空"));
        }
        let reporter = req
            .runtime_scope_id
            .as_ref()
            .map(|scope_id| custom_agent_runtime_reporter(self.broadcaster().clone(), scope_id.clone()));
        Ok(probe(&req.command, &req.acp_args, &req.env, reporter.as_deref()).await)
    }

    pub async fn try_connect_a2a_agent(
        &self,
        req: TryConnectA2aAgentRequest,
    ) -> Result<TryConnectA2aAgentResponse, AgentError> {
        crate::protocol::a2a_probe::discover(&req)
            .await
            .map_err(AgentError::bad_request)
    }

    pub async fn create_custom_agent(&self, req: CustomAgentUpsertRequest) -> Result<AgentMetadata, AgentError> {
        validate_upsert(&req)?;
        probe_or_reject(&req).await?;

        let id = generate_short_id();
        self.upsert_custom_row(&id, &req, /* keep_enabled = */ true).await
    }

    pub async fn update_custom_agent(
        &self,
        id: &str,
        req: CustomAgentUpsertRequest,
    ) -> Result<AgentMetadata, AgentError> {
        validate_upsert(&req)?;
        let existing = self
            .registry()
            .repo_handle()
            .get(id)
            .await
            .map_err(|e| AgentError::internal(format!("读取 Agent 仓储失败：{e}")))?
            .ok_or_else(|| AgentError::not_found(format!("找不到 Agent“{id}”")))?;
        if existing.agent_source != "custom" {
            return Err(AgentError::forbidden(
                "Only custom agents can be edited via this endpoint",
            ));
        }
        let mut req = req;
        if req.protocol == CustomAgentProtocol::A2a && req.auth_type.is_some() && req.auth_token.is_none() {
            req.auth_token = persisted_a2a_auth_token(existing.env.as_deref());
        }
        probe_or_reject(&req).await?;

        let keep_enabled = existing.enabled;
        self.upsert_custom_row(id, &req, keep_enabled).await
    }

    pub async fn delete_custom_agent(&self, id: &str) -> Result<(), AgentError> {
        let existing = self
            .registry()
            .repo_handle()
            .get(id)
            .await
            .map_err(|e| AgentError::internal(format!("读取 Agent 仓储失败：{e}")))?
            .ok_or_else(|| AgentError::not_found(format!("找不到 Agent“{id}”")))?;
        if existing.agent_source != "custom" {
            return Err(AgentError::forbidden(
                "Only custom agents can be deleted via this endpoint",
            ));
        }
        let removed = self
            .registry()
            .repo_handle()
            .delete(id)
            .await
            .map_err(|e| AgentError::internal(format!("从 Agent 仓储删除失败：{e}")))?;
        if !removed {
            return Err(AgentError::not_found(format!("找不到 Agent“{id}”")));
        }
        if let Err(err) = self.registry().reload_one(id).await {
            warn!(agent_id = %id, error = %err, "registry reload failed after delete_custom_agent");
        }
        Ok(())
    }

    pub async fn set_agent_enabled(&self, id: &str, enabled: bool) -> Result<AgentMetadata, AgentError> {
        let updated = self
            .registry()
            .repo_handle()
            .set_enabled(id, enabled)
            .await
            .map_err(|e| AgentError::internal(format!("更新 Agent 启用状态失败：{e}")))?;
        if !updated {
            return Err(AgentError::not_found(format!("找不到 Agent“{id}”")));
        }
        if let Err(err) = self.registry().reload_one(id).await {
            warn!(agent_id = %id, error = %err, "registry reload failed after set_agent_enabled");
        }
        self.registry()
            .get(id)
            .await
            .ok_or_else(|| AgentError::internal(format!("切换启用状态后找不到 Agent“{id}”")))
    }

    async fn upsert_custom_row(
        &self,
        id: &str,
        req: &CustomAgentUpsertRequest,
        enabled: bool,
    ) -> Result<AgentMetadata, AgentError> {
        let advanced = req.advanced.clone().unwrap_or_default();

        let args_json =
            serde_json::to_string(&req.args).map_err(|e| AgentError::internal(format!("编码 args 失败：{e}")))?;
        let mut persisted_env = req.env.clone();
        if req.protocol == CustomAgentProtocol::A2a {
            if let Some(auth_type) = req.auth_type.as_deref() {
                persisted_env.push(AgentEnvEntry {
                    name: "TJUAE_A2A_AUTH_TYPE".to_owned(),
                    value: auth_type.to_owned(),
                    description: None,
                });
            }
            if let Some(auth_token) = req.auth_token.as_deref().filter(|value| !value.is_empty()) {
                persisted_env.push(AgentEnvEntry {
                    name: "TJUAE_A2A_AUTH_TOKEN".to_owned(),
                    value: auth_token.to_owned(),
                    description: None,
                });
            }
        }
        let env_json =
            serde_json::to_string(&persisted_env).map_err(|e| AgentError::internal(format!("编码 env 失败：{e}")))?;
        let native_skills_dirs_json = advanced
            .native_skills_dirs
            .as_ref()
            .map(|v| {
                serde_json::to_string(v).map_err(|e| AgentError::internal(format!("编码 native_skills_dirs 失败：{e}")))
            })
            .transpose()?;
        let behavior_policy_json = advanced
            .behavior_policy
            .as_ref()
            .map(|v| {
                serde_json::to_string(v).map_err(|e| AgentError::internal(format!("编码 behavior_policy 失败：{e}")))
            })
            .transpose()?;

        let source_info = match req.protocol {
            CustomAgentProtocol::Acp => serde_json::json!({
                "binary_name": first_token(&req.command),
            }),
            CustomAgentProtocol::A2a => serde_json::json!({
                "endpoint": req.endpoint,
                "auth_type": req.auth_type,
                "allow_insecure": req.allow_insecure,
            }),
        };
        let source_info_json = source_info.to_string();

        let params = UpsertAgentMetadataParams {
            id,
            icon: req.icon.as_deref(),
            name: req.name.trim(),
            name_i18n: None,
            description: advanced.description.as_deref(),
            description_i18n: None,
            backend: None,
            agent_type: match req.protocol {
                CustomAgentProtocol::Acp => "acp",
                CustomAgentProtocol::A2a => "a2a",
            },
            agent_source: "custom",
            agent_source_info: Some(&source_info_json),
            enabled,
            command: match req.protocol {
                CustomAgentProtocol::Acp => Some(req.command.trim()),
                CustomAgentProtocol::A2a => None,
            },
            args: Some(&args_json),
            env: Some(&env_json),
            native_skills_dirs: native_skills_dirs_json.as_deref(),
            behavior_policy: behavior_policy_json.as_deref(),
            yolo_id: advanced.yolo_id.as_deref(),
            agent_capabilities: None,
            auth_methods: None,
            config_options: None,
            available_modes: None,
            available_models: None,
            available_commands: None,
            sort_order: CUSTOM_SORT_ORDER_DEFAULT,
        };

        self.registry()
            .repo_handle()
            .upsert(&params)
            .await
            .map_err(|e| AgentError::internal(format!("写入 Agent 仓储失败：{e}")))?;

        self.registry()
            .reload_one(id)
            .await
            .map_err(|e| AgentError::internal(format!("重新加载 Agent 注册表失败：{e}")))?;

        self.registry()
            .get(id)
            .await
            .ok_or_else(|| AgentError::internal(format!("写入后找不到 Agent“{id}”")))
    }
}

fn validate_upsert(req: &CustomAgentUpsertRequest) -> Result<(), AgentError> {
    if req.name.trim().is_empty() {
        return Err(AgentError::bad_request("name 不能为空"));
    }
    match req.protocol {
        CustomAgentProtocol::Acp if req.command.trim().is_empty() => {
            return Err(AgentError::bad_request("command 不能为空"));
        }
        CustomAgentProtocol::A2a if req.endpoint.as_deref().is_none_or(|value| value.trim().is_empty()) => {
            return Err(AgentError::bad_request("A2A endpoint 不能为空"));
        }
        _ => {}
    }
    Ok(())
}

async fn probe_or_reject(req: &CustomAgentUpsertRequest) -> Result<(), AgentError> {
    // 仅供测试绕过：真实探测需要启动子进程并依赖 PATH 中可用的 ACP CLI。
    // 此分支只在测试或 `test-support` 功能下编译，生产构建无法通过环境变量跳过探测。
    #[cfg(any(test, feature = "test-support"))]
    if std::env::var("TJUAE_BYPASS_PROBE").is_ok() {
        tracing::warn!("TJUAE_BYPASS_PROBE set — skipping custom agent probe. Test-only.");
        return Ok(());
    }

    if req.protocol == CustomAgentProtocol::A2a {
        let probe_request = TryConnectA2aAgentRequest {
            endpoint: req.endpoint.clone().unwrap_or_default(),
            auth_type: req.auth_type.clone(),
            auth_token: req.auth_token.clone(),
            allow_insecure: req.allow_insecure,
        };
        return crate::protocol::a2a_probe::discover(&probe_request)
            .await
            .map(|_| ())
            .map_err(|error| AgentError::bad_request(format!("a2a_discovery_failed: {error}")));
    }

    let env_map: HashMap<String, String> = req.env.iter().map(|e| (e.name.clone(), e.value.clone())).collect();
    match probe(&req.command, &req.args, &env_map, None).await {
        TryConnectCustomAgentResponse::Success => Ok(()),
        // 可连接但未授权仍是有效 Agent；允许保存并标记为需登录，之后可通过连接测试确认恢复。
        TryConnectCustomAgentResponse::FailAuth { error } => {
            tracing::info!(%error, "custom agent reachable but requires auth; accepting save");
            Ok(())
        }
        TryConnectCustomAgentResponse::FailCli { error } => {
            Err(AgentError::bad_request(format!("cli_not_found: {error}")))
        }
        TryConnectCustomAgentResponse::FailAcp { error } => {
            Err(AgentError::bad_request(format!("acp_init_failed: {error}")))
        }
    }
}

fn first_token(s: &str) -> &str {
    s.split_whitespace().next().unwrap_or(s)
}

fn persisted_a2a_auth_token(env_json: Option<&str>) -> Option<String> {
    env_json
        .and_then(|value| serde_json::from_str::<Vec<AgentEnvEntry>>(value).ok())
        .and_then(|entries| {
            entries
                .into_iter()
                .find(|entry| entry.name == "TJUAE_A2A_AUTH_TOKEN")
                .map(|entry| entry.value)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_token_is_reused_when_editor_does_not_return_secret() {
        let env = serde_json::json!([
            { "name": "TJUAE_A2A_AUTH_TYPE", "value": "bearer" },
            { "name": "TJUAE_A2A_AUTH_TOKEN", "value": "secret-token" }
        ])
        .to_string();

        assert_eq!(persisted_a2a_auth_token(Some(&env)).as_deref(), Some("secret-token"));
    }

    #[test]
    fn malformed_environment_does_not_expose_or_invent_a_token() {
        assert!(persisted_a2a_auth_token(Some("not-json")).is_none());
        assert!(persisted_a2a_auth_token(None).is_none());
    }
}
