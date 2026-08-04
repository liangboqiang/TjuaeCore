use std::collections::{HashMap, HashSet, VecDeque};

use a2a::{Message, Part, Role, SendMessageConfiguration, SendMessageRequest, SendMessageResponse};
use base64::Engine;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tjuaeui_api_types::{
    A2aAuditEventResponse, A2aDelegationGraphResponse, A2aDelegationPermissionResponse, A2aDelegationResponse,
    A2aDelegationTaskNode, DelegateA2aTaskRequest, RequestA2aDelegationPermission,
};
use tjuaeui_common::now_ms;
use tjuaeui_db::{
    A2aAuditEventRow, A2aDelegationPermissionRow, A2aDelegationRow, A2aTaskRow, CreateA2aDelegationParams,
    CreateA2aDelegationPermissionParams, RecordA2aAuditParams, UpdateA2aDelegationParams, UpsertA2aTaskParams,
};

use crate::error::AgentError;

use super::service::A2aAgentService;

const MAX_PERMISSION_SECONDS: u64 = 24 * 60 * 60;
const MAX_DELEGATION_MESSAGE_BYTES: usize = 64 * 1024;
const MAX_GRAPH_NODES: usize = 128;
const SCOPE_MESSAGE: &str = "delegate:message";
const SCOPE_CANCEL: &str = "delegate:cancel";
const ALLOWED_SCOPES: [&str; 2] = [SCOPE_MESSAGE, SCOPE_CANCEL];

impl A2aAgentService {
    pub async fn request_delegation_permission(
        &self,
        actor_agent_id: &str,
        request: RequestA2aDelegationPermission,
    ) -> Result<A2aDelegationPermissionResponse, AgentError> {
        let parent = self.owned_task(actor_agent_id, &request.parent_task_id).await?;
        let target_agent_ids = normalize_targets(request.target_agent_ids)?;
        let scopes = normalize_scopes(request.scopes)?;
        if !(60..=MAX_PERMISSION_SECONDS).contains(&request.expires_in_seconds) {
            return Err(AgentError::bad_request("委托授权有效期必须在 60 秒到 24 小时之间"));
        }
        for target in &target_agent_ids {
            if target == actor_agent_id {
                return Err(AgentError::bad_request("委托目标不能是当前 Agent 自身"));
            }
            self.profile(target).await?;
        }
        let id = tjuaeui_common::generate_prefixed_id("a2aperm");
        let targets_json = serde_json::to_string(&target_agent_ids)
            .map_err(|error| AgentError::internal(format!("编码委托目标失败：{error}")))?;
        let scopes_json = serde_json::to_string(&scopes)
            .map_err(|error| AgentError::internal(format!("编码委托权限失败：{error}")))?;
        let expires_at = now_ms().saturating_add(
            i64::try_from(request.expires_in_seconds)
                .unwrap_or(i64::MAX)
                .saturating_mul(1000),
        );
        let row = self
            .repo
            .create_delegation_permission(CreateA2aDelegationPermissionParams {
                id: &id,
                parent_task_id: &parent.id,
                target_agent_ids_json: &targets_json,
                scopes_json: &scopes_json,
                requested_expires_at: expires_at,
            })
            .await
            .map_err(db_error)?;
        self.audit(
            "delegation.permission_requested",
            Some(actor_agent_id),
            None,
            Some(&parent.id),
            None,
            json!({
                "targetCount": target_agent_ids.len(),
                "scopes": scopes,
                "expiresAt": expires_at,
            }),
        )
        .await?;
        permission_response(&row, None)
    }

    pub async fn approve_delegation_permission(
        &self,
        actor_agent_id: &str,
        permission_id: &str,
    ) -> Result<A2aDelegationPermissionResponse, AgentError> {
        let existing = self.permission_owned_by(actor_agent_id, permission_id).await?;
        if existing.status != "pending" || existing.requested_expires_at <= now_ms() {
            return Err(AgentError::conflict("委托权限已决定或已经过期"));
        }
        let token = random_token(32)?;
        let row = self
            .repo
            .approve_delegation_permission(permission_id, &hash_text(&token), now_ms())
            .await
            .map_err(db_error)?;
        self.audit(
            "delegation.permission_approved",
            Some(actor_agent_id),
            None,
            Some(&row.parent_task_id),
            None,
            json!({
                "permissionId": permission_id,
                "expiresAt": row.requested_expires_at,
            }),
        )
        .await?;
        permission_response(&row, Some(token))
    }

