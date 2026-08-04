use std::time::Duration;

use base64::Engine;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, COOKIE, HeaderName, HeaderValue};
use reqwest::{Client, RequestBuilder, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tjuaeui_api_types::{A2aAuthKind, A2aBinding, A2aCompatibilityMode, A2aCredentialInput, A2aCredentialLocation};
use url::Url;

use crate::error::AgentError;
use crate::services::a2a::security::{A2aNetworkPolicy, apply_mtls_to_reqwest_builder, resolve_network_target};

const MAX_SSE_EVENT_BYTES: usize = 2 * 1024 * 1024;
const MAX_JSON_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const A2A_JSON_MEDIA_TYPE: &str = "application/a2a+json";

#[derive(Debug, Clone)]
pub(crate) struct A2aClientConfig {
    pub endpoint: Url,
    pub binding: A2aBinding,
    pub credentials: Vec<A2aCredentialInput>,
    pub tenant: Option<String>,
    pub compatibility_mode: A2aCompatibilityMode,
    pub extensions: Vec<String>,
    pub allow_insecure: bool,
    pub allow_private_network: bool,
}

#[derive(Clone)]
pub(crate) struct A2aClient {
    config: A2aClientConfig,
}

pub(crate) struct A2aEventStream {
    response: reqwest::Response,
    json_rpc_envelope: bool,
    buffer: String,
    last_event_id: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct StreamItem {
    pub event: a2a::StreamResponse,
    pub event_id: Option<String>,
}

#[async_trait::async_trait]
pub(crate) trait IA2aEventStream: Send {
    async fn next(&mut self) -> Result<Option<StreamItem>, AgentError>;
}

#[async_trait::async_trait]
pub(crate) trait IA2aClient: Send + Sync {
    async fn send_message(&self, request: &a2a::SendMessageRequest) -> Result<a2a::SendMessageResponse, AgentError>;

    async fn send_streaming_message(
        &self,
        request: &a2a::SendMessageRequest,
    ) -> Result<Box<dyn IA2aEventStream>, AgentError>;

    async fn get_task(&self, task_id: &str) -> Result<a2a::Task, AgentError>;

    /// Kept for A2A v1 interoperability. Conversation sessions operate on
    /// their bound task and therefore do not enumerate an Agent's task set.
    #[allow(dead_code)]
    async fn list_tasks(&self, request: &a2a::ListTasksRequest) -> Result<a2a::ListTasksResponse, AgentError>;

    async fn subscribe_to_task(
        &self,
        task_id: &str,
        last_event_id: Option<&str>,
    ) -> Result<Box<dyn IA2aEventStream>, AgentError>;

    async fn cancel_task(&self, task_id: &str) -> Result<a2a::Task, AgentError>;

    async fn get_extended_agent_card(&self) -> Result<a2a::AgentCard, AgentError>;

    async fn create_push_config(
        &self,
        config: &a2a::TaskPushNotificationConfig,
    ) -> Result<a2a::TaskPushNotificationConfig, AgentError>;

    /// Retained for full A2A push-management interoperability. The current
    /// product flow uses create/delete and keeps a local redacted catalog.
    #[allow(dead_code)]
    async fn get_push_config(
        &self,
        request: &a2a::GetTaskPushNotificationConfigRequest,
    ) -> Result<a2a::TaskPushNotificationConfig, AgentError>;

    #[allow(dead_code)]
    async fn list_push_configs(
        &self,
        request: &a2a::ListTaskPushNotificationConfigsRequest,
    ) -> Result<a2a::ListTaskPushNotificationConfigsResponse, AgentError>;

    async fn delete_push_config(
        &self,
        request: &a2a::DeleteTaskPushNotificationConfigRequest,
    ) -> Result<(), AgentError>;
}

impl A2aClient {
    pub(crate) fn new(config: A2aClientConfig) -> Result<Self, AgentError> {
        if config.binding == A2aBinding::Grpc {
            return Err(AgentError::bad_request(
                "该 Agent 只提供 gRPC；当前运行时尚未启用 gRPC 传输",
            ));
        }
        Ok(Self { config })
    }

    pub(crate) async fn send_message(
        &self,
        request: &a2a::SendMessageRequest,
    ) -> Result<a2a::SendMessageResponse, AgentError> {
        let mut request = request.clone();
        request.tenant = request.tenant.or_else(|| self.config.tenant.clone());
        match self.config.binding {
            A2aBinding::JsonRpc => {
                self.json_rpc(self.method(a2a::methods::SEND_MESSAGE, "message/send"), &request)
                    .await
            }
            A2aBinding::HttpJson => {
                let url = self.http_json_url("message:send")?;
                self.http_json_post(url, &request).await
            }
            A2aBinding::Grpc => unreachable!("rejected by constructor"),
        }
    }

    pub(crate) async fn send_streaming_message(
        &self,
        request: &a2a::SendMessageRequest,
    ) -> Result<A2aEventStream, AgentError> {
        let mut request = request.clone();
        request.tenant = request.tenant.or_else(|| self.config.tenant.clone());
        match self.config.binding {
            A2aBinding::JsonRpc => {
                let rpc = a2a::JsonRpcRequest::new(
                    uuid::Uuid::now_v7().to_string().into(),
                    self.method(a2a::methods::SEND_STREAMING_MESSAGE, "message/stream"),
                    Some(to_value(&request)?),
                );
                self.open_sse(self.config.endpoint.clone(), &rpc, true).await
            }
            A2aBinding::HttpJson => {
                let url = self.http_json_url("message:stream")?;
                self.open_sse(url, &request, false).await
            }
            A2aBinding::Grpc => unreachable!("rejected by constructor"),
        }
    }

    pub(crate) async fn get_task(&self, task_id: &str) -> Result<a2a::Task, AgentError> {
        let request = a2a::GetTaskRequest {
            id: task_id.to_owned(),
            history_length: None,
            tenant: self.config.tenant.clone(),
        };
        match self.config.binding {
            A2aBinding::JsonRpc => {
                self.json_rpc(self.method(a2a::methods::GET_TASK, "tasks/get"), &request)
                    .await
            }
            A2aBinding::HttpJson => {
                let mut url = self.http_json_task_url(task_id, None)?;
                append_optional_query(&mut url, "tenant", self.config.tenant.as_deref());
                self.http_json_get(url).await
            }
            A2aBinding::Grpc => unreachable!("rejected by constructor"),
        }
    }

    #[allow(dead_code)]
    pub(crate) async fn list_tasks(
        &self,
        request: &a2a::ListTasksRequest,
    ) -> Result<a2a::ListTasksResponse, AgentError> {
        if self.config.compatibility_mode == A2aCompatibilityMode::V03 {
            return Err(AgentError::bad_request("A2A v0.3 不提供 ListTasks 操作"));
        }
        let mut request = request.clone();
        request.tenant = request.tenant.or_else(|| self.config.tenant.clone());
        match self.config.binding {
            A2aBinding::JsonRpc => self.json_rpc(a2a::methods::LIST_TASKS, &request).await,
            A2aBinding::HttpJson => {
                let mut url = self.http_json_url("tasks")?;
                append_optional_query(&mut url, "contextId", request.context_id.as_deref());
                append_optional_query(&mut url, "pageToken", request.page_token.as_deref());
                append_optional_query(&mut url, "tenant", request.tenant.as_deref());
                append_optional_query(
                    &mut url,
                    "pageSize",
                    request.page_size.as_ref().map(|value| value.to_string()).as_deref(),
                );
                append_optional_query(
                    &mut url,
                    "historyLength",
                    request
                        .history_length
                        .as_ref()
                        .map(|value| value.to_string())
                        .as_deref(),
                );
                append_optional_query(
                    &mut url,
                    "includeArtifacts",
                    request
                        .include_artifacts
                        .as_ref()
                        .map(|value| value.to_string())
                        .as_deref(),
                );
                if let Some(status) = request.status.as_ref() {
                    let encoded = to_value(status)?;
                    append_optional_query(&mut url, "status", encoded.as_str());
                }
                if let Some(timestamp) = request.status_timestamp_after.as_ref() {
                    append_optional_query(&mut url, "statusTimestampAfter", Some(&timestamp.to_rfc3339()));
                }
                self.http_json_get(url).await
            }
            A2aBinding::Grpc => unreachable!("rejected by constructor"),
        }
    }

    pub(crate) async fn subscribe_to_task(
        &self,
        task_id: &str,
        last_event_id: Option<&str>,
    ) -> Result<A2aEventStream, AgentError> {
        let request = a2a::SubscribeToTaskRequest {
            id: task_id.to_owned(),
            tenant: self.config.tenant.clone(),
        };
        match self.config.binding {
            A2aBinding::JsonRpc => {
                let rpc = a2a::JsonRpcRequest::new(
                    uuid::Uuid::now_v7().to_string().into(),
                    self.method(a2a::methods::SUBSCRIBE_TO_TASK, "tasks/resubscribe"),
                    Some(to_value(&request)?),
                );
                self.open_sse_with_last_event_id(self.config.endpoint.clone(), &rpc, true, last_event_id)
                    .await
            }
            A2aBinding::HttpJson => {
                let mut url = self.http_json_task_url(task_id, Some(":subscribe"))?;
                append_optional_query(&mut url, "tenant", self.config.tenant.as_deref());
                self.open_sse_get(url, false, last_event_id).await
            }
            A2aBinding::Grpc => unreachable!("rejected by constructor"),
        }
    }

    pub(crate) async fn cancel_task(&self, task_id: &str) -> Result<a2a::Task, AgentError> {
        let request = a2a::CancelTaskRequest {
            id: task_id.to_owned(),
            metadata: None,
            tenant: self.config.tenant.clone(),
        };
        match self.config.binding {
            A2aBinding::JsonRpc => {
                self.json_rpc(self.method(a2a::methods::CANCEL_TASK, "tasks/cancel"), &request)
                    .await
            }
            A2aBinding::HttpJson => {
                let url = self.http_json_task_url(task_id, Some(":cancel"))?;
                self.http_json_post(url, &serde_json::json!({})).await
            }
            A2aBinding::Grpc => unreachable!("rejected by constructor"),
        }
    }

    pub(crate) async fn get_extended_agent_card(&self) -> Result<a2a::AgentCard, AgentError> {
        let request = a2a::GetExtendedAgentCardRequest {
            tenant: self.config.tenant.clone(),
        };
        match self.config.binding {
            A2aBinding::JsonRpc => self.json_rpc(a2a::methods::GET_EXTENDED_AGENT_CARD, &request).await,
            A2aBinding::HttpJson => {
                let url = self.http_json_url("extendedAgentCard")?;
                self.http_json_get(url).await
            }
            A2aBinding::Grpc => unreachable!("rejected by constructor"),
        }
    }

    pub(crate) async fn create_push_config(
        &self,
        config: &a2a::TaskPushNotificationConfig,
    ) -> Result<a2a::TaskPushNotificationConfig, AgentError> {
        match self.config.binding {
            A2aBinding::JsonRpc => self.json_rpc(a2a::methods::CREATE_PUSH_CONFIG, config).await,
            A2aBinding::HttpJson => {
                let url = self.http_json_push_config_url(&config.task_id, None)?;
                self.http_json_post(url, config).await
            }
            A2aBinding::Grpc => unreachable!("rejected by constructor"),
        }
    }

    #[allow(dead_code)]
    pub(crate) async fn get_push_config(
        &self,
        request: &a2a::GetTaskPushNotificationConfigRequest,
    ) -> Result<a2a::TaskPushNotificationConfig, AgentError> {
        match self.config.binding {
            A2aBinding::JsonRpc => self.json_rpc(a2a::methods::GET_PUSH_CONFIG, request).await,
            A2aBinding::HttpJson => {
                let url = self.http_json_push_config_url(&request.task_id, Some(&request.id))?;
                self.http_json_get(url).await
            }
            A2aBinding::Grpc => unreachable!("rejected by constructor"),
        }
    }

    #[allow(dead_code)]
    pub(crate) async fn list_push_configs(
        &self,
        request: &a2a::ListTaskPushNotificationConfigsRequest,
    ) -> Result<a2a::ListTaskPushNotificationConfigsResponse, AgentError> {
        match self.config.binding {
            A2aBinding::JsonRpc => self.json_rpc(a2a::methods::LIST_PUSH_CONFIGS, request).await,
            A2aBinding::HttpJson => {
                let mut url = self.http_json_push_config_url(&request.task_id, None)?;
                {
                    let mut query = url.query_pairs_mut();
                    if let Some(page_size) = request.page_size {
                        query.append_pair("pageSize", &page_size.to_string());
                    }
                    if let Some(page_token) = request.page_token.as_deref() {
                        query.append_pair("pageToken", page_token);
                    }
                }
                self.http_json_get(url).await
            }
            A2aBinding::Grpc => unreachable!("rejected by constructor"),
        }
    }

    pub(crate) async fn delete_push_config(
        &self,
        request: &a2a::DeleteTaskPushNotificationConfigRequest,
    ) -> Result<(), AgentError> {
        match self.config.binding {
            A2aBinding::JsonRpc => {
                let _: Value = self.json_rpc(a2a::methods::DELETE_PUSH_CONFIG, request).await?;
                Ok(())
            }
            A2aBinding::HttpJson => {
                let url = self.http_json_push_config_url(&request.task_id, Some(&request.id))?;
                self.http_json_delete(url).await
            }
            A2aBinding::Grpc => unreachable!("rejected by constructor"),
        }
    }

    async fn json_rpc<P: Serialize, R: DeserializeOwned>(&self, method: &str, params: &P) -> Result<R, AgentError> {
        let client = self.client_for_endpoint(&self.config.endpoint).await?;
        let rpc = a2a::JsonRpcRequest::new(uuid::Uuid::now_v7().to_string().into(), method, Some(to_value(params)?));
        let response = self
            .apply_headers(
                client
                    .post(self.authenticated_url(&self.config.endpoint)?)
                    .timeout(Duration::from_secs(60))
                    .header(CONTENT_TYPE, "application/json")
                    .header(ACCEPT, "application/json")
                    .json(&rpc),
            )?
            .send()
            .await
            .map_err(|_| AgentError::bad_gateway("A2A JSON-RPC 请求失败"))?;
        let response = checked_response(response).await?;
        let rpc: a2a::JsonRpcResponse = decode_json_response(response, "A2A JSON-RPC 响应").await?;
        if let Some(error) = rpc.error {
            return Err(json_rpc_error(error));
        }
        serde_json::from_value(
            rpc.result
                .ok_or_else(|| AgentError::bad_gateway("A2A JSON-RPC 响应缺少 result"))?,
        )
        .map_err(|_| AgentError::bad_gateway("A2A JSON-RPC result 格式无效"))
    }

    async fn http_json_post<P: Serialize, R: DeserializeOwned>(&self, url: Url, body: &P) -> Result<R, AgentError> {
        let client = self.client_for_endpoint(&url).await?;
        let request_url = self.authenticated_url(&url)?;
        let response = self
            .apply_headers(
                client
                    .post(request_url)
                    .timeout(Duration::from_secs(60))
                    .header(CONTENT_TYPE, A2A_JSON_MEDIA_TYPE)
                    .header(ACCEPT, A2A_JSON_MEDIA_TYPE)
                    .json(body),
            )?
            .send()
            .await
            .map_err(|_| AgentError::bad_gateway("A2A HTTP+JSON 请求失败"))?;
        decode_json_response(checked_response(response).await?, "A2A HTTP+JSON 响应").await
    }

    async fn http_json_get<R: DeserializeOwned>(&self, url: Url) -> Result<R, AgentError> {
        let client = self.client_for_endpoint(&url).await?;
        let request_url = self.authenticated_url(&url)?;
        let response = self
            .apply_headers(
                client
                    .get(request_url)
                    .timeout(Duration::from_secs(60))
                    .header(ACCEPT, A2A_JSON_MEDIA_TYPE),
            )?
            .send()
            .await
            .map_err(|_| AgentError::bad_gateway("A2A HTTP+JSON 请求失败"))?;
        decode_json_response(checked_response(response).await?, "A2A HTTP+JSON 响应").await
    }

    async fn http_json_delete(&self, url: Url) -> Result<(), AgentError> {
        let client = self.client_for_endpoint(&url).await?;
        let request_url = self.authenticated_url(&url)?;
        let response = self
            .apply_headers(
                client
                    .delete(request_url)
                    .timeout(Duration::from_secs(60))
                    .header(ACCEPT, A2A_JSON_MEDIA_TYPE),
            )?
            .send()
            .await
            .map_err(|_| AgentError::bad_gateway("A2A HTTP+JSON 删除请求失败"))?;
        checked_response(response).await?;
        Ok(())
    }

    async fn open_sse<P: Serialize>(
        &self,
        url: Url,
        body: &P,
        json_rpc_envelope: bool,
    ) -> Result<A2aEventStream, AgentError> {
        self.open_sse_with_last_event_id(url, body, json_rpc_envelope, None)
            .await
    }

    async fn open_sse_with_last_event_id<P: Serialize>(
        &self,
        url: Url,
        body: &P,
        json_rpc_envelope: bool,
        last_event_id: Option<&str>,
    ) -> Result<A2aEventStream, AgentError> {
        let client = self.client_for_endpoint(&url).await?;
        let request_url = self.authenticated_url(&url)?;
        let content_type = if json_rpc_envelope {
            "application/json"
        } else {
            A2A_JSON_MEDIA_TYPE
        };
        let mut builder = self
            .apply_headers(client.post(request_url).header(CONTENT_TYPE, content_type).json(body))?
            .header(ACCEPT, "text/event-stream");
        if let Some(last_event_id) = last_event_id {
            builder = builder.header("Last-Event-ID", last_event_id);
        }
        let response = builder
            .send()
            .await
            .map_err(|_| AgentError::bad_gateway("无法打开 A2A 事件流"))?;
        A2aEventStream::new(checked_response(response).await?, json_rpc_envelope)
    }

    async fn open_sse_get(
        &self,
        url: Url,
        json_rpc_envelope: bool,
        last_event_id: Option<&str>,
    ) -> Result<A2aEventStream, AgentError> {
        let client = self.client_for_endpoint(&url).await?;
        let request_url = self.authenticated_url(&url)?;
        let mut builder = self
            .apply_headers(client.get(request_url))?
            .header(ACCEPT, "text/event-stream");
        if let Some(last_event_id) = last_event_id {
            builder = builder.header("Last-Event-ID", last_event_id);
        }
        let response = builder
            .send()
            .await
            .map_err(|_| AgentError::bad_gateway("无法打开 A2A 任务事件流"))?;
        A2aEventStream::new(checked_response(response).await?, json_rpc_envelope)
    }

    fn apply_headers(&self, mut builder: RequestBuilder) -> Result<RequestBuilder, AgentError> {
        builder = builder.header(
            "A2A-Version",
            if self.config.compatibility_mode == A2aCompatibilityMode::V03 {
                "0.3"
            } else {
                a2a::VERSION
            },
        );
        if !self.config.extensions.is_empty() {
            builder = builder.header("A2A-Extensions", self.config.extensions.join(","));
        }
        let mut authorization_set = false;
        let mut cookies = Vec::new();
        for credential in &self.config.credentials {
            let secret = credential.secret.as_deref().unwrap_or_default();
            match credential.kind {
                A2aAuthKind::None => {}
                A2aAuthKind::Bearer | A2aAuthKind::OAuth2 | A2aAuthKind::Oidc => {
                    if authorization_set {
                        return Err(AgentError::bad_request("A2A 凭据包含多个互斥的 Authorization 方案"));
                    }
                    builder = builder.header(AUTHORIZATION, format!("Bearer {secret}"));
                    authorization_set = true;
                }
                A2aAuthKind::Basic => {
                    if authorization_set {
                        return Err(AgentError::bad_request("A2A 凭据包含多个互斥的 Authorization 方案"));
                    }
                    let encoded = base64::engine::general_purpose::STANDARD.encode(secret);
                    builder = builder.header(AUTHORIZATION, format!("Basic {encoded}"));
                    authorization_set = true;
                }
                A2aAuthKind::ApiKey if credential_location(credential) == A2aCredentialLocation::Query => {}
                A2aAuthKind::ApiKey if credential_location(credential) == A2aCredentialLocation::Cookie => {
                    let name = credential_name(credential, "A2A Cookie API Key 缺少名称")?;
                    validate_cookie_component(name, "A2A Cookie API Key 名称无效")?;
                    validate_cookie_component(secret, "A2A Cookie API Key 值无效")?;
                    cookies.push(format!("{name}={secret}"));
                }
                A2aAuthKind::ApiKey | A2aAuthKind::CustomHeader => {
                    let name = credential
                        .header_name
                        .as_deref()
                        .ok_or_else(|| AgentError::bad_request("A2A 凭据缺少 Header 名称"))
                        .and_then(|name| {
                            HeaderName::from_bytes(name.as_bytes())
                                .map_err(|_| AgentError::bad_request("A2A 凭据 Header 名称无效"))
                        })?;
                    let value =
                        HeaderValue::from_str(secret).map_err(|_| AgentError::bad_request("A2A 凭据值包含非法字符"))?;
                    builder = builder.header(name, value);
                }
                A2aAuthKind::Mtls => {}
            }
        }
        if !cookies.is_empty() {
            builder = builder.header(COOKIE, cookies.join("; "));
        }
        Ok(builder)
    }

    fn authenticated_url(&self, url: &Url) -> Result<Url, AgentError> {
        let mut authenticated = url.clone();
        for credential in self.config.credentials.iter().filter(|credential| {
            credential.kind == A2aAuthKind::ApiKey && credential_location(credential) == A2aCredentialLocation::Query
        }) {
            let name = credential_name(credential, "A2A Query API Key 缺少参数名称")?;
            if name.len() > 128 || name.bytes().any(|byte| byte.is_ascii_control()) {
                return Err(AgentError::bad_request("A2A Query API Key 参数名称无效"));
            }
            if authenticated.query_pairs().any(|(key, _)| key == name) {
                return Err(AgentError::bad_request("A2A Endpoint 已包含同名 Query API Key 参数"));
            }
            authenticated
                .query_pairs_mut()
                .append_pair(name, credential.secret.as_deref().unwrap_or_default());
        }
        Ok(authenticated)
    }

    async fn client_for_endpoint(&self, url: &Url) -> Result<Client, AgentError> {
        let addresses = resolve_network_target(
            url,
            A2aNetworkPolicy {
                allow_insecure: self.config.allow_insecure,
                allow_private_network: self.config.allow_private_network,
            },
        )
        .await?;
        let host = url
            .host_str()
            .ok_or_else(|| AgentError::bad_request("A2A 地址缺少主机名"))?;
        if self
            .config
            .credentials
            .iter()
            .any(|value| value.kind == A2aAuthKind::Mtls)
            && url.scheme() != "https"
        {
            return Err(AgentError::bad_request("mTLS 只能用于 HTTPS A2A 地址"));
        }
        let builder = apply_mtls_to_reqwest_builder(
            Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none())
                .resolve_to_addrs(host, &addresses)
                .user_agent(concat!("tjuaecore-a2a/", env!("CARGO_PKG_VERSION"))),
            &self.config.credentials,
        )?;
        tjuaeui_runtime::apply_network_proxy_to_http_client(builder)
            .build()
            .map_err(|_| AgentError::internal("无法创建 A2A 会话客户端"))
    }

    fn http_json_url(&self, suffix: &str) -> Result<Url, AgentError> {
        append_path(&self.config.endpoint, suffix)
    }

    fn method<'a>(&self, v1: &'a str, v03: &'a str) -> &'a str {
        if self.config.compatibility_mode == A2aCompatibilityMode::V03 {
            v03
        } else {
            v1
        }
    }

    fn http_json_task_url(&self, task_id: &str, action: Option<&str>) -> Result<Url, AgentError> {
        validate_path_segment(task_id)?;
        append_path(
            &self.config.endpoint,
            &format!("tasks/{task_id}{}", action.unwrap_or_default()),
        )
    }

    fn http_json_push_config_url(&self, task_id: &str, config_id: Option<&str>) -> Result<Url, AgentError> {
        validate_path_segment(task_id)?;
        if let Some(config_id) = config_id {
            validate_path_segment(config_id)?;
        }
        let suffix = config_id
            .map(|id| format!("tasks/{task_id}/pushNotificationConfigs/{id}"))
            .unwrap_or_else(|| format!("tasks/{task_id}/pushNotificationConfigs"));
        self.http_json_url(&suffix)
    }
}

fn validate_path_segment(value: &str) -> Result<(), AgentError> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~'))
    {
        return Err(AgentError::bad_gateway("A2A 返回了不安全的路径标识符"));
    }
    Ok(())
}

