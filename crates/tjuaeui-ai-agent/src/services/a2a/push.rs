#![allow(
    dead_code,
    reason = "A2A push callback ingress is implemented and covered by unit tests, but is not exposed by the desktop router yet"
)]

use std::sync::Arc;

use base64::Engine;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tjuaeui_api_types::{A2aBinding, A2aPushSubscriptionResponse, RegisterA2aPushRequest, WebSocketMessage};
use tjuaeui_common::now_ms;
use tjuaeui_db::{
    A2aAgentProfileRow, A2aPushSubscriptionRow, A2aTaskRow, RecordA2aPushDeliveryParams, RecordA2aPushDeliveryResult,
    UpsertA2aPushSubscriptionParams, UpsertA2aTaskParams,
};
use url::Url;

use crate::error::AgentError;
use crate::manager::a2a::{A2aClient, A2aClientConfig, GrpcA2aClient, IA2aClient};

use super::service::A2aAgentService;

const MAX_PUSH_BODY_BYTES: usize = 2 * 1024 * 1024;
const MAX_PUSHES_PER_MINUTE: i64 = 120;
const MAX_PUSH_EXPIRY_SECONDS: u64 = 30 * 24 * 60 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PushReceipt {
    Accepted,
    Duplicate,
}

impl A2aAgentService {
    pub async fn register_push(
        &self,
        agent_id: &str,
        request: RegisterA2aPushRequest,
    ) -> Result<A2aPushSubscriptionResponse, AgentError> {
        if request.task_id.trim().is_empty() {
            return Err(AgentError::bad_request("A2A task_id 不能为空"));
        }
        if !(60..=MAX_PUSH_EXPIRY_SECONDS).contains(&request.expires_in_seconds) {
            return Err(AgentError::bad_request("Push 有效期必须在 60 秒到 30 天之间"));
        }
        let profile = self.profile(agent_id).await?;
        let card = profile
            .extended_card_json
            .as_deref()
            .or(profile.normalized_card_json.as_deref())
            .ok_or_else(|| AgentError::conflict("A2A Agent 尚无 Card 缓存"))
            .and_then(parse_card)?;
        if card.capabilities.push_notifications != Some(true) {
            return Err(AgentError::conflict("该 A2A Agent 未声明 Push Notification 能力"));
        }
        let task = self
            .repo
            .find_task_by_remote(agent_id, &request.task_id)
            .await
            .map_err(db_error)?
            .ok_or_else(|| AgentError::not_found("找不到对应的本地 A2A Task"))?;

        let subscription_id = random_token(18)?;
        let path_secret = random_token(32)?;
        let notification_token = random_token(32)?;
        let callback_url = build_callback_url(
            &request.callback_base_url,
            &subscription_id,
            &path_secret,
            profile.allow_insecure,
        )?;
        let local_config_id = random_token(18)?;
        let requested_config = a2a::TaskPushNotificationConfig {
            url: callback_url.clone(),
            id: Some(local_config_id.clone()),
            task_id: request.task_id.clone(),
            token: Some(notification_token.clone()),
            authentication: None,
            tenant: None,
        };
        let client = self.client_for_profile(&profile).await?;
        let remote_config = client.create_push_config(&requested_config).await?;
        if remote_config.task_id != request.task_id {
            return Err(AgentError::bad_gateway("A2A Agent 返回的 Push 配置绑定到了不同的 Task"));
        }
        let config_id = remote_config.id.unwrap_or(local_config_id);
        let expires_at = now_ms().saturating_add(
            i64::try_from(request.expires_in_seconds)
                .unwrap_or(i64::MAX)
                .saturating_mul(1000),
        );
        let redacted_callback_url = callback_url.replace(&path_secret, "{secret}");
        let row = self
            .repo
            .upsert_push_subscription(UpsertA2aPushSubscriptionParams {
                id: &subscription_id,
                agent_id,
                task_id: &request.task_id,
                config_id: &config_id,
                callback_url: &redacted_callback_url,
                path_secret_hash: &hash_text(&path_secret),
                notification_token_hash: &hash_text(&notification_token),
                expires_at,
            })
            .await
            .map_err(db_error)?;
        let push_metadata = json!({
            "subscriptionId": row.id,
            "configId": row.config_id,
            "expiresAt": row.expires_at,
        })
        .to_string();
        self.repo
            .upsert_task(UpsertA2aTaskParams {
                id: Some(&task.id),
                conversation_id: &task.conversation_id,
                agent_id: &task.agent_id,
                remote_task_id: task.remote_task_id.as_deref(),
                context_id: task.context_id.as_deref(),
                state: &task.state,
                interface_snapshot_json: &task.interface_snapshot_json,
                last_event_id: task.last_event_id.as_deref(),
                artifact_snapshot_json: task.artifact_snapshot_json.as_deref(),
                push_config_json: Some(&push_metadata),
            })
            .await
            .map_err(db_error)?;
        Ok(subscription_response(&row, Some(callback_url)))
    }