    pub async fn revoke_delegation_permission(
        &self,
        actor_agent_id: &str,
        permission_id: &str,
    ) -> Result<(), AgentError> {
        let row = self.permission_owned_by(actor_agent_id, permission_id).await?;
        self.repo
            .revoke_delegation_permission(permission_id, now_ms())
            .await
            .map_err(db_error)?;
        self.audit(
            "delegation.permission_revoked",
            Some(actor_agent_id),
            None,
            Some(&row.parent_task_id),
            None,
            json!({"permissionId": permission_id}),
        )
        .await?;
        Ok(())
    }

    pub async fn delegate_task(
        &self,
        actor_agent_id: &str,
        request: DelegateA2aTaskRequest,
    ) -> Result<A2aDelegationResponse, AgentError> {
        let parent = self.owned_task(actor_agent_id, &request.parent_task_id).await?;
        validate_delegation_request(&request)?;
        let permission = self.validate_permission(&parent, &request, SCOPE_MESSAGE).await?;
        if let Some(existing) = self
            .repo
            .find_delegation_by_idempotency(&parent.id, &request.target_agent_id, &request.idempotency_key)
            .await
            .map_err(db_error)?
        {
            return Ok(delegation_response(&existing));
        }

        let delegation_id = tjuaeui_common::generate_prefixed_id("a2adeleg");
        let edge = self
            .repo
            .create_delegation(CreateA2aDelegationParams {
                id: &delegation_id,
                parent_task_id: &parent.id,
                target_agent_id: &request.target_agent_id,
                permission_id: &permission.id,
                idempotency_key: &request.idempotency_key,
            })
            .await
            .map_err(db_error)?;
        let message_hash = hash_text(&request.message);
        self.audit(
            "delegation.dispatched",
            Some(actor_agent_id),
            Some(&request.target_agent_id),
            Some(&parent.id),
            Some(&edge.id),
            json!({
                "messageBytes": request.message.len(),
                "messageSha256": message_hash,
                "permissionId": permission.id,
            }),
        )
        .await?;

        match self.dispatch_delegation(&parent, &permission, &edge, &request).await {
            Ok(edge) => Ok(delegation_response(&edge)),
            Err(error) => {
                let code = orchestration_error_code(&error);
                let _ = self
                    .repo
                    .update_delegation(UpdateA2aDelegationParams {
                        id: &edge.id,
                        child_task_id: None,
                        state: "failed",
                        context_id: None,
                        last_error_code: Some(code),
                    })
                    .await;
                let _ = self
                    .audit(
                        "delegation.failed",
                        Some(actor_agent_id),
                        Some(&request.target_agent_id),
                        Some(&parent.id),
                        Some(&edge.id),
                        json!({"errorCode": code}),
                    )
                    .await;
                Err(error)
            }
        }
    }

    pub async fn delegation_graph(
        &self,
        actor_agent_id: &str,
        root_task_id: &str,
    ) -> Result<A2aDelegationGraphResponse, AgentError> {
        self.owned_task(actor_agent_id, root_task_id).await?;
        self.build_graph(root_task_id).await
    }

    pub async fn recover_delegation_graph(
        &self,
        actor_agent_id: &str,
        root_task_id: &str,
    ) -> Result<A2aDelegationGraphResponse, AgentError> {
        self.owned_task(actor_agent_id, root_task_id).await?;
        let (_, edges) = self.collect_graph(root_task_id).await?;
        for edge in edges {
            let Some(child_task_id) = edge.child_task_id.as_deref() else {
                continue;
            };
            let Some(child) = self.repo.find_task(child_task_id).await.map_err(db_error)? else {
                continue;
            };
            if is_terminal_state(&child.state) {
                continue;
            }
            let Some(remote_task_id) = child.remote_task_id.as_deref() else {
                continue;
            };
            let profile = self.profile(&edge.target_agent_id).await?;
            let client = self.client_for_profile(&profile).await?;
            match client.get_task(remote_task_id).await {
                Ok(remote) => {
                    let state = task_state_name(&remote.status.state);
                    let artifact_json = remote
                        .artifacts
                        .as_ref()
                        .map(serde_json::to_string)
                        .transpose()
                        .map_err(|error| AgentError::internal(format!("编码 A2A Artifact 失败：{error}")))?;
                    self.update_task_snapshot(&child, Some(&remote.context_id), state, artifact_json.as_deref())
                        .await?;
                    self.repo
                        .update_delegation(UpdateA2aDelegationParams {
                            id: &edge.id,
                            child_task_id: Some(&child.id),
                            state: edge_state(state),
                            context_id: Some(&remote.context_id),
                            last_error_code: None,
                        })
                        .await
                        .map_err(db_error)?;
                }
                Err(error) => {
                    let code = orchestration_error_code(&error);
                    self.repo
                        .update_delegation(UpdateA2aDelegationParams {
                            id: &edge.id,
                            child_task_id: Some(&child.id),
                            state: "recovery_failed",
                            context_id: child.context_id.as_deref(),
                            last_error_code: Some(code),
                        })
                        .await
                        .map_err(db_error)?;
                }
            }
        }
        self.audit(
            "delegation.graph_recovered",
            Some(actor_agent_id),
            None,
            Some(root_task_id),
            None,
            json!({}),
        )
        .await?;
        self.build_graph(root_task_id).await
    }

