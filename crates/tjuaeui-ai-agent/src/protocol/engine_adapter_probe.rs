//! 类型化 Engine Adapter 的两阶段 ACP 试跑。
//!
//! Step 1: `which`/`where` — resolve the first token of `command` on
//!         `$PATH`. Bounded by `execFileSync`-equivalent 5 s timeout.
//! Step 2: Spawn the CLI via `CliAgentProcess::spawn_for_sdk`, connect
//!         an `AcpProtocol` (which owns the ACP `initialize` handshake
//!         with a built-in 30 s timeout), then shut down cleanly.
//!
//! 该模块只服务于 Core 资产的试跑与运行诊断，不再暴露旧自定义引擎 CRUD
//! 或独立连接测试入口。

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use tjuaeui_api_types::{AgentHandshake, EngineAdapterProbeResponse, ModelInfoEntry, ModelInfoPayload};
use tjuaeui_common::{CommandSpec, EnvVar, normalize_keys_to_snake_case};
use tjuaeui_runtime::{NodeRuntimeProgressReporter, ResolvedCommand, ensure_runtime_command_with_reporter};
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, warn};

use crate::capability::cli_process::CliAgentProcess;
use crate::protocol::acp::AcpProtocol;
use crate::protocol::error::AcpError;

use agent_client_protocol::schema::NewSessionRequest;

/// Step 2 overall timeout. Belt-and-suspenders: `AcpProtocol::connect`
/// already caps the initialize RPC at 30 s, but a CLI that hangs
/// before writing any ACP frame at all is covered by this outer cap.
const STEP2_TIMEOUT: Duration = Duration::from_secs(35);

/// Grace period for the child to exit on its own after stdin close, before
/// we fall back to SIGKILL on the whole process group. Keep this short because
/// manual connection tests should return promptly after the ACP probe finishes.
const PROBE_KILL_GRACE: Duration = Duration::from_millis(500);

/// 试跑一个 Engine Adapter。
///
/// Returns `Success` only if both `which` and the ACP `initialize`
/// handshake succeed. Any failure short-circuits into the
/// corresponding variant.
pub async fn probe_engine_adapter(
    command: &str,
    args: &[String],
    env: &HashMap<String, String>,
    reporter: Option<&dyn NodeRuntimeProgressReporter>,
) -> EngineAdapterProbeResponse {
    probe_engine_adapter_in_directory(command, args, env, None, reporter).await
}

/// 使用显式工作目录执行同一套 ACP initialize/session-new 探测。
pub async fn probe_engine_adapter_in_directory(
    command: &str,
    args: &[String],
    env: &HashMap<String, String>,
    working_directory: Option<&Path>,
    reporter: Option<&dyn NodeRuntimeProgressReporter>,
) -> EngineAdapterProbeResponse {
    match probe_engine_adapter_detailed_in_directory(command, args, env, working_directory, reporter).await {
        EngineAdapterProbeResult::Success { .. } => EngineAdapterProbeResponse::Success,
        EngineAdapterProbeResult::FailCli { error } => EngineAdapterProbeResponse::FailCli { error },
        EngineAdapterProbeResult::FailAcp { error, .. } => EngineAdapterProbeResponse::FailAcp { error },
        EngineAdapterProbeResult::FailAuth { error } => EngineAdapterProbeResponse::FailAuth { error },
    }
}