    pub async fn list_pushes(&self, agent_id: &str) -> Result<Vec<A2aPushSubscriptionResponse>, AgentError> {
        self.profile(agent_id).await?;
        Ok(self
            .repo
            .list_push_subscriptions(agent_id)
            .await
            .map_err(db_error)?
            .iter()
            .map(|row| subscription_response(row, None))
            .collect())
    }

    pub async fn revoke_push(&self, agent_id: &str, subscription_id: &str) -> Result<(), AgentError> {
        let profile = self.profile(agent_id).await?;
        let row = self
            .repo
            .find_push_subscription(subscription_id)
            .await
            .map_err(db_error)?
            .filter(|row| row.agent_id == agent_id)
            .ok_or_else(|| AgentError::not_found("找不到 A2A Push 订阅"))?;
        if row.revoked_at.is_some() {
            return Ok(());
        }

        // Stop accepting callbacks first. A failed upstream delete must never
        // leave a locally active callback credential.
        self.repo
            .revoke_push_subscription(subscription_id, now_ms())
            .await
            .map_err(db_error)?;
        let client = self.client_for_profile(&profile).await?;
        client
            .delete_push_config(&a2a::DeleteTaskPushNotificationConfigRequest {
                task_id: row.task_id,
                id: row.config_id,
                tenant: None,
            })
            .await
    }

    pub(crate) async fn receive_push(
        &self,
        subscription_id: &str,
        path_secret: &str,
        notification_token: Option<&str>,
        event_id: Option<&str>,
        body: &[u8],
    ) -> Result<PushReceipt, AgentError> {
        if body.len() > MAX_PUSH_BODY_BYTES {
            return Err(AgentError::bad_request("A2A Push 请求体超过 2 MiB 限制"));
        }
        let row = self
            .repo
            .find_push_subscription(subscription_id)
            .await
            .map_err(db_error)?
            .ok_or_else(|| AgentError::not_found("A2A Push 订阅不存在"))?;
        if row.revoked_at.is_some() || row.expires_at <= now_ms() {
            return Err(AgentError::forbidden("A2A Push 订阅已失效"));
        }
        if !constant_time_eq(&hash_text(path_secret), &row.path_secret_hash)
            || !constant_time_eq(
                &hash_text(notification_token.unwrap_or_default()),
                &row.notification_token_hash,
            )
        {
            return Err(AgentError::unauthorized("A2A Push 回调凭据无效"));
        }

        let payload_hash = hash_bytes(body);
        let parsed = parse_push_event(body)?;
        if parsed.task_id != row.task_id {
            return Err(AgentError::bad_request("A2A Push 事件的 Task 与订阅不匹配"));
        }
        let task = self
            .repo
            .find_task_by_remote(&row.agent_id, &row.task_id)
            .await
            .map_err(db_error)?
            .ok_or_else(|| AgentError::not_found("A2A Push 对应的本地 Task 不存在"))?;
        let event_key = sanitize_event_id(event_id).unwrap_or_else(|| payload_hash.clone());
        match self
            .repo
            .record_push_delivery(
                RecordA2aPushDeliveryParams {
                    subscription_id,
                    event_key: &event_key,
                    event_kind: parsed.kind,
                    task_id: &parsed.task_id,
                    payload_hash: &payload_hash,
                    received_at: now_ms(),
                },
                MAX_PUSHES_PER_MINUTE,
            )
            .await
            .map_err(db_error)?
        {
            RecordA2aPushDeliveryResult::Duplicate => return Ok(PushReceipt::Duplicate),
            RecordA2aPushDeliveryResult::RateLimited => return Err(AgentError::RateLimited),
            RecordA2aPushDeliveryResult::Accepted => {}
        }

        let event_kind = parsed.kind;
        if let Err(error) = self.persist_push_event(&task, parsed).await {
            // The idempotency receipt is final only after the task snapshot is
            // durable. Releasing it lets the sender retry the same event.
            if let Err(cleanup_error) = self.repo.delete_push_delivery(subscription_id, &event_key).await {
                tracing::error!(
                    subscription_id,
                    event_key,
                    error = %cleanup_error,
                    "failed to release A2A push idempotency record"
                );
            }
            return Err(error);
        }
        if let Some(broadcaster) = &self.broadcaster {
            broadcaster.broadcast(WebSocketMessage::new(
                "a2a.pushReceived",
                json!({
                    "agentId": row.agent_id,
                    "taskId": row.task_id,
                    "subscriptionId": row.id,
                    "eventKind": event_kind,
                }),
            ));
        }
        Ok(PushReceipt::Accepted)
    }