    pub async fn cancel_delegation_graph(
        &self,
        actor_agent_id: &str,
        root_task_id: &str,
    ) -> Result<A2aDelegationGraphResponse, AgentError> {
        self.owned_task(actor_agent_id, root_task_id).await?;
        let (_, mut edges) = self.collect_graph(root_task_id).await?;
        edges.reverse();
        let mut failures = 0_u32;
        let mut denied = 0_u32;
        for edge in edges {
            let Some(child_task_id) = edge.child_task_id.as_deref() else {
                continue;
            };
            let Some(child) = self.repo.find_task(child_task_id).await.map_err(db_error)? else {
                continue;
            };
            if is_terminal_state(&child.state) {
                continue;
            }
            let cancel_allowed = match self
                .repo
                .find_delegation_permission(&edge.permission_id)
                .await
                .map_err(db_error)?
            {
                Some(permission) => permission_has_scope(&permission, SCOPE_CANCEL)?,
                None => false,
            };
            if !cancel_allowed {
                failures += 1;
                denied += 1;
                self.repo
                    .update_delegation(UpdateA2aDelegationParams {
                        id: &edge.id,
                        child_task_id: Some(&child.id),
                        state: "cancel_denied",
                        context_id: child.context_id.as_deref(),
                        last_error_code: Some("permission_denied"),
                    })
                    .await
                    .map_err(db_error)?;
                continue;
            }
            let Some(remote_task_id) = child.remote_task_id.as_deref() else {
                continue;
            };
            let profile = self.profile(&edge.target_agent_id).await?;
            let client = self.client_for_profile(&profile).await?;
            match client.cancel_task(remote_task_id).await {
                Ok(remote) => {
                    let state = task_state_name(&remote.status.state);
                    self.update_task_snapshot(&child, Some(&remote.context_id), state, None)
                        .await?;
                    self.repo
                        .update_delegation(UpdateA2aDelegationParams {
                            id: &edge.id,
                            child_task_id: Some(&child.id),
                            state: "canceled",
                            context_id: Some(&remote.context_id),
                            last_error_code: None,
                        })
                        .await
                        .map_err(db_error)?;
                }
                Err(error) => {
                    failures += 1;
                    let code = orchestration_error_code(&error);
                    self.repo
                        .update_delegation(UpdateA2aDelegationParams {
                            id: &edge.id,
                            child_task_id: Some(&child.id),
                            state: "cancel_failed",
                            context_id: child.context_id.as_deref(),
                            last_error_code: Some(code),
                        })
                        .await
                        .map_err(db_error)?;
                }
            }
        }
        self.audit(
            "delegation.cancel_propagated",
            Some(actor_agent_id),
            None,
            Some(root_task_id),
            None,
            json!({
                "failureCount": failures,
                "permissionDeniedCount": denied,
            }),
        )
        .await?;
        self.build_graph(root_task_id).await
    }