fn credential_location(credential: &A2aCredentialInput) -> A2aCredentialLocation {
    credential.location.unwrap_or_else(|| {
        credential
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("location"))
            .and_then(Value::as_str)
            .and_then(|location| match location.to_ascii_lowercase().as_str() {
                "query" => Some(A2aCredentialLocation::Query),
                "cookie" => Some(A2aCredentialLocation::Cookie),
                "header" => Some(A2aCredentialLocation::Header),
                _ => None,
            })
            .unwrap_or_default()
    })
}

fn credential_name<'a>(credential: &'a A2aCredentialInput, message: &str) -> Result<&'a str, AgentError> {
    credential
        .header_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| AgentError::bad_request(message))
}

fn validate_cookie_component(value: &str, message: &str) -> Result<(), AgentError> {
    if value.is_empty()
        || value
            .bytes()
            .any(|byte| byte.is_ascii_control() || matches!(byte, b';' | b',' | b'=' | b' '))
    {
        return Err(AgentError::bad_request(message));
    }
    Ok(())
}

fn append_optional_query(url: &mut Url, name: &str, value: Option<&str>) {
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        url.query_pairs_mut().append_pair(name, value);
    }
}

impl A2aEventStream {
    fn new(response: reqwest::Response, json_rpc_envelope: bool) -> Result<Self, AgentError> {
        let is_sse = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.to_ascii_lowercase().starts_with("text/event-stream"));
        if !is_sse {
            return Err(AgentError::bad_gateway("A2A 流式接口未返回 text/event-stream"));
        }
        Ok(Self {
            response,
            json_rpc_envelope,
            buffer: String::new(),
            last_event_id: None,
        })
    }

    async fn next_event(&mut self) -> Result<Option<StreamItem>, AgentError> {
        loop {
            if let Some(boundary) = find_event_boundary(&self.buffer) {
                let raw = self.buffer[..boundary.start].to_owned();
                self.buffer.drain(..boundary.end);
                if let Some(item) = parse_sse_event(&raw, self.json_rpc_envelope, &mut self.last_event_id)? {
                    return Ok(Some(item));
                }
                continue;
            }
            let Some(chunk) = self
                .response
                .chunk()
                .await
                .map_err(|_| AgentError::bad_gateway("读取 A2A 事件流失败"))?
            else {
                if self.buffer.trim().is_empty() {
                    return Ok(None);
                }
                let raw = std::mem::take(&mut self.buffer);
                return parse_sse_event(&raw, self.json_rpc_envelope, &mut self.last_event_id);
            };
            if self.buffer.len().saturating_add(chunk.len()) > MAX_SSE_EVENT_BYTES {
                return Err(AgentError::bad_gateway("A2A SSE 事件超过 2 MiB 限制"));
            }
            self.buffer
                .push_str(std::str::from_utf8(&chunk).map_err(|_| AgentError::bad_gateway("A2A SSE 不是 UTF-8"))?);
        }
    }
}

