use std::sync::Arc;

use crate::agent_task::AgentInstance;
use crate::error::AgentError;
use crate::factory::AgentFactoryDeps;
use crate::factory::acp_assembler::{WorkspaceInfo, assemble_acp_params};
use crate::factory::acp_launch_policy::{AcpLaunchPolicyInput, apply_acp_launch_policy};
use crate::factory::context::FactoryContext;
use crate::manager::acp::{AcpAgentManager, CatalogForwarder};
use crate::session_context::AcpSessionBuildContext;
use agent_client_protocol::schema::{EnvVariable, HttpHeader, McpServer, McpServerHttp, McpServerSse, McpServerStdio};
use tjuaeui_api_types::{SessionMcpServer, SessionMcpTransport};
use tjuaeui_common::{CommandSpec, now_ms};
use tjuaeui_db::IMcpServerRepository;
use tjuaeui_db::models::McpServerRow;
use tjuaeui_mcp::{AcpMcpCapabilities, parse_acp_mcp_capabilities};
use tjuaeui_runtime::{ensure_runtime_command, ensure_runtime_command_with_reporter};
use tracing::{info, warn};

use crate::runtime_assets::{
    RuntimeAssetLoadRequest, RuntimeBoundaryPhase, RuntimeBoundaryReporter, handshake_runtime_asset_receipt,
};
use crate::runtime_status::conversation_runtime_reporter;

