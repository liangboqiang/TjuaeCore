use base64::Engine;
use futures_util::StreamExt;
use tjuaeui_api_types::{A2aAuthKind, A2aCredentialInput, A2aCredentialLocation};

use a2a_client::{ServiceParams, Transport};
use a2a_grpc::GrpcTransport;
use hyper_util::rt::TokioIo;
use tonic::transport::{Certificate, ClientTlsConfig, Endpoint, Identity};

use crate::error::AgentError;
use crate::services::a2a::security::{A2aNetworkPolicy, mtls_material, resolve_network_target};

use super::client::{A2aClientConfig, IA2aClient, IA2aEventStream, StreamItem};

pub(crate) struct GrpcA2aClient {
    transport: GrpcTransport,
    service_params: ServiceParams,
    tenant: Option<String>,
}

struct GrpcEventStream {
    inner: a2a_client::BoxStream<'static, Result<a2a::StreamResponse, a2a::A2AError>>,
}

impl GrpcA2aClient {
    pub(crate) async fn connect(config: A2aClientConfig) -> Result<Self, AgentError> {
        let addresses = resolve_network_target(
            &config.endpoint,
            A2aNetworkPolicy {
                allow_insecure: config.allow_insecure,
                allow_private_network: config.allow_private_network,
            },
        )
        .await?;
        let service_params = service_params(&config.credentials, &config.extensions)?;
        let mut endpoint = Endpoint::from_shared(config.endpoint.to_string())
            .map_err(|_| AgentError::bad_request("A2A gRPC 接口 URL 无效"))?
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(60));
        if config.endpoint.scheme() == "https" {
            let host = config
                .endpoint
                .host_str()
                .ok_or_else(|| AgentError::bad_request("A2A gRPC 地址缺少主机名"))?;
            let mut tls = ClientTlsConfig::new().with_native_roots().domain_name(host.to_owned());
            let mtls = config
                .credentials
                .iter()
                .filter(|credential| credential.kind == A2aAuthKind::Mtls)
                .collect::<Vec<_>>();
            if mtls.len() > 1 {
                return Err(AgentError::bad_request("单个 A2A 连接只能配置一份 mTLS 身份"));
            }
            if let Some(material) = mtls_material(mtls.first().copied())? {
                tls = tls.identity(Identity::from_pem(
                    material.client_certificate_pem,
                    material.private_key_pem,
                ));
                if let Some(ca_certificate_pem) = material.ca_certificate_pem {
                    tls = tls.ca_certificate(Certificate::from_pem(ca_certificate_pem));
                }
            }
            endpoint = endpoint
                .tls_config(tls)
                .map_err(|_| AgentError::bad_request("A2A gRPC TLS 配置无效"))?;
        } else if config.credentials.iter().any(|value| value.kind == A2aAuthKind::Mtls) {
            return Err(AgentError::bad_request("mTLS 只能用于 HTTPS A2A gRPC 地址"));
        }
        let channel = endpoint
            .connect_with_connector(tower::service_fn(move |_| {
                let addresses = addresses.clone();
                async move { connect_pinned(addresses).await }
            }))
            .await
            .map_err(|error| AgentError::bad_gateway(format!("A2A gRPC 连接失败：{error}")))?;
        let transport = GrpcTransport::from_channel(channel);
        Ok(Self {
            transport,
            service_params,
            tenant: config.tenant,
        })
    }

    fn params_with_last_event_id(&self, last_event_id: Option<&str>) -> ServiceParams {
        let mut params = self.service_params.clone();
        if let Some(last_event_id) = last_event_id {
            params.insert("last-event-id".to_owned(), vec![last_event_id.to_owned()]);
        }
        params
    }
}

#[async_trait::async_trait]
impl IA2aClient for GrpcA2aClient {
    async fn send_message(&self, request: &a2a::SendMessageRequest) -> Result<a2a::SendMessageResponse, AgentError> {
        let mut request = request.clone();
        request.tenant = request.tenant.or_else(|| self.tenant.clone());
        self.transport
            .send_message(&self.service_params, &request)
            .await
            .map_err(map_a2a_error)
    }

    async fn send_streaming_message(
        &self,
        request: &a2a::SendMessageRequest,
    ) -> Result<Box<dyn IA2aEventStream>, AgentError> {
        let mut request = request.clone();
        request.tenant = request.tenant.or_else(|| self.tenant.clone());
        let inner = self
            .transport
            .send_streaming_message(&self.service_params, &request)
            .await
            .map_err(map_a2a_error)?;
        Ok(Box::new(GrpcEventStream { inner }))
    }

    async fn get_task(&self, task_id: &str) -> Result<a2a::Task, AgentError> {
        self.transport
            .get_task(
                &self.service_params,
                &a2a::GetTaskRequest {
                    id: task_id.to_owned(),
                    history_length: None,
                    tenant: self.tenant.clone(),
                },
            )
            .await
            .map_err(map_a2a_error)
    }