    async fn persist_push_event(&self, task: &A2aTaskRow, event: ParsedPushEvent) -> Result<(), AgentError> {
        let artifact_snapshot = match event.artifact {
            Some(artifact) => merge_artifact(task.artifact_snapshot_json.as_deref(), artifact, event.append)?,
            None => event
                .artifacts
                .map(|artifacts| serde_json::to_string(&artifacts))
                .transpose()
                .map_err(|error| AgentError::internal(format!("编码 A2A Artifact 失败：{error}")))?
                .or_else(|| task.artifact_snapshot_json.clone()),
        };
        let state = event.state.unwrap_or_else(|| task.state.clone());
        let context_id = event.context_id.as_deref().or(task.context_id.as_deref());
        self.repo
            .upsert_task(UpsertA2aTaskParams {
                id: Some(&task.id),
                conversation_id: &task.conversation_id,
                agent_id: &task.agent_id,
                remote_task_id: task.remote_task_id.as_deref(),
                context_id,
                state: &state,
                interface_snapshot_json: &task.interface_snapshot_json,
                last_event_id: task.last_event_id.as_deref(),
                artifact_snapshot_json: artifact_snapshot.as_deref(),
                push_config_json: task.push_config_json.as_deref(),
            })
            .await
            .map_err(db_error)?;
        Ok(())
    }

    pub(super) async fn profile(&self, agent_id: &str) -> Result<A2aAgentProfileRow, AgentError> {
        self.repo
            .find_profile(agent_id)
            .await
            .map_err(db_error)?
            .ok_or_else(|| AgentError::not_found(format!("找不到 A2A Agent“{agent_id}”")))
    }

    pub(super) async fn client_for_profile(
        &self,
        profile: &A2aAgentProfileRow,
    ) -> Result<Arc<dyn IA2aClient>, AgentError> {
        let endpoint_text = profile
            .selected_interface_url
            .as_deref()
            .ok_or_else(|| AgentError::conflict("A2A Agent 尚无可用接口缓存"))?;
        let endpoint = Url::parse(endpoint_text).map_err(|_| AgentError::internal("A2A 接口 URL 缓存损坏"))?;
        let binding = match profile.selected_binding.as_deref() {
            Some("json_rpc") => A2aBinding::JsonRpc,
            Some("http_json") => A2aBinding::HttpJson,
            Some("grpc") => A2aBinding::Grpc,
            _ => return Err(AgentError::internal("A2A Binding 缓存损坏")),
        };
        let config = A2aClientConfig {
            endpoint,
            binding,
            credentials: self.load_credentials_for_url(profile, endpoint_text).await?,
            tenant: profile.selected_tenant.clone(),
            compatibility_mode: if profile.compatibility_mode == "v0_3" {
                tjuaeui_api_types::A2aCompatibilityMode::V03
            } else {
                tjuaeui_api_types::A2aCompatibilityMode::V1
            },
            extensions: profile_extensions(profile)?,
            allow_insecure: profile.allow_insecure,
            allow_private_network: profile.allow_private_network,
        };
        match binding {
            A2aBinding::Grpc => Ok(Arc::new(GrpcA2aClient::connect(config).await?)),
            A2aBinding::JsonRpc | A2aBinding::HttpJson => Ok(Arc::new(A2aClient::new(config)?)),
        }
    }
}