    async fn dispatch_delegation(
        &self,
        parent: &A2aTaskRow,
        permission: &A2aDelegationPermissionRow,
        edge: &A2aDelegationRow,
        request: &DelegateA2aTaskRequest,
    ) -> Result<A2aDelegationRow, AgentError> {
        let target = self.profile(&request.target_agent_id).await?;
        let client = self.client_for_profile(&target).await?;
        let scopes: Vec<String> = parse_json_array(&permission.scopes_json, "委托权限")?;
        let mut message = Message::new(Role::User, vec![Part::text(&request.message)]);
        message.context_id = None;
        message.reference_task_ids = parent.remote_task_id.clone().map(|id| vec![id]);
        message.metadata = Some(HashMap::from([(
            "tjuaeDelegation".to_owned(),
            json!({
                "delegationId": edge.id,
                "parentTaskId": parent.id,
                "permissionId": permission.id,
                "scopes": scopes,
                "expiresAt": permission.requested_expires_at,
            }),
        )]));
        let response = client
            .send_message(&SendMessageRequest {
                message,
                configuration: Some(SendMessageConfiguration {
                    accepted_output_modes: target
                        .normalized_card_json
                        .as_deref()
                        .and_then(|value| serde_json::from_str::<a2a::AgentCard>(value).ok())
                        .map(|card| card.default_output_modes),
                    task_push_notification_config: None,
                    history_length: Some(0),
                    return_immediately: Some(true),
                }),
                metadata: Some(HashMap::from([(
                    "tjuaeDelegationId".to_owned(),
                    Value::String(edge.id.clone()),
                )])),
                tenant: None,
            })
            .await?;
        let interface_snapshot = json!({
            "binding": target.selected_binding,
            "url": target.selected_interface_url,
            "protocolVersion": target.protocol_version,
            "delegationId": edge.id,
        })
        .to_string();
        let (remote_task_id, context_id, state, artifacts) = match response {
            SendMessageResponse::Task(task) => {
                let artifacts = task
                    .artifacts
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()
                    .map_err(|error| AgentError::internal(format!("编码委托 Artifact 失败：{error}")))?;
                (
                    Some(task.id),
                    Some(task.context_id),
                    task_state_name(&task.status.state).to_owned(),
                    artifacts,
                )
            }
            SendMessageResponse::Message(message) => {
                (message.task_id, message.context_id, "completed".to_owned(), None)
            }
        };
        let child = self
            .repo
            .upsert_task(UpsertA2aTaskParams {
                id: None,
                conversation_id: &parent.conversation_id,
                agent_id: &request.target_agent_id,
                remote_task_id: remote_task_id.as_deref(),
                context_id: context_id.as_deref(),
                state: &state,
                interface_snapshot_json: &interface_snapshot,
                last_event_id: None,
                artifact_snapshot_json: artifacts.as_deref(),
                push_config_json: None,
            })
            .await
            .map_err(db_error)?;
        let updated = self
            .repo
            .update_delegation(UpdateA2aDelegationParams {
                id: &edge.id,
                child_task_id: Some(&child.id),
                state: edge_state(&state),
                context_id: context_id.as_deref(),
                last_error_code: None,
            })
            .await
            .map_err(db_error)?;
        self.audit(
            "delegation.accepted",
            Some(&parent.agent_id),
            Some(&request.target_agent_id),
            Some(&parent.id),
            Some(&edge.id),
            json!({
                "childTaskId": child.id,
                "remoteTaskPresent": remote_task_id.is_some(),
                "state": state,
            }),
        )
        .await?;
        Ok(updated)
    }

    async fn validate_permission(
        &self,
        parent: &A2aTaskRow,
        request: &DelegateA2aTaskRequest,
        required_scope: &str,
    ) -> Result<A2aDelegationPermissionRow, AgentError> {
        let row = self
            .repo
            .find_delegation_permission(&request.permission_id)
            .await
            .map_err(db_error)?
            .ok_or_else(|| AgentError::forbidden("委托权限不存在"))?;
        if row.parent_task_id != parent.id || row.status != "approved" || row.requested_expires_at <= now_ms() {
            return Err(AgentError::forbidden("委托权限无效或已过期"));
        }
        let targets: Vec<String> = parse_json_array(&row.target_agent_ids_json, "委托目标")?;
        let scopes: Vec<String> = parse_json_array(&row.scopes_json, "委托权限")?;
        if !targets.contains(&request.target_agent_id) || !scopes.iter().any(|scope| scope == required_scope) {
            return Err(AgentError::forbidden("委托目标或操作超出授权范围"));
        }
        let expected_hash = row
            .capability_token_hash
            .as_deref()
            .ok_or_else(|| AgentError::forbidden("委托 Capability Token 已撤销"))?;
        if !constant_time_eq(&hash_text(&request.capability_token), expected_hash) {
            return Err(AgentError::unauthorized("委托 Capability Token 无效"));
        }
        Ok(row)
    }