/// 主动诊断使用的完整 ACP 探测结果。
///
/// 与表单中的连接测试不同，成功结果保留 initialize/session-new 暴露的
/// 能力目录，供 Agent 目录缓存直接持久化。
#[derive(Debug)]
pub(crate) enum EngineAdapterProbeResult {
    Success { handshake: AgentHandshake },
    FailCli { error: String },
    FailAcp { code: &'static str, error: String },
    FailAuth { error: String },
}

pub(crate) async fn probe_engine_adapter_detailed(
    command: &str,
    args: &[String],
    env: &HashMap<String, String>,
    reporter: Option<&dyn NodeRuntimeProgressReporter>,
) -> EngineAdapterProbeResult {
    probe_engine_adapter_detailed_in_directory(command, args, env, None, reporter).await
}

async fn probe_engine_adapter_detailed_in_directory(
    command: &str,
    args: &[String],
    env: &HashMap<String, String>,
    working_directory: Option<&Path>,
    reporter: Option<&dyn NodeRuntimeProgressReporter>,
) -> EngineAdapterProbeResult {
    // ── Step 1 — which check ────────────────────────────────────────
    let head = first_token(command);
    let resolved = match ensure_runtime_command_with_reporter(head, reporter).await {
        Ok(resolved) => resolved,
        Err(error) => {
            return EngineAdapterProbeResult::FailCli {
                error: error.to_string(),
            };
        }
    };
    debug!(program = %resolved.program.display(), "自定义 Agent 探测第 1 步通过");

    // ── Step 2 — spawn + ACP initialize ─────────────────────────────
    let proc = match spawn_probe_process(resolved, args, env, working_directory).await {
        Ok(proc) => proc,
        Err(error) => {
            return EngineAdapterProbeResult::FailAcp {
                code: "process_start_failed",
                error,
            };
        }
    };

    let outcome = match tokio::time::timeout(STEP2_TIMEOUT, run_handshake(&proc, working_directory)).await {
        Ok(outcome) => outcome.into_result(),
        Err(_) => EngineAdapterProbeResult::FailAcp {
            code: "connection_timeout",
            error: format!("ACP 握手未在 {} 秒内完成", STEP2_TIMEOUT.as_secs()),
        },
    };

    // Always tear down the whole process group. `kill_on_drop(true)` only
    // signals the direct child (e.g. `npm exec ...`) — wrapper CLIs spawn
    // grandchildren (`openclaw-acp`) that survive unless we SIGKILL the
    // group explicitly via `proc.kill()`.
    if let Err(error) = proc.kill(PROBE_KILL_GRACE).await {
        warn!(pid = proc.pid(), error = %error, "Engine Adapter 试跑未能终止进程组");
    }

    outcome
}

fn first_token(command: &str) -> &str {
    command.split_whitespace().next().unwrap_or(command)
}

async fn spawn_probe_process(
    resolved: ResolvedCommand,
    args: &[String],
    env: &HashMap<String, String>,
    working_directory: Option<&Path>,
) -> Result<CliAgentProcess, String> {
    let mut final_args: Vec<String> = resolved
        .args_prefix
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    final_args.extend(args.iter().cloned());

    let mut final_env: Vec<EnvVar> = env
        .iter()
        .map(|(name, value)| EnvVar {
            name: name.clone(),
            value: value.clone(),
        })
        .collect();
    final_env.extend(resolved.env.iter().map(|(name, value)| EnvVar {
        name: name.to_string_lossy().into_owned(),
        value: value.to_string_lossy().into_owned(),
    }));
    let cwd = working_directory
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir);

    let spec = CommandSpec {
        command: resolved.program,
        args: final_args,
        env: final_env,
        cwd: Some(cwd.to_string_lossy().into_owned()),
    };

    CliAgentProcess::spawn_for_sdk(spec)
        .await
        .map_err(|e| format!("启动进程失败：{e}"))
}