fn profile_extensions(profile: &A2aAgentProfileRow) -> Result<Vec<String>, AgentError> {
    let Some(raw_card) = profile.normalized_card_json.as_deref() else {
        return Ok(Vec::new());
    };
    let card: a2a::AgentCard = serde_json::from_str(raw_card).map_err(|_| AgentError::internal("A2A Card 缓存损坏"))?;
    Ok(card
        .capabilities
        .extensions
        .unwrap_or_default()
        .into_iter()
        .map(|extension| extension.uri)
        .collect())
}

#[derive(Debug)]
struct ParsedPushEvent {
    kind: &'static str,
    task_id: String,
    context_id: Option<String>,
    state: Option<String>,
    artifacts: Option<Vec<a2a::Artifact>>,
    artifact: Option<a2a::Artifact>,
    append: bool,
}

fn parse_push_event(body: &[u8]) -> Result<ParsedPushEvent, AgentError> {
    let value: Value =
        serde_json::from_slice(body).map_err(|_| AgentError::bad_request("A2A Push 请求体不是有效 JSON"))?;
    let event = serde_json::from_value::<a2a::StreamResponse>(value.clone())
        .or_else(|_| serde_json::from_value::<a2a::Task>(value).map(a2a::StreamResponse::Task))
        .map_err(|_| AgentError::bad_request("A2A Push 不是有效的 Task 或事件"))?;
    Ok(match event {
        a2a::StreamResponse::Task(task) => ParsedPushEvent {
            kind: "task",
            task_id: task.id,
            context_id: Some(task.context_id),
            state: Some(task_state_name(&task.status.state).to_owned()),
            artifacts: task.artifacts,
            artifact: None,
            append: false,
        },
        a2a::StreamResponse::StatusUpdate(update) => ParsedPushEvent {
            kind: "status",
            task_id: update.task_id,
            context_id: Some(update.context_id),
            state: Some(task_state_name(&update.status.state).to_owned()),
            artifacts: None,
            artifact: None,
            append: false,
        },
        a2a::StreamResponse::ArtifactUpdate(update) => ParsedPushEvent {
            kind: "artifact",
            task_id: update.task_id,
            context_id: Some(update.context_id),
            state: None,
            artifacts: None,
            artifact: Some(update.artifact),
            append: update.append == Some(true),
        },
        a2a::StreamResponse::Message(message) => ParsedPushEvent {
            kind: "message",
            task_id: message
                .task_id
                .ok_or_else(|| AgentError::bad_request("A2A Push Message 缺少 taskId"))?,
            context_id: message.context_id,
            state: None,
            artifacts: None,
            artifact: None,
            append: false,
        },
    })
}

fn merge_artifact(
    current_json: Option<&str>,
    artifact: a2a::Artifact,
    append: bool,
) -> Result<Option<String>, AgentError> {
    let candidate = serde_json::to_value(artifact)
        .map_err(|error| AgentError::internal(format!("编码 A2A Artifact 失败：{error}")))?;
    let mut values = current_json
        .and_then(|value| serde_json::from_str::<Vec<Value>>(value).ok())
        .unwrap_or_default();
    let candidate_id = candidate.get("artifactId").and_then(Value::as_str);
    if let Some(existing) = values
        .iter_mut()
        .find(|value| value.get("artifactId").and_then(Value::as_str) == candidate_id)
    {
        if append {
            let new_parts = candidate.get("parts").and_then(Value::as_array).cloned();
            if let Some(new_parts) = new_parts
                && let Some(parts) = existing
                    .as_object_mut()
                    .and_then(|object| object.get_mut("parts"))
                    .and_then(Value::as_array_mut)
            {
                parts.extend(new_parts);
            }
        } else {
            *existing = candidate;
        }
    } else {
        values.push(candidate);
    }
    serde_json::to_string(&values)
        .map(Some)
        .map_err(|error| AgentError::internal(format!("缓存 A2A Artifact 失败：{error}")))
}

