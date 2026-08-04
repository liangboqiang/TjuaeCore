use std::path::{Path, PathBuf};
use std::sync::Arc;

use tjuaeui_api_types::{AgentHandshake, AgentMetadata};
use tjuaeui_common::EnvVar;
use tjuaeui_process::Spawner;
use tjuaeui_session::{
    BackendConnection, BackendError, ClaudeConnection, CodexConnection, SessionConfig, SessionInit, SessionSpec,
};

pub(super) enum DirectProbeFailure {
    Connection { code: String, message: String },
    Catalog(String),
}

pub(super) async fn probe_direct_session_catalog(
    meta: &AgentMetadata,
    spawner: Arc<dyn Spawner>,
) -> Result<AgentHandshake, DirectProbeFailure> {
    let backend = meta.backend.as_deref().ok_or_else(|| DirectProbeFailure::Connection {
        code: "configuration_missing".to_owned(),
        message: "Agent 缺少 backend 配置".to_owned(),
    })?;
    let workspace = DiagnosticWorkspace::create().map_err(|error| DirectProbeFailure::Connection {
        code: "diagnostic_workspace_failed".to_owned(),
        message: format!("创建诊断工作区失败：{error}"),
    })?;
    let session_id = format!("agent-diagnostic-{}", uuid::Uuid::now_v7());
    let connection: Box<dyn BackendConnection> = match backend {
        "claude" => Box::new(ClaudeConnection::new(spawner)),
        "codex" => Box::new(CodexConnection::new(spawner)),
        _ => {
            return Err(DirectProbeFailure::Connection {
                code: "unsupported_backend".to_owned(),
                message: format!("不支持诊断 backend：{backend}"),
            });
        }
    };

    let mut spawn_env: Vec<EnvVar> = meta
        .env
        .iter()
        .map(|entry| EnvVar {
            name: entry.name.clone(),
            value: entry.value.clone(),
        })
        .collect();
    if backend == "claude" {
        spawn_env.extend(
            crate::cc_switch::read_claude_provider_env()
                .into_iter()
                .map(|(name, value)| EnvVar { name, value }),
        );
    }

    let cli_program = if meta.has_command_override {
        meta.resolved_command.clone()
    } else {
        meta.resolved_command
            .clone()
            .or_else(|| tjuaeui_runtime::resolve_command_path(backend))
    };
    let config = SessionConfig {
        cwd: Some(workspace.path().to_string_lossy().into_owned()),
        init: SessionInit::default(),
        spawn_env,
        cli_program,
        ..Default::default()
    };
    let session = connection
        .open_session(
            SessionSpec::Fresh {
                session_id: session_id.clone(),
            },
            config,
        )
        .await
        .map_err(map_direct_backend_error)?;

    let catalog = wait_for_session_catalog(session.as_ref()).await;
    let _ = connection.close_session(&session_id).await;
    catalog
}

async fn wait_for_session_catalog(
    session: &dyn tjuaeui_session::SessionBackend,
) -> Result<AgentHandshake, DirectProbeFailure> {
    for _ in 0..100 {
        let capabilities = session.capabilities();
        if !capabilities.available_models.is_empty()
            && let Some(catalog) = crate::catalog::handshake_from_session_capabilities(&capabilities)
        {
            return Ok(catalog);
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    Err(DirectProbeFailure::Catalog(
        "Agent 已连接，但未能在 5 秒内返回模型目录".to_owned(),
    ))
}

fn map_direct_backend_error(error: BackendError) -> DirectProbeFailure {
    let code = match &error {
        BackendError::HandshakeTimeout(_) => "connection_timeout",
        BackendError::SetupRejected(_) => "setup_rejected",
        BackendError::WorkspaceUnavailable(_) => "diagnostic_workspace_failed",
        BackendError::Transport(_) => "process_start_failed",
        BackendError::SessionNotFound(_) => "session_create_failed",
        BackendError::CommandNotSupported { .. } => "protocol_incompatible",
    };
    DirectProbeFailure::Connection {
        code: code.to_owned(),
        message: error.to_string(),
    }
}

struct DiagnosticWorkspace {
    path: PathBuf,
}

impl DiagnosticWorkspace {
    fn create() -> std::io::Result<Self> {
        let path = std::env::temp_dir().join(format!("tjuae-agent-diagnostic-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for DiagnosticWorkspace {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.path) {
            tracing::debug!(path = %self.path.display(), %error, "清理 Agent 诊断工作区失败");
        }
    }
}