/// Result of the Step 2 probe (`initialize` + `session/new`).
///
/// The probe reaches `session/new` so it can tell "reachable but not
/// authorized" (`Auth`) apart from other ACP failures (`Fail`) — `initialize`
/// alone returns `authMethods` even for already-authorized agents and cannot
/// make this distinction.
enum ProbeOutcome {
    Ok(AgentHandshake),
    Auth(String),
    Fail { code: &'static str, error: String },
}

impl ProbeOutcome {
    fn into_result(self) -> EngineAdapterProbeResult {
        match self {
            ProbeOutcome::Ok(handshake) => EngineAdapterProbeResult::Success { handshake },
            ProbeOutcome::Auth(error) => EngineAdapterProbeResult::FailAuth { error },
            ProbeOutcome::Fail { code, error } => EngineAdapterProbeResult::FailAcp { code, error },
        }
    }
}

async fn run_handshake(proc: &CliAgentProcess, working_directory: Option<&Path>) -> ProbeOutcome {
    let Some((stdin, stdout)) = proc.take_stdio().await else {
        return ProbeOutcome::Fail {
            code: "process_start_failed",
            error: "启动 SDK 进程后无法获取标准输入输出".to_string(),
        };
    };

    // Throwaway channels — a probe session never sends a prompt, so no events,
    // permission requests, or notifications are consumed.
    let (event_tx, _event_rx) = broadcast::channel(16);
    let (permission_tx, _permission_rx) = mpsc::channel(4);
    let (notification_tx, _notification_rx) = mpsc::channel(4);

    // Race the ACP initialize handshake against the child process exiting.
    // A misconfigured CLI (e.g. an invalid package launcher command) exits
    // almost immediately with a non-zero status; without this race the
    // `AcpProtocol::connect` call would block on its internal 30 s
    // timeout waiting for an `initialize` reply that will never arrive.
    let connect = AcpProtocol::connect(stdin, stdout, event_tx, permission_tx, notification_tx);
    let protocol = tokio::select! {
        biased;
        res = connect => match res {
            Ok(protocol) => protocol,
            Err(e) => {
                return ProbeOutcome::Fail {
                    code: "protocol_incompatible",
                    error: format!("ACP 初始化失败：{e}"),
                };
            }
        },
        exit = proc.wait_for_exit() => {
            let stderr = proc.take_stderr().await;
            let stderr = stderr.trim();
            let status = match exit {
                Some(s) => format!("{s}"),
                None => "unknown".to_string(),
            };
            return if stderr.is_empty() {
                ProbeOutcome::Fail {
                    code: "process_start_failed",
                    error: format!("CLI 在 ACP 初始化完成前已退出（状态={status}）"),
                }
            } else {
                ProbeOutcome::Fail {
                    code: "process_start_failed",
                    error: format!("CLI 在 ACP 初始化完成前已退出（状态={status}）：{stderr}"),
                }
            };
        }
    };

    // `initialize` only proves the agent speaks ACP, not that it is usable.
    // Open a real session (no prompt) so an auth-gated agent surfaces its
    // `auth_required` error here instead of silently appearing "online".
    let session_directory = working_directory
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir);
    let outcome = match protocol.new_session(NewSessionRequest::new(session_directory)).await {
        Ok(response) => ProbeOutcome::Ok(handshake_from_probe(&protocol, response)),
        Err(AcpError::AuthRequired) => ProbeOutcome::Auth("Agent 可连接，但需要登录或授权".to_string()),
        Err(e) => ProbeOutcome::Fail {
            code: "session_create_failed",
            error: format!("ACP 创建会话失败：{e}"),
        },
    };

    // Drop `protocol` so its shutdown oneshot fires before the outer cleanup
    // path (or the drop guard for timeout-cancelled callers) reaps the process
    // tree. The probe session is throwaway; the process-group kill in the
    // caller tears down the session along with the CLI.
    drop(protocol);
    outcome
}

fn handshake_from_probe(
    protocol: &AcpProtocol,
    response: agent_client_protocol::schema::NewSessionResponse,
) -> AgentHandshake {
    let agent_capabilities = protocol.agent_capabilities().and_then(to_snake_value);
    let auth_methods = protocol.auth_methods().and_then(to_snake_value);
    let config_options = response
        .config_options
        .as_ref()
        .and_then(to_snake_value)
        .map(|options| serde_json::json!({ "config_options": options }));
    let available_modes = response.modes.as_ref().and_then(to_snake_value);
    let available_models = response.models.as_ref().and_then(|models| {
        let current_model_id = models.current_model_id.to_string();
        let available_models = models
            .available_models
            .iter()
            .map(|model| ModelInfoEntry {
                id: model.model_id.to_string(),
                label: model.name.clone(),
            })
            .collect::<Vec<_>>();
        let current_model_label = available_models
            .iter()
            .find(|model| model.id == current_model_id)
            .map(|model| model.label.clone())
            .unwrap_or_else(|| current_model_id.clone());
        to_snake_value(ModelInfoPayload {
            current_model_id: Some(current_model_id),
            current_model_label: Some(current_model_label),
            available_models,
        })
    });

    AgentHandshake {
        agent_capabilities,
        auth_methods,
        config_options,
        available_modes,
        available_models,
        available_commands: None,
    }
}