fn task_state_name(state: &a2a::TaskState) -> &'static str {
    match state {
        a2a::TaskState::Unspecified => "unspecified",
        a2a::TaskState::Submitted => "submitted",
        a2a::TaskState::Working => "working",
        a2a::TaskState::Completed => "completed",
        a2a::TaskState::Failed => "failed",
        a2a::TaskState::Canceled => "canceled",
        a2a::TaskState::InputRequired => "input_required",
        a2a::TaskState::Rejected => "rejected",
        a2a::TaskState::AuthRequired => "auth_required",
    }
}

fn parse_card(value: &str) -> Result<a2a::AgentCard, AgentError> {
    serde_json::from_str(value).map_err(|_| AgentError::internal("A2A Card 缓存损坏"))
}

fn build_callback_url(
    base: &str,
    subscription_id: &str,
    path_secret: &str,
    allow_insecure: bool,
) -> Result<String, AgentError> {
    let mut url = Url::parse(base).map_err(|_| AgentError::bad_request("Push Callback URL 无效"))?;
    if url.scheme() != "https" && !(allow_insecure && url.scheme() == "http") {
        return Err(AgentError::bad_request(
            "Push Callback 必须使用 HTTPS；仅显式允许不安全连接时可使用 HTTP",
        ));
    }
    if url.host_str().is_none() || !url.username().is_empty() || url.password().is_some() {
        return Err(AgentError::bad_request("Push Callback URL 不得包含用户信息"));
    }
    url.set_query(None);
    url.set_fragment(None);
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| AgentError::bad_request("Push Callback URL 不能作为层级 URL"))?;
        segments.pop_if_empty();
        segments.extend(["api", "a2a", "push", subscription_id, path_secret]);
    }
    Ok(url.to_string())
}

fn random_token(bytes: usize) -> Result<String, AgentError> {
    let mut random = vec![0_u8; bytes];
    getrandom::getrandom(&mut random).map_err(|error| AgentError::internal(format!("生成 Push 凭据失败：{error}")))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random))
}

fn hash_text(value: &str) -> String {
    hash_bytes(value.as_bytes())
}

fn hash_bytes(value: &[u8]) -> String {
    hex::encode(Sha256::digest(value))
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.as_bytes()
        .iter()
        .zip(right.as_bytes())
        .fold(0_u8, |diff, (left, right)| diff | (left ^ right))
        == 0
}

fn sanitize_event_id(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    if value.is_empty()
        || value.len() > 200
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return None;
    }
    Some(value.to_owned())
}

