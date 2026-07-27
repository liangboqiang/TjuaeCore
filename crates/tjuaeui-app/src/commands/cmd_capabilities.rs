//! Top-level agent-readable capability index for the `tjuaecore` binary.

use std::io::{self, Write};
use std::process::ExitCode;

use serde_json::{Value, json};

const RUNTIME_ENV: [&str; 4] = [
    "TJUAE_HELPER_BIN",
    "TJUAE_BASE_URL",
    "TJUAE_CONVERSATION_ID",
    "TJUAE_USER_ID",
];

pub(crate) fn run_capabilities() -> ExitCode {
    match print_envelope(data()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(()) => {
            eprintln!("CAPABILITIES_STDOUT_WRITE_FAILED command=\"capabilities\": 写入 JSON 输出失败");
            ExitCode::from(1)
        }
    }
}

fn data() -> Value {
    json!({
        "schema_version": 1,
        "contract": "agent-facing-tjuaecore-cli",
        "stability": "stable",
        "entrypoint": "tjuaecore capabilities",
        "purpose": "面向智能体的 TjuaeCore CLI 顶层领域索引。",
        "output": {
            "stdout": "JSON 信封",
            "stderr": "single stable ..._FAILED error line when output cannot be written",
            "success_shape": {
                "success": true,
                "data": {},
                "meta": {
                    "schema_version": 1
                }
            }
        },
        "runtime_context": {
            "primary": "TJUAE_CONVERSATION_ID",
            "environment": RUNTIME_ENV,
            "selectors": {
                "conversation_id": {
                    "current": "resolve from TJUAE_CONVERSATION_ID"
                },
                "assistant_id": {
                    "current": "resolve via current conversation"
                },
                "user_id": {
                    "current": "resolve from TJUAE_USER_ID"
                }
            }
        },
        "input": {
            "default_mode": "stdin_json",
            "business_flags": false,
            "domain_contracts": "使用各领域的 capabilities 命令查看准确的标准输入字段与安全元数据。"
        },
        "domains": [
            {
                "name": "config",
                "mode": "read-write",
                "description": "管理 TjuaeUI 配置：助手、助手规则、技能、MCP 服务、模型提供商、设置、智能体和定时任务。",
                "contract": "agent-facing-config-cli",
                "contract_command": "config capabilities",
                "invocation": "tjuaecore config capabilities",
                "runtime_required": ["TJUAE_BASE_URL", "TJUAE_CONVERSATION_ID", "TJUAE_USER_ID"],
                "safety": {
                    "can_write": true,
                    "read_before_write": true,
                    "redacted_by_default": true
                }
            },
            {
                "name": "diagnose",
                "mode": "read-only",
                "description": "诊断正在运行的 TjuaeUI：后端健康、对话、模型提供商健康、MCP、定时任务、团队、日志和受控 GET 读取。",
                "contract": "agent-facing-diagnose-cli",
                "contract_command": "diagnose capabilities",
                "invocation": "tjuaecore diagnose capabilities",
                "runtime_required": ["TJUAE_BASE_URL", "TJUAE_CONVERSATION_ID", "TJUAE_USER_ID"],
                "optional_runtime": ["TJUAE_LOG_DIR"],
                "safety": {
                    "can_write": false,
                    "read_only": true,
                    "redacted_by_default": true,
                    "escape_hatch": "diagnose http get"
                }
            },
            {
                "name": "team",
                "mode": "team-collaboration",
                "description": "面向智能体的团队协作 CLI 备用通道，供未注入 MCP 的智能体使用。",
                "contract": "agent-facing-team-cli",
                "contract_command": "team capabilities",
                "invocation": "tjuaecore team capabilities",
                "runtime_required": ["TJUAE_BASE_URL", "TJUAE_CONVERSATION_ID", "TJUAE_USER_ID", "TJUAE_RUNTIME_TOKEN"],
                "runtime_free_commands": ["team capabilities", "team help"],
                "safety": {
                    "can_write": true,
                    "runtime_token_required_for_context_and_call": true,
                    "does_not_accept_identity_authority_from_stdin": true
                }
            }
        ],
        "non_agent_subcommands": [
            {
                "name": "doctor",
                "description": "面向用户和开发者的智能体后端可用性自检。"
            },
            {
                "name": "mcp-bridge",
                "description": "团队 MCP 的内部标准输入输出到 TCP 桥接器。"
            },
            {
                "name": "mcp-team-stdio",
                "description": "内部团队 MCP 标准输入输出服务。"
            },
            {
                "name": "prepare-managed-resources",
                "description": "托管运行时资源的打包辅助工具。"
            }
        ]
    })
}

fn print_envelope(data: Value) -> Result<(), ()> {
    let rendered = serde_json::to_string_pretty(&json!({
        "success": true,
        "data": data,
        "meta": {
            "schema_version": 1
        }
    }))
    .map_err(|_| ())?;
    let mut stdout = io::stdout().lock();
    stdout
        .write_all(rendered.as_bytes())
        .and_then(|_| stdout.write_all(b"\n"))
        .map_err(|_| ())
}