#[async_trait::async_trait]
impl IA2aEventStream for A2aEventStream {
    async fn next(&mut self) -> Result<Option<StreamItem>, AgentError> {
        self.next_event().await
    }
}

#[async_trait::async_trait]
impl IA2aClient for A2aClient {
    async fn send_message(&self, request: &a2a::SendMessageRequest) -> Result<a2a::SendMessageResponse, AgentError> {
        A2aClient::send_message(self, request).await
    }

    async fn send_streaming_message(
        &self,
        request: &a2a::SendMessageRequest,
    ) -> Result<Box<dyn IA2aEventStream>, AgentError> {
        Ok(Box::new(A2aClient::send_streaming_message(self, request).await?))
    }

    async fn get_task(&self, task_id: &str) -> Result<a2a::Task, AgentError> {
        A2aClient::get_task(self, task_id).await
    }

    async fn list_tasks(&self, request: &a2a::ListTasksRequest) -> Result<a2a::ListTasksResponse, AgentError> {
        A2aClient::list_tasks(self, request).await
    }

    async fn subscribe_to_task(
        &self,
        task_id: &str,
        last_event_id: Option<&str>,
    ) -> Result<Box<dyn IA2aEventStream>, AgentError> {
        Ok(Box::new(
            A2aClient::subscribe_to_task(self, task_id, last_event_id).await?,
        ))
    }