fn subscription_response(row: &A2aPushSubscriptionRow, callback_url: Option<String>) -> A2aPushSubscriptionResponse {
    A2aPushSubscriptionResponse {
        id: row.id.clone(),
        agent_id: row.agent_id.clone(),
        task_id: row.task_id.clone(),
        config_id: row.config_id.clone(),
        callback_url: callback_url.unwrap_or_else(|| row.callback_url.clone()),
        expires_at: row.expires_at,
        revoked: row.revoked_at.is_some(),
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

fn db_error(error: impl std::fmt::Display) -> AgentError {
    AgentError::internal(format!("A2A 数据库操作失败：{error}"))
}

#[cfg(test)]
mod tests {
    use tjuaeui_db::{
        IA2aRepository, SqliteA2aRepository, SqliteAgentMetadataRepository, UpsertA2aAgentProfileParams,
        init_database_memory,
    };

    use crate::registry::AgentRegistry;

    use super::*;

    #[test]
    fn callback_url_adds_unpredictable_path() {
        let url = build_callback_url("https://ui.example.test/root/", "subscription", "secret", false).unwrap();
        assert_eq!(url, "https://ui.example.test/root/api/a2a/push/subscription/secret");
    }

    #[test]
    fn event_id_rejects_header_injection_and_oversize() {
        assert_eq!(sanitize_event_id(Some("evt:42")).as_deref(), Some("evt:42"));
        assert!(sanitize_event_id(Some("evt\n42")).is_none());
        assert!(sanitize_event_id(Some(&"x".repeat(201))).is_none());
    }

    #[test]
    fn constant_time_hash_comparison_requires_exact_match() {
        assert!(constant_time_eq(&hash_text("secret"), &hash_text("secret")));
        assert!(!constant_time_eq(&hash_text("secret"), &hash_text("other")));
    }

    #[tokio::test]
    async fn authenticated_push_updates_task_once_and_replay_is_idempotent() {
        let database = init_database_memory().await.unwrap();
        let now = now_ms();
        sqlx::query(
            "INSERT INTO agent_metadata (
                id, name, agent_type, agent_source, enabled, sort_order, created_at, updated_at
             ) VALUES (?, 'Push Test', 'a2a', 'custom', 1, 5000, ?, ?)",
        )
        .bind("a2a-push-test")
        .bind(now)
        .bind(now)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO conversations (
                id, user_id, name, type, status, created_at, updated_at
             ) VALUES (?, 'system_default_user', 'A2A Push', 'a2a', 'running', ?, ?)",
        )
        .bind("conversation-push-test")
        .bind(now)
        .bind(now)
        .execute(database.pool())
        .await
        .unwrap();

        let repo = Arc::new(SqliteA2aRepository::new(database.pool().clone()));
        repo.upsert_profile(UpsertA2aAgentProfileParams {
            agent_id: "a2a-push-test",
            card_url: "https://agent.example/.well-known/agent-card.json",
            base_url: "https://agent.example",
            display_name: Some("Push Test"),
            allow_insecure: false,
            allow_private_network: false,
            compatibility_mode: "v1",
            raw_card_json: None,
            normalized_card_json: None,
            extended_card_json: None,
            protocol_version: Some("1.0"),
            selected_binding: Some("json_rpc"),
            selected_interface_url: Some("https://agent.example/a2a"),
            credential_ref: None,
            credential_refs_json: "[]",
            selected_tenant: None,
            etag: None,
            last_modified: None,
            cache_expires_at: None,
            fetched_at: Some(now),
            card_hash: None,
            signature_status: "unchecked",
            trust_status: "untrusted",
            trusted_origin: None,
        })
        .await
        .unwrap();
        repo.upsert_task(UpsertA2aTaskParams {
            id: Some("local-push-task"),
            conversation_id: "conversation-push-test",
            agent_id: "a2a-push-test",
            remote_task_id: Some("remote-push-task"),
            context_id: Some("remote-context"),
            state: "working",
            interface_snapshot_json: r#"{"binding":"json_rpc"}"#,
            last_event_id: None,
            artifact_snapshot_json: None,
            push_config_json: None,
        })
        .await
        .unwrap();
        repo.upsert_push_subscription(UpsertA2aPushSubscriptionParams {
            id: "subscription-push-test",
            agent_id: "a2a-push-test",
            task_id: "remote-push-task",
            config_id: "remote-config",
            callback_url: "https://callback.example/{secret}",
            path_secret_hash: &hash_text("path-secret"),
            notification_token_hash: &hash_text("notification-token"),
            expires_at: now + 60_000,
        })
        .await
        .unwrap();

        let metadata_repo = Arc::new(SqliteAgentMetadataRepository::new(database.pool().clone()));
        let service = A2aAgentService::new(repo.clone(), AgentRegistry::new(metadata_repo), [7_u8; 32]);
        let body = serde_json::json!({
            "statusUpdate": {
                "taskId": "remote-push-task",
                "contextId": "remote-context",
                "status": {"state": "TASK_STATE_COMPLETED"}
            }
        })
        .to_string();

        let first = service
            .receive_push(
                "subscription-push-test",
                "path-secret",
                Some("notification-token"),
                Some("event-1"),
                body.as_bytes(),
            )
            .await
            .unwrap();
        let duplicate = service
            .receive_push(
                "subscription-push-test",
                "path-secret",
                Some("notification-token"),
                Some("event-1"),
                body.as_bytes(),
            )
            .await
            .unwrap();
        assert_eq!(first, PushReceipt::Accepted);
        assert_eq!(duplicate, PushReceipt::Duplicate);
        assert_eq!(
            repo.find_task_by_remote("a2a-push-test", "remote-push-task")
                .await
                .unwrap()
                .unwrap()
                .state,
            "completed"
        );
        assert!(
            service
                .receive_push(
                    "subscription-push-test",
                    "path-secret",
                    Some("wrong-token"),
                    Some("event-2"),
                    body.as_bytes(),
                )
                .await
                .is_err()
        );
    }
}