    async fn list_tasks(&self, request: &a2a::ListTasksRequest) -> Result<a2a::ListTasksResponse, AgentError> {
        let mut request = request.clone();
        request.tenant = request.tenant.or_else(|| self.tenant.clone());
        self.transport
            .list_tasks(&self.service_params, &request)
            .await
            .map_err(map_a2a_error)
    }

    async fn subscribe_to_task(
        &self,
        task_id: &str,
        last_event_id: Option<&str>,
    ) -> Result<Box<dyn IA2aEventStream>, AgentError> {
        let params = self.params_with_last_event_id(last_event_id);
        let inner = self
            .transport
            .subscribe_to_task(
                &params,
                &a2a::SubscribeToTaskRequest {
                    id: task_id.to_owned(),
                    tenant: self.tenant.clone(),
                },
            )
            .await
            .map_err(map_a2a_error)?;
        Ok(Box::new(GrpcEventStream { inner }))
    }

    async fn cancel_task(&self, task_id: &str) -> Result<a2a::Task, AgentError> {
        self.transport
            .cancel_task(
                &self.service_params,
                &a2a::CancelTaskRequest {
                    id: task_id.to_owned(),
                    metadata: None,
                    tenant: self.tenant.clone(),
                },
            )
            .await
            .map_err(map_a2a_error)
    }

    async fn get_extended_agent_card(&self) -> Result<a2a::AgentCard, AgentError> {
        self.transport
            .get_extended_agent_card(
                &self.service_params,
                &a2a::GetExtendedAgentCardRequest {
                    tenant: self.tenant.clone(),
                },
            )
            .await
            .map_err(map_a2a_error)
    }

    async fn create_push_config(
        &self,
        config: &a2a::TaskPushNotificationConfig,
    ) -> Result<a2a::TaskPushNotificationConfig, AgentError> {
        self.transport
            .create_push_config(&self.service_params, config)
            .await
            .map_err(map_a2a_error)
    }

    async fn get_push_config(
        &self,
        request: &a2a::GetTaskPushNotificationConfigRequest,
    ) -> Result<a2a::TaskPushNotificationConfig, AgentError> {
        self.transport
            .get_push_config(&self.service_params, request)
            .await
            .map_err(map_a2a_error)
    }

    async fn list_push_configs(
        &self,
        request: &a2a::ListTaskPushNotificationConfigsRequest,
    ) -> Result<a2a::ListTaskPushNotificationConfigsResponse, AgentError> {
        self.transport
            .list_push_configs(&self.service_params, request)
            .await
            .map_err(map_a2a_error)
    }

    async fn delete_push_config(
        &self,
        request: &a2a::DeleteTaskPushNotificationConfigRequest,
    ) -> Result<(), AgentError> {
        self.transport
            .delete_push_config(&self.service_params, request)
            .await
            .map_err(map_a2a_error)
    }
}

#[async_trait::async_trait]
impl IA2aEventStream for GrpcEventStream {
    async fn next(&mut self) -> Result<Option<StreamItem>, AgentError> {
        self.inner
            .next()
            .await
            .transpose()
            .map(|event| event.map(|event| StreamItem { event, event_id: None }))
            .map_err(map_a2a_error)
    }
}

fn service_params(credentials: &[A2aCredentialInput], extensions: &[String]) -> Result<ServiceParams, AgentError> {
    let mut params = ServiceParams::new();
    params.insert("a2a-version".to_owned(), vec![a2a::VERSION.to_owned()]);
    if !extensions.is_empty() {
        params.insert("a2a-extensions".to_owned(), vec![extensions.join(",")]);
    }
    let mut authorization_set = false;
    for credential in credentials {
        let secret = credential.secret.as_deref().unwrap_or_default();
        match credential.kind {
            A2aAuthKind::None => {}
            A2aAuthKind::Bearer | A2aAuthKind::OAuth2 | A2aAuthKind::Oidc => {
                if authorization_set {
                    return Err(AgentError::bad_request("A2A 凭据包含多个互斥的 Authorization 方案"));
                }
                params.insert("authorization".to_owned(), vec![format!("Bearer {secret}")]);
                authorization_set = true;
            }
            A2aAuthKind::Basic => {
                if authorization_set {
                    return Err(AgentError::bad_request("A2A 凭据包含多个互斥的 Authorization 方案"));
                }
                let encoded = base64::engine::general_purpose::STANDARD.encode(secret);
                params.insert("authorization".to_owned(), vec![format!("Basic {encoded}")]);
                authorization_set = true;
            }
            A2aAuthKind::ApiKey
                if credential.location.is_some_and(|location| {
                    matches!(location, A2aCredentialLocation::Query | A2aCredentialLocation::Cookie)
                }) =>
            {
                return Err(AgentError::bad_request("Query/Cookie API Key 不能用于 gRPC A2A 接口"));
            }
            A2aAuthKind::ApiKey | A2aAuthKind::CustomHeader => {
                let name = credential
                    .header_name
                    .as_deref()
                    .ok_or_else(|| AgentError::bad_request("A2A 凭据缺少 Header 名称"))?
                    .to_ascii_lowercase();
                if !name.bytes().all(|byte| {
                    byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
                }) {
                    return Err(AgentError::bad_request("A2A gRPC 凭据 Metadata 名称无效"));
                }
                params.insert(name, vec![secret.to_owned()]);
            }
            A2aAuthKind::Mtls => {
                mtls_material(Some(credential))?;
            }
        }
    }
    Ok(params)
}