    async fn cancel_task(&self, task_id: &str) -> Result<a2a::Task, AgentError> {
        A2aClient::cancel_task(self, task_id).await
    }

    async fn get_extended_agent_card(&self) -> Result<a2a::AgentCard, AgentError> {
        A2aClient::get_extended_agent_card(self).await
    }

    async fn create_push_config(
        &self,
        config: &a2a::TaskPushNotificationConfig,
    ) -> Result<a2a::TaskPushNotificationConfig, AgentError> {
        A2aClient::create_push_config(self, config).await
    }

    async fn get_push_config(
        &self,
        request: &a2a::GetTaskPushNotificationConfigRequest,
    ) -> Result<a2a::TaskPushNotificationConfig, AgentError> {
        A2aClient::get_push_config(self, request).await
    }

    async fn list_push_configs(
        &self,
        request: &a2a::ListTaskPushNotificationConfigsRequest,
    ) -> Result<a2a::ListTaskPushNotificationConfigsResponse, AgentError> {
        A2aClient::list_push_configs(self, request).await
    }

    async fn delete_push_config(
        &self,
        request: &a2a::DeleteTaskPushNotificationConfigRequest,
    ) -> Result<(), AgentError> {
        A2aClient::delete_push_config(self, request).await
    }
}

struct EventBoundary {
    start: usize,
    end: usize,
}