    async fn permission_owned_by(
        &self,
        actor_agent_id: &str,
        permission_id: &str,
    ) -> Result<A2aDelegationPermissionRow, AgentError> {
        let row = self
            .repo
            .find_delegation_permission(permission_id)
            .await
            .map_err(db_error)?
            .ok_or_else(|| AgentError::not_found("委托权限请求不存在"))?;
        self.owned_task(actor_agent_id, &row.parent_task_id).await?;
        Ok(row)
    }

    async fn owned_task(&self, agent_id: &str, task_id: &str) -> Result<A2aTaskRow, AgentError> {
        self.repo
            .find_task(task_id)
            .await
            .map_err(db_error)?
            .filter(|task| task.agent_id == agent_id)
            .ok_or_else(|| AgentError::not_found("A2A Task 不存在或不属于该 Agent"))
    }

    async fn update_task_snapshot(
        &self,
        task: &A2aTaskRow,
        context_id: Option<&str>,
        state: &str,
        artifact_snapshot_json: Option<&str>,
    ) -> Result<A2aTaskRow, AgentError> {
        self.repo
            .upsert_task(UpsertA2aTaskParams {
                id: Some(&task.id),
                conversation_id: &task.conversation_id,
                agent_id: &task.agent_id,
                remote_task_id: task.remote_task_id.as_deref(),
                context_id: context_id.or(task.context_id.as_deref()),
                state,
                interface_snapshot_json: &task.interface_snapshot_json,
                last_event_id: task.last_event_id.as_deref(),
                artifact_snapshot_json: artifact_snapshot_json.or(task.artifact_snapshot_json.as_deref()),
                push_config_json: task.push_config_json.as_deref(),
            })
            .await
            .map_err(db_error)
    }

    async fn collect_graph(&self, root_task_id: &str) -> Result<(Vec<A2aTaskRow>, Vec<A2aDelegationRow>), AgentError> {
        let root = self
            .repo
            .find_task(root_task_id)
            .await
            .map_err(db_error)?
            .ok_or_else(|| AgentError::not_found("A2A 根 Task 不存在"))?;
        let mut tasks = vec![root];
        let mut edges = Vec::new();
        let mut seen = HashSet::from([root_task_id.to_owned()]);
        let mut queue = VecDeque::from([root_task_id.to_owned()]);
        while let Some(parent_id) = queue.pop_front() {
            for edge in self
                .repo
                .list_delegations_by_parent(&parent_id)
                .await
                .map_err(db_error)?
            {
                if let Some(child_id) = edge.child_task_id.as_deref()
                    && seen.insert(child_id.to_owned())
                {
                    if seen.len() > MAX_GRAPH_NODES {
                        return Err(AgentError::conflict("A2A 委托图超过 128 个 Task 限制"));
                    }
                    if let Some(task) = self.repo.find_task(child_id).await.map_err(db_error)? {
                        tasks.push(task);
                        queue.push_back(child_id.to_owned());
                    }
                }
                edges.push(edge);
            }
        }
        Ok((tasks, edges))
    }

    async fn build_graph(&self, root_task_id: &str) -> Result<A2aDelegationGraphResponse, AgentError> {
        let (tasks, edges) = self.collect_graph(root_task_id).await?;
        let mut audit = Vec::new();
        let mut seen_audit = HashSet::new();
        for task in &tasks {
            for event in self.repo.list_a2a_audit_for_task(&task.id).await.map_err(db_error)? {
                if seen_audit.insert(event.id.clone()) {
                    audit.push(audit_response(event));
                }
            }
        }
        audit.sort_by_key(|event| event.created_at);
        Ok(A2aDelegationGraphResponse {
            root_task_id: root_task_id.to_owned(),
            tasks: tasks
                .into_iter()
                .map(|task| A2aDelegationTaskNode {
                    id: task.id,
                    agent_id: task.agent_id,
                    remote_task_id: task.remote_task_id,
                    context_id: task.context_id,
                    state: task.state,
                })
                .collect(),
            delegations: edges.iter().map(delegation_response).collect(),
            audit,
        })
    }