pub(super) async fn build(
    deps: Arc<AgentFactoryDeps>,
    build_context: AcpSessionBuildContext,
    ctx: FactoryContext,
    runtime_asset_request: Option<RuntimeAssetLoadRequest>,
    runtime_boundary_reporter: Option<RuntimeBoundaryReporter>,
) -> Result<AgentInstance, AgentError> {
    let mut config = build_context.config;

    // Resolve the catalog row — prefer explicit agent_id, fall
    // back to a vendor-label match for legacy payloads.
    let meta = if let Some(ref agent_id) = config.agent_id {
        deps.agent_registry.get(agent_id).await
    } else if let Some(ref vendor) = config.backend {
        deps.agent_registry.find_builtin_by_backend(vendor).await
    } else {
        None
    }
    .ok_or_else(|| AgentError::bad_request("ACP Agent 的 extra 必须包含 agent_id 或 backend"))?;

    // Trust the catalog row over the client-supplied `backend` when an
    // `agent_id` was provided. The frontend collapses row-scoped rows
    // (custom ACP / remote) to a shared `custom`/`remote` slot string,
    // which downstream consumers (MCP injection, preset-context
    // composition) would mis-interpret. When the caller only supplied a
    // vendor label (builtin path), we preserve it as-is.
    if config.agent_id.is_some() || config.backend.is_none() {
        config.backend.clone_from(&meta.backend);
    }

    // Session-model port: claude/codex ALWAYS run through the clean-slate direct-CLI
    // SessionBackend (SessionAgentTask), NOT the ACP manager. Every other ACP vendor
    // keeps the AcpAgentManager path below. There is no fallback: a claude/codex
    // build that yields no instance is a hard error, not a silent drop to ACP. The
    // build inputs mirror clean-slate `build_runtime` 1:1 (resume anchor, mode/model
    // precedence, MCP + preset + skills init surface, cc-switch env, codex sandbox/approval).
    if let Some(backend_label) = config.backend.as_deref()
        && matches!(backend_label, "claude" | "codex")
    {
        let instance = crate::session_agent::build_session_instance(
            backend_label,
            crate::session_agent::SessionBuildInputs {
                conversation_id: ctx.conversation_id.clone(),
                user_id: ctx.user_id.clone(),
                workspace: ctx.workspace.clone(),
                is_custom_workspace: ctx.is_custom_workspace,
                skill_roots: ctx.skill_roots.clone(),
                config: &config,
                metadata: &meta,
                session_snapshot: build_context.session_snapshot.as_ref(),
                backend_session_id: build_context.session_id.clone(),
                mcp_server_repo: deps.mcp_server_repo.as_ref(),
                runtime_asset_configuration_resolver: deps.runtime_asset_configuration_resolver.as_ref(),
                // The TJUAE_* conversation runtime context the legacy path
                // injects via apply_acp_launch_policy — forwarded into
                // SessionConfig.spawn_env so direct-CLI spawns get it too.
                runtime_env: &ctx.runtime_env,
                broadcaster: deps.broadcaster.clone(),
                // G5: keyed by the resolved catalog row so the discovered
                // modes/models/commands refresh the `/api/engines` picker (the
                // AcpAgentManager path does this via CatalogForwarder; the session
                // path polls capabilities() directly since its stream carries no
                // catalog events).
                catalog_writeback: Some((meta.id.clone(), deps.agent_registry.catalog_sender())),
                // Persist the resume anchor + observed mode/model from the session
                // pump (the ACP path does this via acp_agent_service.attach, which
                // this early-return bypasses).
                acp_session_repo: Some(deps.acp_agent_service.repo()),
                // DEV (`--dump-prompts`): resolve the dump dir once (mirrors the
                // tjuae_cli factory's `prompt_dump_dir`). `None` when off.
                prompt_dump_dir: crate::dev_prompt_dump::dump_dir_for_data_dir(&deps.data_dir, deps.dump_prompts),
                runtime_asset_request,
                runtime_boundary_reporter,
            },
            deps.session_spawner.clone(),
        )
        .await?
        .ok_or_else(|| {
            AgentError::internal(format!(
                "session backend for '{backend_label}' unexpectedly returned no instance"
            ))
        })?;
        tracing::info!(
            conversation_id = %ctx.conversation_id,
            backend = %backend_label,
            "session-port: routing conversation through the direct-CLI SessionAgentTask (not AcpAgentManager)"
        );
        return Ok(instance);
    }

    let mut command_spec = resolve_agent_command_spec(
        &meta,
        &ctx.workspace,
        &ctx.user_id,
        deps.runtime_asset_configuration_resolver.as_deref(),
        &ctx.conversation_id,
        deps.broadcaster.clone(),
    )
    .await?;
    apply_acp_launch_policy(
        &mut command_spec,
        AcpLaunchPolicyInput {
            metadata: &meta,
            config: &config,
            session_snapshot: build_context.session_snapshot.as_ref(),
            runtime_env: &ctx.runtime_env,
        },
    );
    let session_snapshot = build_context.session_snapshot;

    // Load user-configured MCP servers from the DB so they reach
    // ACP `session/new` mcpServers payload. Without this the agent
    // starts with zero MCP tools even when the user configured them
    // via Settings → MCP (ELECTRON-1JG).
    let mcp_capabilities = meta
        .handshake
        .agent_capabilities
        .as_ref()
        .map(parse_acp_mcp_capabilities)
        .unwrap_or_default();

    let user_mcp_servers = match deps.mcp_server_repo.as_ref() {
        Some(repo) => {
            load_user_mcp_servers(
                repo.as_ref(),
                config.mcp_server_ids.as_deref(),
                &ctx.conversation_id,
                &mcp_capabilities,
                &ctx.user_id,
                deps.runtime_asset_configuration_resolver.as_deref(),
            )
            .await?
        }
        None => Vec::new(),
    };
    let mut session_mcp_servers = user_mcp_servers;
    for server in &config.session_mcp_servers {
        if !session_server_supported_by_capabilities(server, &mcp_capabilities) {
            warn!(
                ctx.conversation_id,
                server_id = %server.id,
                server_name = %server.name,
                "session_mcp: transport unsupported by ACP agent; skipping"
            );
            continue;
        }
        match session_server_to_sdk_mcp_server(server).await {
            Ok(server) => session_mcp_servers.push(server),
            Err(err) => {
                warn!(
                    ctx.conversation_id,
                    server_id = %server.id,
                    server_name = %server.name,
                    error = %err,
                    "session_mcp: failed to convert session snapshot; skipping"
                );
            }
        }
    }

    let inject_started_at = now_ms();
    let params = Arc::new(
        assemble_acp_params(
            ctx.conversation_id.clone(),
            WorkspaceInfo {
                path: ctx.workspace,
                is_custom: ctx.is_custom_workspace,
            },
            meta,
            command_spec,
            config,
            session_mcp_servers,
            session_snapshot,
            deps.data_dir.clone(),
            deps.dump_prompts,
        )
        .await,
    );
    if let Some(reporter) = runtime_boundary_reporter.as_ref()
        && let Some(request) = runtime_asset_request.as_ref()
    {
        let ended_at = now_ms();
        for asset in &request.core_assets {
            reporter.succeeded(RuntimeBoundaryPhase::Inject, inject_started_at, ended_at, Some(asset));
        }
    }

    let skill_mgr = deps.skill_manager.clone();
    let catalog_tx = deps.agent_registry.catalog_sender();

    let spawn_started_at = now_ms();
    let (agent, domain_rx, notification_rx) = match AcpAgentManager::build(params, skill_mgr, &catalog_tx).await {
        Ok(value) => {
            if let Some(reporter) = runtime_boundary_reporter.as_ref() {
                reporter.succeeded(RuntimeBoundaryPhase::Spawn, spawn_started_at, now_ms(), None);
            }
            value
        }
        Err(error) => {
            if let Some(reporter) = runtime_boundary_reporter.as_ref() {
                reporter.failed(
                    RuntimeBoundaryPhase::Spawn,
                    spawn_started_at,
                    now_ms(),
                    None,
                    "TJUAE_RUNTIME_SPAWN_FAILED",
                );
            }
            return Err(error);
        }
    };

    let arc = Arc::new(agent);
    arc.start_permission_handler();
    arc.start_session_event_tracker(notification_rx);
    CatalogForwarder::spawn(
        arc.agent_id().to_owned(),
        crate::IAgentTask::subscribe(arc.as_ref()),
        catalog_tx,
    );

    // Desired (mode/model/config) are seeded from `params.session_snapshot`
    // inside `AcpAgentManager::new`. The CLI-assigned session id is still
    // loaded here so the first turn after a task rebuild takes the resume
    // path.
    if let Some(sid) = build_context.session_id {
        arc.set_session_id(sid).await;
    }

    // Open the ACP session eagerly so runtime preparation returns only after
    // session/new (or claude-meta-resume / session/load) and the first
    // reconcile pass have completed. Matches tjuae_cli factory behaviour:
    // the caller sees "warmed up" == "ready for PUT /mode | /model".
    let handshake_started_at = now_ms();
    if let Err(error) = arc.warmup_session().await {
        if let Some(reporter) = runtime_boundary_reporter.as_ref() {
            let ended_at = now_ms();
            let runtime_assets = runtime_asset_request
                .as_ref()
                .map(|request| request.runtime_assets.as_slice())
                .unwrap_or_default();
            if runtime_assets.is_empty() {
                reporter.failed(
                    RuntimeBoundaryPhase::Handshake,
                    handshake_started_at,
                    ended_at,
                    None,
                    "TJUAE_RUNTIME_HANDSHAKE_FAILED",
                );
            } else {
                for asset in runtime_assets {
                    reporter.failed(
                        RuntimeBoundaryPhase::Handshake,
                        handshake_started_at,
                        ended_at,
                        Some(asset),
                        "TJUAE_RUNTIME_HANDSHAKE_FAILED",
                    );
                }
            }
        }
        return Err(error);
    }
    if let Some(reporter) = runtime_boundary_reporter.as_ref() {
        let ended_at = now_ms();
        let runtime_assets = runtime_asset_request
            .as_ref()
            .map(|request| request.runtime_assets.as_slice())
            .unwrap_or_default();
        if runtime_assets.is_empty() {
            reporter.succeeded(RuntimeBoundaryPhase::Handshake, handshake_started_at, ended_at, None);
        } else {
            for asset in runtime_assets {
                reporter.succeeded(
                    RuntimeBoundaryPhase::Handshake,
                    handshake_started_at,
                    ended_at,
                    Some(asset),
                );
            }
        }
    }
    if let Some(request) = runtime_asset_request.as_ref() {
        let receipt = handshake_runtime_asset_receipt(request)
            .map_err(|error| AgentError::conflict(format!("ACP 运行资产回执无法确认：{error}")))?;
        arc.set_runtime_asset_receipt(receipt)?;
    }

    let instance = AgentInstance::Acp(Arc::clone(&arc));

    // Hand the service the domain event receiver so it can
    // persist user intent changes without reverse-engineering
    // them from CLI observations.
    deps.acp_agent_service.attach(ctx.conversation_id, domain_rx).await;

    Ok(instance)
}