fn find_event_boundary(buffer: &str) -> Option<EventBoundary> {
    let lf = buffer.find("\n\n").map(|start| EventBoundary { start, end: start + 2 });
    let crlf = buffer
        .find("\r\n\r\n")
        .map(|start| EventBoundary { start, end: start + 4 });
    match (lf, crlf) {
        (Some(left), Some(right)) => Some(if left.start <= right.start { left } else { right }),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn parse_sse_event(
    raw: &str,
    json_rpc_envelope: bool,
    last_event_id: &mut Option<String>,
) -> Result<Option<StreamItem>, AgentError> {
    let mut data = Vec::new();
    let mut event_id = None;
    for line in raw.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with(':') {
            continue;
        }
        if let Some(value) = line.strip_prefix("data:") {
            data.push(value.strip_prefix(' ').unwrap_or(value));
        } else if let Some(value) = line.strip_prefix("id:") {
            event_id = Some(value.trim().to_owned());
        }
    }
    if let Some(event_id) = event_id {
        *last_event_id = Some(event_id);
    }
    if data.is_empty() {
        return Ok(None);
    }
    let data = data.join("\n");
    let value: Value =
        serde_json::from_str(&data).map_err(|_| AgentError::bad_gateway("A2A SSE data 不是有效 JSON"))?;
    let event = if json_rpc_envelope {
        let rpc: a2a::JsonRpcResponse =
            serde_json::from_value(value).map_err(|_| AgentError::bad_gateway("A2A SSE JSON-RPC 响应无效"))?;
        if let Some(error) = rpc.error {
            return Err(json_rpc_error(error));
        }
        serde_json::from_value(
            rpc.result
                .ok_or_else(|| AgentError::bad_gateway("A2A SSE JSON-RPC 缺少 result"))?,
        )
        .map_err(|_| AgentError::bad_gateway("A2A SSE result 无效"))?
    } else {
        serde_json::from_value(value).map_err(|_| AgentError::bad_gateway("A2A SSE 事件格式无效"))?
    };
    Ok(Some(StreamItem {
        event,
        event_id: last_event_id.clone(),
    }))
}

async fn checked_response(response: reqwest::Response) -> Result<reqwest::Response, AgentError> {
    match response.status() {
        StatusCode::UNAUTHORIZED => Err(AgentError::unauthorized("A2A Agent 需要认证")),
        StatusCode::FORBIDDEN => Err(AgentError::forbidden("A2A Agent 拒绝访问")),
        StatusCode::TOO_MANY_REQUESTS => Err(AgentError::RateLimited),
        status if status.is_redirection() => Err(AgentError::bad_gateway("A2A 会话接口不允许重定向")),
        status if !status.is_success() => Err(AgentError::bad_gateway(format!(
            "A2A Agent 返回 HTTP {}",
            status.as_u16()
        ))),
        _ => Ok(response),
    }
}

async fn decode_json_response<R: DeserializeOwned>(
    mut response: reqwest::Response,
    label: &str,
) -> Result<R, AgentError> {
    if response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|length| length > MAX_JSON_RESPONSE_BYTES)
    {
        return Err(AgentError::bad_gateway(format!("{label}超过 2 MiB 限制")));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| AgentError::bad_gateway(format!("读取{label}失败")))?
    {
        if body.len().saturating_add(chunk.len()) > MAX_JSON_RESPONSE_BYTES {
            return Err(AgentError::bad_gateway(format!("{label}超过 2 MiB 限制")));
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| AgentError::bad_gateway(format!("{label}格式无效")))
}

fn append_path(base: &Url, suffix: &str) -> Result<Url, AgentError> {
    Url::parse(&format!(
        "{}/{}",
        base.as_str().trim_end_matches('/'),
        suffix.trim_start_matches('/')
    ))
    .map_err(|_| AgentError::internal("无法构造 A2A HTTP+JSON 地址"))
}

fn to_value(value: &impl Serialize) -> Result<Value, AgentError> {
    serde_json::to_value(value).map_err(|_| AgentError::internal("无法编码 A2A 请求"))
}

fn json_rpc_error(error: a2a::JsonRpcError) -> AgentError {
    match error.code {
        -32001 => AgentError::not_found("A2A 任务不存在"),
        -32002 => AgentError::conflict("A2A 任务当前无法取消"),
        -32003 => AgentError::bad_request("A2A Agent 不支持推送通知"),
        -32004 => AgentError::bad_request("A2A Agent 不支持该操作"),
        -32005 => AgentError::bad_request("A2A Agent 不支持该内容类型"),
        -32006 => AgentError::bad_gateway("A2A Agent 返回了无效响应"),
        -32007 => AgentError::not_found("A2A Agent 未配置扩展 Agent Card"),
        -32008 => AgentError::bad_request("A2A Agent 要求客户端支持扩展"),
        -32009 => AgentError::bad_request("A2A 协议版本不受支持"),
        -32603 => AgentError::bad_gateway("A2A Agent 内部错误"),
        -32600 | -32602 => AgentError::bad_request("A2A 请求参数无效"),
        -32601 => AgentError::bad_request("A2A Agent 不支持该方法"),
        _ => AgentError::bad_gateway(format!("A2A 上游协议错误（代码 {}）", error.code)),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::{Mutex, oneshot};

    use super::*;

    async fn spawn_raw_http_once(response: String) -> (Url, oneshot::Receiver<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (request_tx, request_rx) = oneshot::channel();
        let request_tx = Arc::new(Mutex::new(Some(request_tx)));
        tokio::spawn(async move {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let mut request = vec![0_u8; 64 * 1024];
            let bytes_read = stream.read(&mut request).await.unwrap_or_default();
            request.truncate(bytes_read);
            if let Some(sender) = request_tx.lock().await.take() {
                let _ = sender.send(String::from_utf8_lossy(&request).into_owned());
            }
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.shutdown().await;
        });
        (Url::parse(&format!("http://{address}/")).unwrap(), request_rx)
    }

    fn local_jsonrpc_client(endpoint: Url, credential: Option<A2aCredentialInput>) -> A2aClient {
        A2aClient::new(A2aClientConfig {
            endpoint,
            binding: A2aBinding::JsonRpc,
            credentials: credential.into_iter().collect(),
            tenant: None,
            compatibility_mode: A2aCompatibilityMode::V1,
            extensions: Vec::new(),
            allow_insecure: true,
            allow_private_network: true,
        })
        .unwrap()
    }

    fn probe_request() -> a2a::SendMessageRequest {
        a2a::SendMessageRequest {
            message: a2a::Message::new(a2a::Role::User, vec![a2a::Part::text("probe")]),
            configuration: None,
            metadata: None,
            tenant: None,
        }
    }

    #[test]
    fn parses_multiline_sse_and_event_id() {
        let event = r#"id: 42
data: {"message":{"messageId":"m1","role":"ROLE_AGENT",
data: "parts":[{"text":"hello"}]}}"#;
        let mut last_id = None;
        let item = parse_sse_event(event, false, &mut last_id).unwrap().unwrap();
        assert_eq!(item.event_id.as_deref(), Some("42"));
        assert!(matches!(item.event, a2a::StreamResponse::Message(_)));
    }

    #[test]
    fn rejects_unsafe_task_id() {
        let config = A2aClientConfig {
            endpoint: Url::parse("https://agent.example").unwrap(),
            binding: A2aBinding::HttpJson,
            credentials: Vec::new(),
            tenant: None,
            compatibility_mode: A2aCompatibilityMode::V1,
            extensions: Vec::new(),
            allow_insecure: false,
            allow_private_network: false,
        };
        let client = A2aClient::new(config).unwrap();
        assert!(client.http_json_task_url("../secret", None).is_err());
    }

    #[test]
    fn query_api_key_is_added_only_to_request_clone() {
        let endpoint = Url::parse("https://agent.example/a2a?tenant=one").unwrap();
        let client = A2aClient::new(A2aClientConfig {
            endpoint: endpoint.clone(),
            binding: A2aBinding::JsonRpc,
            credentials: vec![A2aCredentialInput {
                kind: A2aAuthKind::ApiKey,
                scheme_name: Some("queryKey".to_owned()),
                header_name: Some("api_key".to_owned()),
                location: Some(A2aCredentialLocation::Query),
                secret: Some("not-for-logs".to_owned()),
                metadata: Some(serde_json::json!({ "location": "query" })),
            }],
            tenant: None,
            compatibility_mode: A2aCompatibilityMode::V1,
            extensions: Vec::new(),
            allow_insecure: false,
            allow_private_network: false,
        })
        .unwrap();

        let request_url = client.authenticated_url(&endpoint).unwrap();
        assert_eq!(endpoint.as_str(), "https://agent.example/a2a?tenant=one");
        assert_eq!(
            request_url
                .query_pairs()
                .find(|(name, _)| name == "api_key")
                .map(|(_, value)| value.into_owned())
                .as_deref(),
            Some("not-for-logs")
        );
        assert!(!client.config.endpoint.as_str().contains("not-for-logs"));
    }

    #[test]
    fn query_api_key_rejects_ambiguous_existing_parameter() {
        let endpoint = Url::parse("https://agent.example/a2a?api_key=public").unwrap();
        let client = A2aClient::new(A2aClientConfig {
            endpoint: endpoint.clone(),
            binding: A2aBinding::JsonRpc,
            credentials: vec![A2aCredentialInput {
                kind: A2aAuthKind::ApiKey,
                scheme_name: Some("queryKey".to_owned()),
                header_name: Some("api_key".to_owned()),
                location: Some(A2aCredentialLocation::Query),
                secret: Some("secret".to_owned()),
                metadata: Some(serde_json::json!({ "location": "query" })),
            }],
            tenant: None,
            compatibility_mode: A2aCompatibilityMode::V1,
            extensions: Vec::new(),
            allow_insecure: false,
            allow_private_network: false,
        })
        .unwrap();
        assert!(client.authenticated_url(&endpoint).is_err());
    }

    #[test]
    fn static_auth_headers_are_applied_without_touching_none_or_query_credentials() {
        let endpoint = Url::parse("https://agent.example/a2a").unwrap();
        let cases = [
            (
                A2aCredentialInput {
                    kind: A2aAuthKind::None,
                    scheme_name: None,
                    header_name: None,
                    location: None,
                    secret: None,
                    metadata: None,
                },
                None,
                None,
            ),
            (
                A2aCredentialInput {
                    kind: A2aAuthKind::Bearer,
                    scheme_name: Some("bearerAuth".to_owned()),
                    header_name: None,
                    location: None,
                    secret: Some("bearer-secret".to_owned()),
                    metadata: None,
                },
                Some("authorization"),
                Some("Bearer bearer-secret"),
            ),
            (
                A2aCredentialInput {
                    kind: A2aAuthKind::Basic,
                    scheme_name: Some("basicAuth".to_owned()),
                    header_name: None,
                    location: None,
                    secret: Some("user:password".to_owned()),
                    metadata: None,
                },
                Some("authorization"),
                Some("Basic dXNlcjpwYXNzd29yZA=="),
            ),
            (
                A2aCredentialInput {
                    kind: A2aAuthKind::ApiKey,
                    scheme_name: Some("headerKey".to_owned()),
                    header_name: Some("X-API-Key".to_owned()),
                    location: Some(A2aCredentialLocation::Header),
                    secret: Some("api-secret".to_owned()),
                    metadata: None,
                },
                Some("x-api-key"),
                Some("api-secret"),
            ),
            (
                A2aCredentialInput {
                    kind: A2aAuthKind::ApiKey,
                    scheme_name: Some("queryKey".to_owned()),
                    header_name: Some("api_key".to_owned()),
                    location: Some(A2aCredentialLocation::Query),
                    secret: Some("query-secret".to_owned()),
                    metadata: Some(serde_json::json!({ "location": "query" })),
                },
                None,
                None,
            ),
        ];

        for (credential, expected_name, expected_value) in cases {
            let client = local_jsonrpc_client(endpoint.clone(), Some(credential));
            let request = client
                .apply_headers(Client::new().get(endpoint.clone()))
                .unwrap()
                .build()
                .unwrap();
            match (expected_name, expected_value) {
                (Some(name), Some(value)) => {
                    assert_eq!(request.headers().get(name).unwrap(), value);
                }
                _ => assert!(request.headers().get(AUTHORIZATION).is_none()),
            }
        }
    }

    #[tokio::test]
    async fn session_redirect_is_rejected_without_forwarding_credentials() {
        let (redirect_target, redirected_request) =
            spawn_raw_http_once("HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n".to_owned()).await;
        let response = format!(
            "HTTP/1.1 302 Found\r\nLocation: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            redirect_target
        );
        let (endpoint, original_request) = spawn_raw_http_once(response).await;
        let client = local_jsonrpc_client(
            endpoint,
            Some(A2aCredentialInput {
                kind: A2aAuthKind::Bearer,
                scheme_name: Some("bearerAuth".to_owned()),
                header_name: None,
                location: None,
                secret: Some("must-not-leak".to_owned()),
                metadata: None,
            }),
        );

        assert!(client.send_message(&probe_request()).await.is_err());
        let original = original_request.await.unwrap();
        assert!(
            original
                .to_ascii_lowercase()
                .contains("authorization: bearer must-not-leak")
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(250), redirected_request)
                .await
                .is_err(),
            "redirect target must never receive a request"
        );
    }

    #[tokio::test]
    async fn oversized_json_response_is_rejected_from_content_length() {
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{{}}",
            MAX_JSON_RESPONSE_BYTES + 1
        );
        let (endpoint, request) = spawn_raw_http_once(response).await;
        let client = local_jsonrpc_client(endpoint, None);
        let error = client.send_message(&probe_request()).await.unwrap_err();
        let _ = request.await;
        assert!(error.to_string().contains("2 MiB"));
    }

    #[tokio::test]
    async fn list_tasks_uses_v1_method_and_preserves_filters() {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "response",
            "result": {
                "tasks": [],
                "nextPageToken": "",
                "pageSize": 25,
                "totalSize": 0
            }
        })
        .to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let (endpoint, request_rx) = spawn_raw_http_once(response).await;
        let client = local_jsonrpc_client(endpoint, None);
        let result = <A2aClient as IA2aClient>::list_tasks(
            &client,
            &a2a::ListTasksRequest {
                context_id: Some("context-1".to_owned()),
                status: Some(a2a::TaskState::Working),
                page_size: Some(25),
                page_token: Some("page-1".to_owned()),
                history_length: Some(5),
                status_timestamp_after: None,
                include_artifacts: Some(true),
                tenant: Some("tenant-1".to_owned()),
            },
        )
        .await
        .unwrap();
        let request = request_rx.await.unwrap();

        assert_eq!(result.page_size, 25);
        assert!(request.contains(r#""method":"ListTasks""#));
        assert!(request.contains(r#""contextId":"context-1""#));
        assert!(request.contains(r#""tenant":"tenant-1""#));
    }

    /// Interoperability probe for the official `a2aproject/a2a-samples`
    /// JavaScript v1 Hello World Agent.
    ///
    /// Start that sample on port 9999, then run:
    /// `cargo test -p tjuaeui-ai-agent official_v1_sample_interoperability -- --ignored`
    #[tokio::test]
    #[ignore = "requires the official A2A v1 Hello World sample on localhost:9999"]
    async fn official_v1_sample_interoperability() {
        let base_url = std::env::var("A2A_OFFICIAL_SAMPLE_URL").unwrap_or_else(|_| "http://127.0.0.1:9999/".to_owned());
        let card_url = Url::parse(&base_url)
            .unwrap()
            .join(".well-known/agent-card.json")
            .unwrap();
        let card = reqwest::get(card_url)
            .await
            .expect("fetch official card")
            .json::<a2a::AgentCard>()
            .await
            .expect("decode official v1 card");
        assert_eq!(card.name, "Hello World Agent");
        assert_eq!(card.capabilities.streaming, Some(true));

        let client = A2aClient::new(A2aClientConfig {
            endpoint: Url::parse(&base_url).unwrap(),
            binding: A2aBinding::JsonRpc,
            credentials: Vec::new(),
            tenant: None,
            compatibility_mode: A2aCompatibilityMode::V1,
            extensions: Vec::new(),
            allow_insecure: true,
            allow_private_network: true,
        })
        .unwrap();
        let request = a2a::SendMessageRequest {
            message: a2a::Message::new(a2a::Role::User, vec![a2a::Part::text("Tjuae interoperability")]),
            configuration: Some(a2a::SendMessageConfiguration {
                accepted_output_modes: Some(vec!["text/plain".to_owned()]),
                task_push_notification_config: None,
                history_length: Some(10),
                return_immediately: Some(false),
            }),
            metadata: None,
            tenant: None,
        };

        let response = client.send_message(&request).await.expect("send message");
        let task_id = match response {
            a2a::SendMessageResponse::Task(task) => {
                assert_eq!(task.status.state, a2a::TaskState::Completed);
                task.id
            }
            a2a::SendMessageResponse::Message(message) => {
                panic!(
                    "official sample unexpectedly returned direct message {}",
                    message.message_id
                )
            }
        };
        let fetched = client.get_task(&task_id).await.expect("get task");
        assert_eq!(fetched.status.state, a2a::TaskState::Completed);
        assert!(fetched.artifacts.is_some());

        let mut stream = client
            .send_streaming_message(&a2a::SendMessageRequest {
                message: a2a::Message::new(a2a::Role::User, vec![a2a::Part::text("Tjuae streaming")]),
                ..request.clone()
            })
            .await
            .expect("open stream");
        let mut stream_events = Vec::new();
        while let Some(item) = stream.next_event().await.expect("read stream event") {
            let terminal = matches!(
                &item.event,
                a2a::StreamResponse::StatusUpdate(update)
                    if update.status.state == a2a::TaskState::Completed
            );
            stream_events.push(item.event);
            if terminal {
                break;
            }
        }
        assert!(stream_events.len() >= 3);
        assert!(
            stream_events
                .iter()
                .any(|event| matches!(event, a2a::StreamResponse::ArtifactUpdate(_)))
        );

        let extended = client.get_extended_agent_card().await.expect("get extended agent card");
        assert!(extended.skills.len() > card.skills.len());

        // The official Hello World sample intentionally rejects cancellation.
        // Reaching a protocol-level error proves the CancelTask operation and
        // error mapping are interoperable rather than silently unsupported.
        assert!(client.cancel_task(&task_id).await.is_err());
    }

    /// Real HTTP+JSON/REST interoperability probe using the official
    /// `@a2a-js/sdk` v1 server transport.
    #[tokio::test]
    #[ignore = "requires the official JS SDK multi-transport harness on localhost:9998"]
    async fn official_v1_http_json_interoperability() {
        let client = A2aClient::new(A2aClientConfig {
            endpoint: Url::parse("http://127.0.0.1:9998/").unwrap(),
            binding: A2aBinding::HttpJson,
            credentials: Vec::new(),
            tenant: None,
            compatibility_mode: A2aCompatibilityMode::V1,
            extensions: Vec::new(),
            allow_insecure: true,
            allow_private_network: true,
        })
        .unwrap();
        let request = a2a::SendMessageRequest {
            message: a2a::Message::new(a2a::Role::User, vec![a2a::Part::text("Tjuae HTTP+JSON")]),
            configuration: Some(a2a::SendMessageConfiguration {
                accepted_output_modes: Some(vec!["text/plain".to_owned()]),
                task_push_notification_config: None,
                history_length: Some(10),
                return_immediately: Some(false),
            }),
            metadata: None,
            tenant: None,
        };
        let response = client.send_message(&request).await.expect("HTTP+JSON send");
        let task_id = match response {
            a2a::SendMessageResponse::Task(task) => {
                assert_eq!(task.status.state, a2a::TaskState::Completed);
                task.id
            }
            a2a::SendMessageResponse::Message(message) => {
                panic!("unexpected direct message {}", message.message_id)
            }
        };
        assert_eq!(
            client
                .get_task(&task_id)
                .await
                .expect("HTTP+JSON get task")
                .status
                .state,
            a2a::TaskState::Completed
        );

        let mut stream = client
            .send_streaming_message(&a2a::SendMessageRequest {
                message: a2a::Message::new(a2a::Role::User, vec![a2a::Part::text("Tjuae HTTP stream")]),
                ..request
            })
            .await
            .expect("HTTP+JSON stream");
        let mut saw_artifact = false;
        let mut saw_completed = false;
        while let Some(item) = stream.next_event().await.expect("HTTP+JSON stream event") {
            saw_artifact |= matches!(&item.event, a2a::StreamResponse::ArtifactUpdate(_));
            saw_completed |= matches!(
                &item.event,
                a2a::StreamResponse::StatusUpdate(update)
                    if update.status.state == a2a::TaskState::Completed
            );
            if saw_completed {
                break;
            }
        }
        assert!(saw_artifact && saw_completed);
        assert!(
            client
                .get_extended_agent_card()
                .await
                .expect("HTTP+JSON extended card")
                .name
                .ends_with("Extended")
        );
        assert!(client.cancel_task(&task_id).await.is_err());
    }
}