    async fn audit(
        &self,
        event_type: &str,
        actor_agent_id: Option<&str>,
        target_agent_id: Option<&str>,
        task_id: Option<&str>,
        delegation_id: Option<&str>,
        metadata: Value,
    ) -> Result<(), AgentError> {
        let metadata_json = serde_json::to_string(&metadata)
            .map_err(|error| AgentError::internal(format!("编码 A2A 审计元数据失败：{error}")))?;
        if metadata_json.len() > 4096 {
            return Err(AgentError::internal("A2A 审计元数据超过 4 KiB"));
        }
        self.repo
            .record_a2a_audit(RecordA2aAuditParams {
                event_type,
                actor_agent_id,
                target_agent_id,
                task_id,
                delegation_id,
                metadata_json: &metadata_json,
            })
            .await
            .map_err(db_error)?;
        Ok(())
    }
}

fn normalize_targets(values: Vec<String>) -> Result<Vec<String>, AgentError> {
    let mut normalized = Vec::new();
    for value in values {
        let value = value.trim();
        if value.is_empty() || value.len() > 128 {
            return Err(AgentError::bad_request("委托目标 Agent ID 无效"));
        }
        if !normalized.iter().any(|current| current == value) {
            normalized.push(value.to_owned());
        }
    }
    if normalized.is_empty() || normalized.len() > 16 {
        return Err(AgentError::bad_request("委托目标数量必须在 1 到 16 之间"));
    }
    Ok(normalized)
}

fn normalize_scopes(values: Vec<String>) -> Result<Vec<String>, AgentError> {
    let mut normalized = Vec::new();
    for value in values {
        let value = value.trim();
        if !ALLOWED_SCOPES.contains(&value) {
            return Err(AgentError::bad_request(format!("不支持的委托权限：{value}")));
        }
        if !normalized.iter().any(|current| current == value) {
            normalized.push(value.to_owned());
        }
    }
    if !normalized.iter().any(|scope| scope == SCOPE_MESSAGE) {
        return Err(AgentError::bad_request("委托至少需要 delegate:message 权限"));
    }
    Ok(normalized)
}

fn validate_delegation_request(request: &DelegateA2aTaskRequest) -> Result<(), AgentError> {
    if request.message.trim().is_empty() || request.message.len() > MAX_DELEGATION_MESSAGE_BYTES {
        return Err(AgentError::bad_request("委托消息必须在 1 字节到 64 KiB 之间"));
    }
    if request.idempotency_key.is_empty()
        || request.idempotency_key.len() > 128
        || !request
            .idempotency_key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(AgentError::bad_request("委托幂等键无效"));
    }
    Ok(())
}