fn to_snake_value<T: serde::Serialize>(value: T) -> Option<serde_json::Value> {
    let mut value = serde_json::to_value(value).ok()?;
    normalize_keys_to_snake_case(&mut value);
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[tokio::test]
    async fn probe_returns_fail_cli_when_command_missing() {
        let resp = probe_engine_adapter("tjuaeui-definitely-does-not-exist-xyz", &[], &HashMap::new(), None).await;
        match resp {
            EngineAdapterProbeResponse::FailCli { error } => {
                let lower = error.to_lowercase();
                assert!(
                    lower.contains("not found") || lower.contains("no such") || lower.contains("was not found"),
                    "expected 'not found' style message, got: {error}"
                );
            }
            other => panic!("expected FailCli, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn probe_returns_fail_acp_when_command_is_noop() {
        // `true` exits 0 immediately — Step 1 passes (on PATH), but the
        // process dies before ACP initialize completes, so Step 2 maps
        // to FailAcp.
        if cfg!(windows) {
            // `true` is a cmd builtin on Windows, not a standalone exe.
            return;
        }
        let resp = probe_engine_adapter("true", &[], &HashMap::new(), None).await;
        assert!(
            matches!(resp, EngineAdapterProbeResponse::FailAcp { .. }),
            "expected FailAcp, got {resp:?}"
        );
    }

    /// Regression for the production leak: a probe that talks to a wrapper
    /// CLI (`npm exec ...`, etc.) historically left the wrapper's grandchild
    /// process alive when the probe returned, because cleanup relied on
    /// `kill_on_drop(true)` which only signals the direct child. Repeated
    /// connection tests could otherwise accumulate zombie `openclaw-acp`
    /// processes.
    ///
    /// We exercise the public entry point with a CLI that exits immediately
    /// after backgrounding a long-lived grandchild — that's the production
    /// shape `npm exec openclaw --acp` collapses into when its own ACP
    /// handshake fails. The probe will see the wrapper exit (ACP fail), but
    /// by that point the grandchild has been forked. The fix must SIGKILL
    /// the whole process group before returning, so the grandchild dies too.
    #[cfg(unix)]
    #[tokio::test]
    async fn probe_kills_grandchild_left_behind_by_wrapper() {
        use std::time::Duration;
        use tokio::time::Instant;

        fn is_pid_alive(pid: i32) -> bool {
            unsafe { libc::kill(pid, 0) == 0 }
        }

        let marker = tempfile::NamedTempFile::new().unwrap();
        let marker_path = marker.path().to_owned();
        // Background a grandchild, write its pid, then exit. The probe will
        // observe `proc.wait_for_exit()` race the ACP handshake and return
        // FailAcp — the grandchild keeps running unless cleanup kills it.
        let script = format!("sleep 600 & printf '%s' \"$!\" > '{}'", marker_path.display());

        let resp = probe_engine_adapter("sh", &["-c".to_string(), script], &HashMap::new(), None).await;
        assert!(
            matches!(resp, EngineAdapterProbeResponse::FailAcp { .. }),
            "wrapper exits before ACP handshake; expected FailAcp, got {resp:?}"
        );

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut marker_contents = String::new();
        while Instant::now() < deadline {
            if let Ok(s) = std::fs::read_to_string(&marker_path)
                && !s.trim().is_empty()
            {
                marker_contents = s;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let grandchild_pid: i32 = marker_contents.trim().parse().unwrap_or_else(|_| {
            panic!("wrapper did not write the grandchild pid: {marker_contents:?}");
        });

        // Give the OS a brief moment to reap after the probe returned.
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if !is_pid_alive(grandchild_pid) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        // Cleanup so a failing test does not leave an actual leak.
        unsafe {
            libc::kill(grandchild_pid, libc::SIGKILL);
        }
        panic!("grandchild pid={grandchild_pid} survived the probe — process group cleanup is broken");
    }
}