async fn resolve_agent_command_spec(
    meta: &tjuaeui_api_types::AgentMetadata,
    workspace: &str,
    user_id: &str,
    runtime_configuration_resolver: Option<&dyn tjuaeui_asset::RuntimeAssetConfigurationResolver>,
    conversation_id: &str,
    broadcaster: Arc<dyn tjuaeui_realtime::EventBroadcaster>,
) -> Result<CommandSpec, AgentError> {
    let command = meta
        .command
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AgentError::bad_request(format!("Agent“{}”未配置启动命令", meta.name)))?;
    let reporter = conversation_runtime_reporter(broadcaster, conversation_id.to_owned());
    let resolved = ensure_runtime_command_with_reporter(command, Some(reporter.as_ref()))
        .await
        .map_err(|error| AgentError::bad_request(format!("Agent“{}”的 CLI 不可用：{error}", meta.name)))?;

    let mut args: Vec<String> = resolved
        .args_prefix
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let launch_args = if meta.agent_source == tjuaeui_api_types::AgentSource::Builtin
        && meta.agent_source_info.bridge_binary.as_deref() == Some("npx")
        && let Some(backend) = meta.backend.as_deref()
    {
        tjuaeui_runtime::pin_registry_npx_args(backend, &meta.args)
            .map_err(|error| AgentError::bad_request(format!("Agent“{}”的软件包锁无效：{error}", meta.name)))?
    } else {
        meta.args.clone()
    };
    args.extend(launch_args);

    let managed_local_asset_id = meta.agent_source_info.tjuae_local_asset_id.as_deref();
    if managed_local_asset_id.is_some() && !meta.env.is_empty() {
        return Err(AgentError::bad_request(
            "RUNTIME_ASSET_JIT_ENGINE_PERSISTED_ENVIRONMENT_REJECTED：托管引擎不能从旧表读取环境变量",
        ));
    }
    let mut env: Vec<tjuaeui_common::EnvVar> = meta
        .env
        .iter()
        .map(|entry| tjuaeui_common::EnvVar {
            name: entry.name.clone(),
            value: entry.value.clone(),
        })
        .collect();
    env.extend(resolved.env.iter().map(|(name, value)| tjuaeui_common::EnvVar {
        name: name.to_string_lossy().into_owned(),
        value: value.to_string_lossy().into_owned(),
    }));

    let mut cwd = Some(workspace.to_owned());
    if let Some(local_asset_id) = managed_local_asset_id {
        let jit = crate::runtime_asset_jit::resolve_engine_configuration(
            runtime_configuration_resolver,
            user_id,
            local_asset_id,
        )
        .await?;
        for entry in jit.environment {
            if env
                .iter()
                .any(|existing| existing.name.eq_ignore_ascii_case(&entry.name))
            {
                return Err(AgentError::bad_request(
                    "RUNTIME_ASSET_JIT_ENGINE_ENVIRONMENT_COLLISION：托管引擎环境变量与运行命令环境冲突",
                ));
            }
            env.push(entry);
        }
        if let Some(working_directory) = jit.working_directory {
            cwd = Some(working_directory);
        }
    }

    Ok(CommandSpec {
        command: resolved.program,
        args,
        env,
        cwd,
    })
}

/// Load the operator's enabled MCP servers from the DB, log+skip any rows
/// whose `transport_config` JSON fails to parse (better to start without one
/// MCP tool than fail the whole session), and return them in SDK shape ready
/// for `NewSessionRequest::mcp_servers`.
///
/// When `selected_ids` is present, those rows define the session snapshot and
/// are injected regardless of the current global `enabled` flag. Legacy
/// conversations without a snapshot still fall back to "all enabled rows".
/// Builtins are wired through other paths (e.g. team/guide MCP).
async fn load_user_mcp_servers(
    repo: &dyn IMcpServerRepository,
    selected_ids: Option<&[String]>,
    conversation_id: &str,
    capabilities: &AcpMcpCapabilities,
    user_id: &str,
    runtime_configuration_resolver: Option<&dyn tjuaeui_asset::RuntimeAssetConfigurationResolver>,
) -> Result<Vec<McpServer>, AgentError> {
    let rows_result = match selected_ids {
        Some(ids) => repo.list_by_ids_any(ids).await,
        None => repo.list().await,
    };
    let rows = match rows_result {
        Ok(r) => r,
        Err(err) => {
            warn!(
                conversation_id,
                error = %err,
                "user_mcp: list() failed; skipping injection"
            );
            return Ok(Vec::new());
        }
    };

    let mut servers = Vec::with_capacity(rows.len());
    for row in rows {
        let selected = selected_ids
            .map(|ids| ids.iter().any(|id| id == &row.id))
            .unwrap_or(row.enabled);
        if !selected || row.builtin {
            continue;
        }
        if !row_supported_by_capabilities(&row, capabilities) {
            warn!(
                conversation_id,
                server_id = %row.id,
                server_name = %row.name,
                transport_type = %row.transport_type,
                "user_mcp: transport unsupported by ACP agent; skipping"
            );
            continue;
        }
        let managed_local_asset_id = crate::runtime_asset_jit::managed_mcp_local_asset_id(&row)?;
        let jit = match managed_local_asset_id.as_deref() {
            Some(local_asset_id) => {
                let configuration = crate::runtime_asset_jit::resolve_mcp_configuration(
                    runtime_configuration_resolver,
                    user_id,
                    local_asset_id,
                )
                .await?;
                crate::runtime_asset_jit::verify_managed_mcp_projection(&row, &configuration)?;
                Some(configuration)
            }
            None => None,
        };
        match row_to_sdk_mcp_server(&row, jit.as_ref()).await {
            Ok(server) => servers.push(server),
            Err(_) if managed_local_asset_id.is_some() => {
                return Err(AgentError::bad_request(
                    "RUNTIME_ASSET_JIT_MCP_PROJECTION_INVALID：托管 MCP 的公开运行投影无效",
                ));
            }
            Err(err) => {
                warn!(
                    conversation_id,
                    server_id = %row.id,
                    server_name = %row.name,
                    error = %err,
                    "user_mcp: failed to convert row; skipping"
                );
            }
        }
    }

    if !servers.is_empty() {
        info!(
            conversation_id,
            count = servers.len(),
            "user_mcp: injected into session/new"
        );
    }
    Ok(servers)
}