fn permission_response(
    row: &A2aDelegationPermissionRow,
    capability_token: Option<String>,
) -> Result<A2aDelegationPermissionResponse, AgentError> {
    let status = if row.status == "approved" && row.requested_expires_at <= now_ms() {
        "expired".to_owned()
    } else {
        row.status.clone()
    };
    Ok(A2aDelegationPermissionResponse {
        id: row.id.clone(),
        parent_task_id: row.parent_task_id.clone(),
        target_agent_ids: parse_json_array(&row.target_agent_ids_json, "委托目标")?,
        scopes: parse_json_array(&row.scopes_json, "委托权限")?,
        status,
        expires_at: row.requested_expires_at,
        approved_at: row.approved_at,
        revoked_at: row.revoked_at,
        capability_token,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

fn delegation_response(row: &A2aDelegationRow) -> A2aDelegationResponse {
    A2aDelegationResponse {
        id: row.id.clone(),
        parent_task_id: row.parent_task_id.clone(),
        child_task_id: row.child_task_id.clone(),
        target_agent_id: row.target_agent_id.clone(),
        permission_id: row.permission_id.clone(),
        state: row.state.clone(),
        context_id: row.context_id.clone(),
        last_error_code: row.last_error_code.clone(),
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

fn audit_response(row: A2aAuditEventRow) -> A2aAuditEventResponse {
    A2aAuditEventResponse {
        id: row.id,
        event_type: row.event_type,
        actor_agent_id: row.actor_agent_id,
        target_agent_id: row.target_agent_id,
        task_id: row.task_id,
        delegation_id: row.delegation_id,
        metadata: serde_json::from_str(&row.metadata_json).unwrap_or(Value::Null),
        created_at: row.created_at,
    }
}

fn parse_json_array(value: &str, label: &str) -> Result<Vec<String>, AgentError> {
    serde_json::from_str(value).map_err(|_| AgentError::internal(format!("{label}缓存损坏")))
}

fn permission_has_scope(row: &A2aDelegationPermissionRow, required_scope: &str) -> Result<bool, AgentError> {
    let scopes: Vec<String> = parse_json_array(&row.scopes_json, "委托权限")?;
    Ok(scopes.iter().any(|scope| scope == required_scope))
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

fn edge_state(task_state: &str) -> &'static str {
    match task_state {
        "completed" => "completed",
        "failed" | "rejected" => "failed",
        "canceled" => "canceled",
        "input_required" => "input_required",
        "auth_required" => "auth_required",
        _ => "active",
    }
}

fn is_terminal_state(state: &str) -> bool {
    matches!(state, "completed" | "failed" | "canceled" | "rejected")
}

fn orchestration_error_code(error: &AgentError) -> &'static str {
    match error {
        AgentError::BadRequest(_) => "invalid_request",
        AgentError::Unauthorized(_) => "authentication_failed",
        AgentError::Forbidden(_) => "permission_denied",
        AgentError::NotFound(_) => "not_found",
        AgentError::Conflict(_) => "conflict",
        AgentError::BadGateway(_) | AgentError::Acp(_) => "upstream_error",
        AgentError::Timeout(_) => "timeout",
        AgentError::RateLimited => "rate_limited",
        AgentError::ConversationArchived(_) => "conversation_archived",
        AgentError::WorkspacePathRuntimeUnavailable(_) => "workspace_unavailable",
        AgentError::RuntimeAssetContract { reason, .. } => reason.as_code(),
        AgentError::Internal(_) => "internal_error",
    }
}

fn random_token(bytes: usize) -> Result<String, AgentError> {
    let mut random = vec![0_u8; bytes];
    getrandom::getrandom(&mut random)
        .map_err(|error| AgentError::internal(format!("生成 Capability Token 失败：{error}")))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random))
}

fn hash_text(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    left.len() == right.len()
        && left
            .as_bytes()
            .iter()
            .zip(right.as_bytes())
            .fold(0_u8, |diff, (left, right)| diff | (left ^ right))
            == 0
}

fn db_error(error: impl std::fmt::Display) -> AgentError {
    AgentError::internal(format!("A2A 数据库操作失败：{error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_assets::RuntimeAssetFailureReason;

    #[test]
    fn runtime_asset_contract_keeps_its_stable_reason_code() {
        let error = AgentError::runtime_asset_contract(
            RuntimeAssetFailureReason::ReceiptMissing,
            "运行时未返回实际资产加载回执",
        );

        assert_eq!(orchestration_error_code(&error), "TJUAE_RUNTIME_ASSET_RECEIPT_MISSING");
    }

    #[test]
    fn scopes_are_allowlisted_and_message_is_required() {
        assert_eq!(
            normalize_scopes(vec![SCOPE_MESSAGE.into(), SCOPE_CANCEL.into()]).unwrap(),
            vec![SCOPE_MESSAGE, SCOPE_CANCEL]
        );
        assert!(normalize_scopes(vec![SCOPE_CANCEL.into()]).is_err());
        assert!(normalize_scopes(vec!["local:admin".into()]).is_err());
    }

    #[test]
    fn idempotency_keys_reject_control_characters() {
        let request = DelegateA2aTaskRequest {
            parent_task_id: "parent".into(),
            target_agent_id: "target".into(),
            permission_id: "permission".into(),
            capability_token: "token".into(),
            message: "work".into(),
            idempotency_key: "bad\nkey".into(),
        };
        assert!(validate_delegation_request(&request).is_err());
    }

    #[test]
    fn cancellation_scope_is_checked_independently_from_message_scope() {
        let mut permission = A2aDelegationPermissionRow {
            id: "permission".into(),
            parent_task_id: "parent".into(),
            target_agent_ids_json: r#"["target"]"#.into(),
            scopes_json: r#"["delegate:message"]"#.into(),
            status: "revoked".into(),
            capability_token_hash: None,
            requested_expires_at: 0,
            approved_at: Some(0),
            revoked_at: Some(0),
            created_at: 0,
            updated_at: 0,
        };
        assert!(!permission_has_scope(&permission, SCOPE_CANCEL).unwrap());

        // Revoking message dispatch does not remove the already-granted
        // cancellation scope: stopping previously delegated work remains safe.
        permission.scopes_json = r#"["delegate:message","delegate:cancel"]"#.into();
        assert!(permission_has_scope(&permission, SCOPE_CANCEL).unwrap());
    }
}