async fn connect_pinned(addresses: Vec<std::net::SocketAddr>) -> std::io::Result<TokioIo<tokio::net::TcpStream>> {
    let mut last_error = None;
    for address in addresses {
        match tokio::net::TcpStream::connect(address).await {
            Ok(stream) => return Ok(TokioIo::new(stream)),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error
        .unwrap_or_else(|| std::io::Error::new(std::io::ErrorKind::AddrNotAvailable, "没有可用的 A2A gRPC 地址")))
}

fn map_a2a_error(error: a2a::A2AError) -> AgentError {
    match error.code {
        -32001 => AgentError::not_found("A2A 任务不存在"),
        -32002 => AgentError::conflict("A2A 任务当前无法取消"),
        -32003 => AgentError::bad_request("A2A Agent 不支持推送通知"),
        -32004 | -32601 => AgentError::bad_request("A2A Agent 不支持该操作"),
        -32005 => AgentError::bad_request("A2A Agent 不支持该内容类型"),
        -32007 => AgentError::not_found("A2A Agent 未配置扩展 Agent Card"),
        -32008 => AgentError::bad_request("A2A Agent 要求客户端支持扩展"),
        -32009 => AgentError::bad_request("A2A 协议版本不受支持"),
        -32600 | -32602 => AgentError::bad_request("A2A 请求参数无效"),
        _ => AgentError::bad_gateway(format!("A2A gRPC 上游错误（代码 {}）：{}", error.code, error.message)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_bearer_metadata() {
        let credential = A2aCredentialInput {
            kind: A2aAuthKind::Bearer,
            scheme_name: Some("bearerAuth".to_owned()),
            header_name: None,
            location: None,
            secret: Some("secret".to_owned()),
            metadata: None,
        };
        let params = service_params(&[credential], &[]).unwrap();
        assert_eq!(params["authorization"], vec!["Bearer secret"]);
        assert_eq!(params["a2a-version"], vec![a2a::VERSION]);
    }

    #[test]
    fn rejects_invalid_grpc_metadata_name() {
        let credential = A2aCredentialInput {
            kind: A2aAuthKind::CustomHeader,
            scheme_name: Some("customAuth".to_owned()),
            header_name: Some("Bad Header".to_owned()),
            location: None,
            secret: Some("secret".to_owned()),
            metadata: None,
        };
        let result = service_params(&[credential], &[]);
        assert!(result.is_err());
    }

    /// Real gRPC interoperability probe using the official
    /// `@a2a-js/sdk` v1 server transport.
    #[tokio::test]
    #[ignore = "requires the official JS SDK multi-transport harness on localhost:50051"]
    async fn official_v1_grpc_interoperability() {
        let client = GrpcA2aClient::connect(A2aClientConfig {
            endpoint: url::Url::parse("http://127.0.0.1:50051").unwrap(),
            binding: tjuaeui_api_types::A2aBinding::Grpc,
            credentials: Vec::new(),
            tenant: None,
            compatibility_mode: tjuaeui_api_types::A2aCompatibilityMode::V1,
            extensions: Vec::new(),
            allow_insecure: true,
            allow_private_network: true,
        })
        .await
        .expect("connect gRPC");
        let request = a2a::SendMessageRequest {
            message: a2a::Message::new(a2a::Role::User, vec![a2a::Part::text("Tjuae gRPC")]),
            configuration: Some(a2a::SendMessageConfiguration {
                accepted_output_modes: Some(vec!["text/plain".to_owned()]),
                task_push_notification_config: None,
                history_length: Some(10),
                return_immediately: Some(false),
            }),
            metadata: None,
            tenant: None,
        };

        let response = client.send_message(&request).await.expect("gRPC send");
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
            client.get_task(&task_id).await.expect("gRPC get task").status.state,
            a2a::TaskState::Completed
        );

        let mut stream = client
            .send_streaming_message(&a2a::SendMessageRequest {
                message: a2a::Message::new(a2a::Role::User, vec![a2a::Part::text("Tjuae gRPC stream")]),
                ..request
            })
            .await
            .expect("gRPC stream");
        let mut saw_artifact = false;
        let mut saw_completed = false;
        while let Some(item) = stream.next().await.expect("gRPC stream event") {
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
                .expect("gRPC extended card")
                .name
                .ends_with("Extended")
        );
        assert!(client.cancel_task(&task_id).await.is_err());
    }
}