/// Convert an `McpServerRow` into the SDK `McpServer` shape used by
/// `NewSessionRequest::mcp_servers`. Returns an error string when
/// `transport_config` is malformed or required fields are missing.
async fn row_to_sdk_mcp_server(
    row: &McpServerRow,
    jit: Option<&crate::runtime_asset_jit::McpJitConfiguration>,
) -> Result<McpServer, String> {
    let value: serde_json::Value =
        serde_json::from_str(&row.transport_config).map_err(|e| format!("invalid transport_config JSON: {e}"))?;

    match row.transport_type.as_str() {
        "stdio" => {
            let command = value
                .get("command")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "stdio: missing command".to_owned())?;
            let args: Vec<String> = value
                .get("args")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
            let mut env_entries: Vec<(String, String)> = value
                .get("env")
                .and_then(|v| v.as_object())
                .map(|obj| {
                    obj.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
                        .collect()
                })
                .unwrap_or_default();
            if let Some(jit) = jit {
                env_entries.extend(
                    jit.environment
                        .iter()
                        .map(|(name, value)| (name.clone(), value.clone())),
                );
            }
            env_entries.sort_by(|a, b| a.0.cmp(&b.0));
            let (resolved_command, args, env) = ensure_stdio_launch(command, &args, &env_entries).await?;

            let stdio = McpServerStdio::new(row.name.clone(), resolved_command)
                .args(args)
                .env(env);
            Ok(McpServer::Stdio(stdio))
        }
        "http" | "streamable_http" => {
            let url = value
                .get("url")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "http: missing url".to_owned())?;
            let mut headers = parse_headers(value.get("headers"));
            if let Some(jit) = jit {
                headers.extend(
                    jit.headers
                        .iter()
                        .map(|(name, value)| HttpHeader::new(name.clone(), value.clone())),
                );
            }
            Ok(McpServer::Http(
                McpServerHttp::new(row.name.clone(), url).headers(headers),
            ))
        }
        "sse" => {
            let url = value
                .get("url")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "sse: missing url".to_owned())?;
            let mut headers = parse_headers(value.get("headers"));
            if let Some(jit) = jit {
                headers.extend(
                    jit.headers
                        .iter()
                        .map(|(name, value)| HttpHeader::new(name.clone(), value.clone())),
                );
            }
            Ok(McpServer::Sse(
                McpServerSse::new(row.name.clone(), url).headers(headers),
            ))
        }
        other => Err(format!("unknown transport type: {other}")),
    }
}

fn parse_headers(value: Option<&serde_json::Value>) -> Vec<HttpHeader> {
    let Some(obj) = value.and_then(|v| v.as_object()) else {
        return Vec::new();
    };
    let mut entries: Vec<(String, String)> = obj
        .iter()
        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries.into_iter().map(|(k, v)| HttpHeader::new(k, v)).collect()
}

async fn session_server_to_sdk_mcp_server(server: &SessionMcpServer) -> Result<McpServer, String> {
    match &server.transport {
        SessionMcpTransport::Stdio { command, args, env } => {
            if command.is_empty() {
                return Err("stdio: missing command".to_owned());
            }
            let mut entries: Vec<(String, String)> = env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            let (command, args, env) = ensure_stdio_launch(command, args, &entries).await?;
            Ok(McpServer::Stdio(
                McpServerStdio::new(server.name.clone(), command).args(args).env(env),
            ))
        }
        SessionMcpTransport::Http { url, headers } | SessionMcpTransport::StreamableHttp { url, headers } => {
            if url.is_empty() {
                return Err("http: missing url".to_owned());
            }
            let mut entries: Vec<(String, String)> = headers.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            let headers = entries.into_iter().map(|(k, v)| HttpHeader::new(k, v)).collect();
            Ok(McpServer::Http(
                McpServerHttp::new(server.name.clone(), url).headers(headers),
            ))
        }
        SessionMcpTransport::Sse { url, headers } => {
            if url.is_empty() {
                return Err("sse: missing url".to_owned());
            }
            let mut entries: Vec<(String, String)> = headers.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            let headers = entries.into_iter().map(|(k, v)| HttpHeader::new(k, v)).collect();
            Ok(McpServer::Sse(
                McpServerSse::new(server.name.clone(), url).headers(headers),
            ))
        }
    }
}

async fn ensure_stdio_launch(
    command: &str,
    args: &[String],
    env: &[(String, String)],
) -> Result<(std::path::PathBuf, Vec<String>, Vec<EnvVariable>), String> {
    let resolved = ensure_runtime_command(command)
        .await
        .map_err(|error| error.to_string())?;

    let mut final_args: Vec<String> = resolved
        .args_prefix
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    final_args.extend(args.iter().cloned());

    let mut final_env: Vec<EnvVariable> = env
        .iter()
        .map(|(name, value)| EnvVariable::new(name.clone(), value.clone()))
        .collect();
    final_env.extend(resolved.env.iter().map(|(name, value)| {
        EnvVariable::new(
            name.to_string_lossy().into_owned(),
            value.to_string_lossy().into_owned(),
        )
    }));

    Ok((resolved.program, final_args, final_env))
}

fn row_supported_by_capabilities(row: &McpServerRow, capabilities: &AcpMcpCapabilities) -> bool {
    match row.transport_type.as_str() {
        "stdio" => capabilities.stdio,
        "http" | "streamable_http" => capabilities.http,
        "sse" => capabilities.sse,
        _ => false,
    }
}

fn session_server_supported_by_capabilities(server: &SessionMcpServer, capabilities: &AcpMcpCapabilities) -> bool {
    match server.transport {
        SessionMcpTransport::Stdio { .. } => capabilities.stdio,
        SessionMcpTransport::Http { .. } | SessionMcpTransport::StreamableHttp { .. } => capabilities.http,
        SessionMcpTransport::Sse { .. } => capabilities.sse,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::sync::OnceLock;
    #[cfg(unix)]
    use std::{
        mem,
        path::{Path, PathBuf},
    };
    #[cfg(unix)]
    use tjuaeui_realtime::BroadcastEventBus;
    #[cfg(unix)]
    use tjuaeui_runtime::{ManagedResourcesMode, init as init_runtime, set_managed_resources_mode};

    fn make_row(
        name: &str,
        transport_type: &str,
        transport_config: &str,
        enabled: bool,
        builtin: bool,
    ) -> McpServerRow {
        McpServerRow {
            id: format!("mcp_{name}"),
            name: name.to_owned(),
            description: None,
            enabled,
            transport_type: transport_type.into(),
            transport_config: transport_config.into(),
            tools: None,
            last_test_status: "disconnected".into(),
            last_connected: None,
            original_json: None,
            builtin,
            deleted_at: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn stdio_config_for_existing_command() -> String {
        let command = std::env::current_exe()
            .expect("current test executable")
            .to_string_lossy()
            .into_owned();
        serde_json::json!({
            "command": command,
            "args": [],
            "env": {},
        })
        .to_string()
    }

    #[cfg(unix)]
    fn path_test_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    #[cfg(unix)]
    fn is_npx_command_path(command: &str) -> bool {
        command == "npx" || command.ends_with("/npx") || command.ends_with("\\npx.cmd")
    }

    #[cfg(unix)]
    fn test_runtime_data_dir() -> &'static PathBuf {
        static DIR: OnceLock<PathBuf> = OnceLock::new();
        DIR.get_or_init(|| {
            let temp = tempfile::tempdir().expect("tempdir");
            let path = temp.path().to_path_buf();
            mem::forget(temp);
            init_runtime(&path);
            path
        })
    }

    #[cfg(unix)]
    fn install_fake_bundled_runtime() -> tempfile::TempDir {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("tempdir");
        let runtime_root = tmp.path().join("node").join(current_node_runtime_directory_name());
        let bin = runtime_root.join("bin");
        std::fs::create_dir_all(&bin).expect("create bin");

        for tool in ["node", "npm", "npx"] {
            let path = bin.join(tool);
            std::fs::write(&path, "#!/bin/sh\necho v24.11.0\n").expect("write tool");
            let mut perms = std::fs::metadata(&path).expect("metadata").permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).expect("chmod");
        }

        tmp
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    fn current_node_runtime_directory_name() -> &'static str {
        "node-v24.11.0-darwin-arm64"
    }

    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    fn current_node_runtime_directory_name() -> &'static str {
        "node-v24.11.0-darwin-x64"
    }

    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    fn current_node_runtime_directory_name() -> &'static str {
        "node-v24.11.0-linux-arm64"
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn current_node_runtime_directory_name() -> &'static str {
        "node-v24.11.0-linux-x64"
    }

    #[cfg(all(
        unix,
        not(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "macos", target_arch = "x86_64"),
            all(target_os = "linux", target_arch = "aarch64"),
            all(target_os = "linux", target_arch = "x86_64")
        ))
    ))]
    fn current_node_runtime_directory_name() -> &'static str {
        panic!("unsupported managed Node runtime test platform")
    }

    #[cfg(unix)]
    struct BundledRuntimeModeGuard;

    #[cfg(unix)]
    impl BundledRuntimeModeGuard {
        fn install(root: &Path) -> Self {
            unsafe { std::env::set_var("TJUAE_BUNDLED_MANAGED_RESOURCES", root) };
            set_managed_resources_mode(ManagedResourcesMode::Bundled);
            Self
        }
    }

    #[cfg(unix)]
    impl Drop for BundledRuntimeModeGuard {
        fn drop(&mut self) {
            unsafe { std::env::remove_var("TJUAE_BUNDLED_MANAGED_RESOURCES") };
            set_managed_resources_mode(ManagedResourcesMode::Download);
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn row_to_sdk_stdio_flattens_resolved_npx_command() {
        let _lock = path_test_lock().lock().await;
        let runtime = install_fake_bundled_runtime();
        let _runtime_data_dir = test_runtime_data_dir();
        let _runtime_mode = BundledRuntimeModeGuard::install(runtime.path());

        let row = make_row(
            "ctx7",
            "stdio",
            r#"{"command":"npx","args":["-y","@upstash/context7-mcp"],"env":{"K":"V"}}"#,
            true,
            false,
        );

        let server = row_to_sdk_mcp_server(&row, None).await.expect("convert");
        match server {
            McpServer::Stdio(s) => {
                let command = s.command.to_string_lossy();
                assert_ne!(command, "npx");
                assert!(command.ends_with("/npx"), "unexpected stdio command path: {command}");
                assert_eq!(s.args, vec!["-y".to_owned(), "@upstash/context7-mcp".to_owned()]);
            }
            _ => panic!("expected Stdio"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn resolve_agent_command_spec_flattens_bare_npx_command() {
        let _lock = path_test_lock().lock().await;
        let runtime = install_fake_bundled_runtime();
        let _runtime_data_dir = test_runtime_data_dir();
        let _runtime_mode = BundledRuntimeModeGuard::install(runtime.path());

        let mut meta = tjuaeui_api_types::AgentMetadata {
            id: "agent-1".into(),
            icon: None,
            name: "Test ACP".into(),
            name_i18n: None,
            description: None,
            description_i18n: None,
            backend: Some("custom".into()),
            agent_type: tjuaeui_common::AgentType::Acp,
            agent_source: tjuaeui_api_types::AgentSource::Custom,
            agent_source_info: tjuaeui_api_types::AgentSourceInfo::default(),
            enabled: true,
            available: true,
            command: Some("npx".into()),
            resolved_command: None,
            args: vec!["-y".into(), "@scope/test-agent".into()],
            env: vec![tjuaeui_api_types::AgentEnvEntry {
                name: "K".into(),
                value: "V".into(),
                description: None,
            }],
            native_skills_dirs: None,
            behavior_policy: tjuaeui_api_types::BehaviorPolicy::default(),
            yolo_id: None,
            sort_order: 0,
            team_capable: false,
            last_check_status: None,
            last_check_kind: None,
            last_check_error_code: None,
            last_check_error_message: None,
            last_check_error_details: None,
            last_check_guidance: None,
            last_check_latency_ms: None,
            last_check_at: None,
            last_success_at: None,
            last_failure_at: None,
            handshake: tjuaeui_api_types::AgentHandshake::default(),
            has_command_override: false,
            env_override_key_count: 0,
        };

        let spec = resolve_agent_command_spec(
            &meta,
            "/tmp/workspace",
            "user-1",
            None,
            "conv-acp",
            Arc::new(BroadcastEventBus::new(16)),
        )
        .await
        .expect("resolved command spec");

        let command = spec.command.to_string_lossy();
        assert_ne!(command, "npx");
        assert!(command.ends_with("/npx"), "unexpected stdio command path: {command}");
        assert_eq!(spec.args, vec!["-y".to_owned(), "@scope/test-agent".to_owned()]);
        assert!(spec.env.iter().any(|entry| entry.name == "K" && entry.value == "V"));
        assert_eq!(spec.cwd.as_deref(), Some("/tmp/workspace"));

        meta.name = "Pi".into();
        meta.backend = Some("pi".into());
        meta.agent_source = tjuaeui_api_types::AgentSource::Builtin;
        meta.agent_source_info.bridge_binary = Some("npx".into());
        meta.args = vec!["-y".into(), "pi-acp".into()];
        let spec = resolve_agent_command_spec(
            &meta,
            "/tmp/workspace",
            "user-1",
            None,
            "conv-acp",
            Arc::new(BroadcastEventBus::new(16)),
        )
        .await
        .expect("resolved release-pinned builtin command spec");
        assert_eq!(spec.args, vec!["-y", "pi-acp@0.0.31"]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn row_to_sdk_stdio_roundtrip() {
        let _lock = path_test_lock().lock().await;
        let runtime = install_fake_bundled_runtime();
        let _runtime_data_dir = test_runtime_data_dir();
        let _runtime_mode = BundledRuntimeModeGuard::install(runtime.path());

        let row = make_row(
            "ctx7",
            "stdio",
            r#"{"command":"npx","args":["-y","@upstash/context7-mcp"],"env":{"K":"V"}}"#,
            true,
            false,
        );
        let server = row_to_sdk_mcp_server(&row, None).await.expect("convert");
        match server {
            McpServer::Stdio(s) => {
                assert_eq!(s.name, "ctx7");
                let command = s.command.to_string_lossy();
                assert!(
                    is_npx_command_path(&command),
                    "unexpected stdio command path: {command}",
                );
                assert_eq!(s.args, vec!["-y".to_owned(), "@upstash/context7-mcp".to_owned()]);
                assert!(
                    s.env.iter().any(|entry| entry.name == "K" && entry.value == "V"),
                    "missing user-provided env in stdio launch"
                );
            }
            _ => panic!("expected Stdio"),
        }
    }

    #[tokio::test]
    async fn row_to_sdk_http_with_headers() {
        let row = make_row(
            "remote",
            "http",
            r#"{"url":"https://example.com/mcp","headers":{"Authorization":"Bearer tok"}}"#,
            true,
            false,
        );
        let server = row_to_sdk_mcp_server(&row, None).await.expect("convert");
        match server {
            McpServer::Http(h) => {
                assert_eq!(h.name, "remote");
                assert_eq!(h.url, "https://example.com/mcp");
                assert_eq!(h.headers.len(), 1);
                assert_eq!(h.headers[0].name, "Authorization");
                assert_eq!(h.headers[0].value, "Bearer tok");
            }
            _ => panic!("expected Http"),
        }
    }

    #[tokio::test]
    async fn row_to_sdk_unknown_transport_type_errors() {
        let row = make_row("bad", "websocket", "{}", true, false);
        assert!(row_to_sdk_mcp_server(&row, None).await.is_err());
    }

    #[tokio::test]
    async fn row_to_sdk_invalid_json_errors() {
        let row = make_row("bad", "stdio", "not-json", true, false);
        assert!(row_to_sdk_mcp_server(&row, None).await.is_err());
    }

    #[tokio::test]
    async fn row_to_sdk_stdio_missing_command_errors() {
        let row = make_row("bad", "stdio", r#"{"args":[]}"#, true, false);
        assert!(row_to_sdk_mcp_server(&row, None).await.is_err());
    }

    // -- load_user_mcp_servers integration -----------------------------------

    use async_trait::async_trait;
    use std::sync::Arc;

    struct MockRepo {
        rows: Vec<McpServerRow>,
        fail: bool,
    }

    struct MockRuntimeConfigurationResolver {
        expected_user_id: String,
        configuration: tjuaeui_api_types::AssetPublicConfiguration,
        secrets: std::collections::BTreeMap<String, String>,
    }

    #[async_trait]
    impl tjuaeui_asset::RuntimeAssetConfigurationResolver for MockRuntimeConfigurationResolver {
        async fn resolve(
            &self,
            user_id: &str,
            _local_asset_id: &str,
        ) -> Result<Option<tjuaeui_asset::RuntimeResolvedConfiguration>, tjuaeui_asset::AssetError> {
            if user_id != self.expected_user_id {
                return Ok(None);
            }
            Ok(Some(tjuaeui_asset::RuntimeResolvedConfiguration {
                configuration: self.configuration.clone(),
                configuration_schema: Default::default(),
                secrets: self.secrets.clone(),
            }))
        }
    }

    #[async_trait]
    impl IMcpServerRepository for MockRepo {
        async fn list(&self) -> Result<Vec<McpServerRow>, tjuaeui_db::DbError> {
            if self.fail {
                Err(tjuaeui_db::DbError::Init("simulated".into()))
            } else {
                Ok(self.rows.clone())
            }
        }
        async fn find_by_id(&self, _id: &str) -> Result<Option<McpServerRow>, tjuaeui_db::DbError> {
            unimplemented!()
        }
        async fn find_by_name(&self, _name: &str) -> Result<Option<McpServerRow>, tjuaeui_db::DbError> {
            unimplemented!()
        }
        async fn list_by_ids_any(&self, ids: &[String]) -> Result<Vec<McpServerRow>, tjuaeui_db::DbError> {
            if self.fail {
                return Err(tjuaeui_db::DbError::Init("simulated".into()));
            }
            Ok(ids
                .iter()
                .filter_map(|id| self.rows.iter().find(|row| row.id == *id).cloned())
                .collect())
        }
        async fn create(
            &self,
            _params: tjuaeui_db::CreateMcpServerParams<'_>,
        ) -> Result<McpServerRow, tjuaeui_db::DbError> {
            unimplemented!()
        }
        async fn update(
            &self,
            _id: &str,
            _params: tjuaeui_db::UpdateMcpServerParams<'_>,
        ) -> Result<McpServerRow, tjuaeui_db::DbError> {
            unimplemented!()
        }
        async fn delete(&self, _id: &str) -> Result<(), tjuaeui_db::DbError> {
            unimplemented!()
        }
        async fn batch_upsert(
            &self,
            _servers: &[tjuaeui_db::CreateMcpServerParams<'_>],
        ) -> Result<Vec<McpServerRow>, tjuaeui_db::DbError> {
            unimplemented!()
        }
        async fn update_status(
            &self,
            _id: &str,
            _status: &str,
            _last_connected: Option<tjuaeui_common::TimestampMs>,
        ) -> Result<(), tjuaeui_db::DbError> {
            unimplemented!()
        }
        async fn update_tools(&self, _id: &str, _tools: Option<&str>) -> Result<(), tjuaeui_db::DbError> {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn load_user_mcp_servers_skips_disabled_and_builtin() {
        let stdio_config = stdio_config_for_existing_command();
        let caps = AcpMcpCapabilities {
            stdio: true,
            http: true,
            sse: true,
        };
        let repo: Arc<dyn IMcpServerRepository> = Arc::new(MockRepo {
            rows: vec![
                make_row("user-enabled", "stdio", &stdio_config, true, false),
                make_row("user-disabled", "stdio", &stdio_config, false, false),
                make_row(
                    "builtin",
                    "stdio",
                    r#"{"command":"img-gen","args":[],"env":{}}"#,
                    true,
                    true,
                ),
            ],
            fail: false,
        });
        let servers = load_user_mcp_servers(repo.as_ref(), None, "conv-1", &caps, "user-1", None)
            .await
            .unwrap();
        assert_eq!(servers.len(), 1);
        match &servers[0] {
            McpServer::Stdio(s) => assert_eq!(s.name, "user-enabled"),
            _ => panic!("expected stdio"),
        }
    }

    #[tokio::test]
    async fn load_user_mcp_servers_returns_empty_on_repo_failure() {
        let caps = AcpMcpCapabilities {
            stdio: true,
            http: true,
            sse: true,
        };
        let repo: Arc<dyn IMcpServerRepository> = Arc::new(MockRepo {
            rows: vec![],
            fail: true,
        });
        let servers = load_user_mcp_servers(repo.as_ref(), None, "conv-1", &caps, "user-1", None)
            .await
            .unwrap();
        assert!(servers.is_empty());
    }

    #[tokio::test]
    async fn load_user_mcp_servers_skips_malformed_rows_but_keeps_others() {
        let stdio_config = stdio_config_for_existing_command();
        let caps = AcpMcpCapabilities {
            stdio: true,
            http: true,
            sse: true,
        };
        let repo: Arc<dyn IMcpServerRepository> = Arc::new(MockRepo {
            rows: vec![
                make_row("good", "stdio", &stdio_config, true, false),
                make_row("bad", "stdio", "not-json", true, false),
            ],
            fail: false,
        });
        let servers = load_user_mcp_servers(repo.as_ref(), None, "conv-1", &caps, "user-1", None)
            .await
            .unwrap();
        assert_eq!(servers.len(), 1);
        match &servers[0] {
            McpServer::Stdio(s) => assert_eq!(s.name, "good"),
            _ => panic!("expected stdio"),
        }
    }

    #[tokio::test]
    async fn load_user_mcp_servers_uses_selected_snapshot_over_enabled_state() {
        let stdio_config = stdio_config_for_existing_command();
        let caps = AcpMcpCapabilities {
            stdio: true,
            http: true,
            sse: true,
        };
        let repo: Arc<dyn IMcpServerRepository> = Arc::new(MockRepo {
            rows: vec![
                make_row("enabled", "stdio", &stdio_config, true, false),
                make_row("disabled-picked", "stdio", &stdio_config, false, false),
            ],
            fail: false,
        });

        let selected = vec!["mcp_disabled-picked".to_owned()];
        let servers = load_user_mcp_servers(repo.as_ref(), Some(&selected), "conv-1", &caps, "user-1", None)
            .await
            .unwrap();

        assert_eq!(servers.len(), 1);
        match &servers[0] {
            McpServer::Stdio(s) => assert_eq!(s.name, "disabled-picked"),
            _ => panic!("expected stdio"),
        }
    }

    #[tokio::test]
    async fn load_user_mcp_servers_skips_rows_unsupported_by_capabilities() {
        let caps = AcpMcpCapabilities {
            stdio: false,
            http: true,
            sse: false,
        };
        let repo: Arc<dyn IMcpServerRepository> = Arc::new(MockRepo {
            rows: vec![make_row(
                "stdio-only",
                "stdio",
                r#"{"command":"npx","args":[],"env":{}}"#,
                true,
                false,
            )],
            fail: false,
        });

        let servers = load_user_mcp_servers(repo.as_ref(), None, "conv-1", &caps, "user-1", None)
            .await
            .unwrap();
        assert!(servers.is_empty());
    }

    #[tokio::test]
    async fn managed_mcp_credentials_are_jit_injected_without_mutating_projection() {
        let mut row = make_row(
            "managed-http",
            "http",
            r#"{"url":"https://example.invalid/mcp","headers":{}}"#,
            true,
            false,
        );
        row.original_json = Some(
            serde_json::json!({
                "$tjuaeAsset": {
                    "id": "stable-id",
                    "kind": "mcp",
                    "tjuaeLocalAssetId": "mcp:managed-http"
                }
            })
            .to_string(),
        );
        let persisted_projection = row.transport_config.clone();
        let repo: Arc<dyn IMcpServerRepository> = Arc::new(MockRepo {
            rows: vec![row],
            fail: false,
        });
        let resolver: Arc<dyn tjuaeui_asset::RuntimeAssetConfigurationResolver> =
            Arc::new(MockRuntimeConfigurationResolver {
                expected_user_id: "user-1".into(),
                configuration: tjuaeui_api_types::AssetPublicConfiguration::Mcp(
                    tjuaeui_api_types::McpAssetConfiguration {
                        transport: tjuaeui_api_types::McpAssetTransport::StreamableHttp,
                        executable_path: None,
                        arguments: Vec::new(),
                        instance_url: Some("https://example.invalid/mcp".into()),
                        environment: Vec::new(),
                        headers: vec![tjuaeui_api_types::AssetNamedSecretSlot {
                            name: "Authorization".into(),
                            secret_slot: "authorization".into(),
                        }],
                        values: Vec::new(),
                        secrets: Vec::new(),
                    },
                ),
                secrets: std::collections::BTreeMap::from([("authorization".into(), "Bearer session-only".into())]),
            });
        let caps = AcpMcpCapabilities {
            stdio: true,
            http: true,
            sse: true,
        };

        let servers = load_user_mcp_servers(repo.as_ref(), None, "conv-1", &caps, "user-1", Some(resolver.as_ref()))
            .await
            .expect("managed MCP resolves for the conversation owner");
        let McpServer::Http(server) = &servers[0] else {
            panic!("expected HTTP MCP");
        };
        assert!(
            server
                .headers
                .iter()
                .any(|header| header.name == "Authorization" && header.value == "Bearer session-only")
        );
        assert_eq!(
            persisted_projection,
            r#"{"url":"https://example.invalid/mcp","headers":{}}"#
        );
        assert!(!persisted_projection.contains("session-only"));
    }

    #[tokio::test]
    async fn managed_engine_credentials_are_jit_injected_into_command_spec() {
        let workspace = tempfile::tempdir().expect("workspace");
        let command = std::env::current_exe()
            .expect("current executable")
            .to_string_lossy()
            .into_owned();
        let mut metadata = tjuaeui_api_types::AgentMetadata {
            id: "managed-engine".into(),
            icon: None,
            name: "Managed Engine".into(),
            name_i18n: None,
            description: None,
            description_i18n: None,
            backend: None,
            agent_type: tjuaeui_common::AgentType::Acp,
            agent_source: tjuaeui_api_types::AgentSource::Extension,
            agent_source_info: tjuaeui_api_types::AgentSourceInfo {
                tjuae_local_asset_id: Some("engine:managed".into()),
                ..Default::default()
            },
            enabled: true,
            available: true,
            command: Some(command),
            resolved_command: None,
            args: Vec::new(),
            env: Vec::new(),
            native_skills_dirs: None,
            behavior_policy: Default::default(),
            yolo_id: None,
            sort_order: 0,
            team_capable: false,
            last_check_status: None,
            last_check_kind: None,
            last_check_error_code: None,
            last_check_error_message: None,
            last_check_error_details: None,
            last_check_guidance: None,
            last_check_latency_ms: None,
            last_check_at: None,
            last_success_at: None,
            last_failure_at: None,
            handshake: Default::default(),
            has_command_override: false,
            env_override_key_count: 0,
        };
        metadata.agent_source_info.binary_name = metadata.command.clone();
        let resolver: Arc<dyn tjuaeui_asset::RuntimeAssetConfigurationResolver> =
            Arc::new(MockRuntimeConfigurationResolver {
                expected_user_id: "user-1".into(),
                configuration: tjuaeui_api_types::AssetPublicConfiguration::EngineAdapter(
                    tjuaeui_api_types::EngineAdapterAssetConfiguration {
                        environment: vec![tjuaeui_api_types::AssetNamedSecretSlot {
                            name: "ENGINE_API_TOKEN".into(),
                            secret_slot: "token".into(),
                        }],
                        working_directory: Some(workspace.path().to_string_lossy().into_owned()),
                        ..Default::default()
                    },
                ),
                secrets: std::collections::BTreeMap::from([("token".into(), "session-only".into())]),
            });

        let spec = resolve_agent_command_spec(
            &metadata,
            "unused-conversation-workspace",
            "user-1",
            Some(resolver.as_ref()),
            "conv-1",
            Arc::new(tjuaeui_realtime::BroadcastEventBus::new(16)),
        )
        .await
        .expect("managed Engine resolves for the conversation owner");
        assert!(
            spec.env
                .iter()
                .any(|entry| entry.name == "ENGINE_API_TOKEN" && entry.value == "session-only")
        );
        assert_eq!(spec.cwd.as_deref(), Some(workspace.path().to_string_lossy().as_ref()));
        assert!(metadata.env.is_empty(), "legacy projection remains credential-free");
    }
}
