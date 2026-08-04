//! Agent-readable capability contract for `tjuaecore diagnose`.

use serde_json::{Value, json};

const RUNTIME_ENV: [&str; 3] = ["TJUAE_BASE_URL", "TJUAE_CONVERSATION_ID", "TJUAE_USER_ID"];

pub(crate) fn data() -> Value {
    json!({
        "schema_version": 1,
        "contract": "agent-facing-diagnose-cli",
        "stability": "stable",
        "input": {
            "default_mode": "stdin_json",
            "business_flags": false,
            "selectors": {
                "conversation_id": {
                    "current": "resolve from TJUAE_CONVERSATION_ID",
                    "literal": "treat as conversation id"
                }
            }
        },
        "output": {
            "stdout": "JSON 信封",
            "stderr": "single stable DIAGNOSE_... error line",
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
            "optional_environment": ["TJUAE_LOG_DIR"]
        },
        "safety": {
            "read_only": true,
            "redacted_by_default": [
                "provider api keys",
                "Authorization 请求头",
                "MCP 请求头",
                "environment variables",
                "tokens",
                "passwords",
                "secrets"
            ],
            "http_escape_hatch": {
                "command": "diagnose http get",
                "method": "仅 GET",
                "allowed_paths": ["/health", "/api/..."],
                "prefer_named_commands": true
            }
        },
        "domains": [
            domain("core", &[
                no_input(&["capabilities"], "输出智能体可读的能力契约。"),
                no_input(&["context"], "读取当前运行时上下文。"),
                no_input(&["health"], "读取后端健康状态。"),
                no_input(&["overview"], "读取跨领域诊断快照。"),
            ]),
            domain("conversations", &[
                optional_stdin(&["conversations", "list"], "列出对话及运行时摘要。", &["limit"]),
                stdin(&["conversations", "get"], "读取一个对话及卡住或等待提示。", &["conversation_id"], &["conversation_id"]),
                stdin(&["conversations", "messages"], "读取对话消息。", &["conversation_id", "limit", "errors_only"], &["conversation_id"]),
            ]),
            domain("providers", &[
                no_input(&["providers", "summary"], "汇总模型提供商健康状态。"),
            ]),
            domain("mcp", &[
                no_input(&["mcp", "summary"], "汇总 MCP 服务及已启用但没有工具的服务。"),
            ]),
            domain("cron", &[
                no_input(&["cron", "summary"], "汇总定时任务及最近失败状态。"),
            ]),
            domain("teams", &[
                no_input(&["teams", "summary"], "汇总团队及成员对话状态。"),
            ]),
            domain("logs", &[
                command(CommandDescriptor {
                    path: &["logs", "tail"],
                    description: "从 TJUAE_LOG_DIR 或标准输入的 log_dir 读取 tjuaecore 日志尾部。",
                    input: "stdin_json",
                    stdin_fields: &["log_dir", "lines", "errors_only", "conversation_id"],
                    selectors: &["conversation_id"],
                    escape_hatch: false,
                    requires_context: &[],
                    redacted_fields: &["Authorization", "token", "secret", "password"],
                }),
            ]),
            domain("http", &[
                command(CommandDescriptor {
                    path: &["http", "get"],
                    description: "用于尚未覆盖的诊断读取的受控 GET 通道。",
                    input: "stdin_json",
                    stdin_fields: &["path", "reason"],
                    selectors: &[],
                    escape_hatch: true,
                    requires_context: &RUNTIME_ENV,
                    redacted_fields: &["api_key", "headers", "env", "token", "secret", "password"],
                }),
            ]),
        ]
    })
}

fn domain(name: &str, commands: &[Value]) -> Value {
    json!({
        "name": name,
        "commands": commands,
    })
}

fn no_input(path: &[&str], description: &str) -> Value {
    command(CommandDescriptor {
        path,
        description,
        input: "none",
        stdin_fields: &[],
        selectors: &[],
        escape_hatch: false,
        requires_context: if path == ["capabilities"] { &[] } else { &RUNTIME_ENV },
        redacted_fields: &[],
    })
}

fn optional_stdin(path: &[&str], description: &str, stdin_fields: &[&str]) -> Value {
    command(CommandDescriptor {
        path,
        description,
        input: "optional_stdin_json",
        stdin_fields,
        selectors: &[],
        escape_hatch: false,
        requires_context: &RUNTIME_ENV,
        redacted_fields: &[],
    })
}

fn stdin(path: &[&str], description: &str, stdin_fields: &[&str], selectors: &[&str]) -> Value {
    command(CommandDescriptor {
        path,
        description,
        input: "stdin_json",
        stdin_fields,
        selectors,
        escape_hatch: false,
        requires_context: &RUNTIME_ENV,
        redacted_fields: &[],
    })
}

struct CommandDescriptor<'a> {
    path: &'a [&'a str],
    description: &'a str,
    input: &'a str,
    stdin_fields: &'a [&'a str],
    selectors: &'a [&'a str],
    escape_hatch: bool,
    requires_context: &'a [&'a str],
    redacted_fields: &'a [&'a str],
}

fn command(spec: CommandDescriptor<'_>) -> Value {
    json!({
        "path": spec.path,
        "command": format!("diagnose {}", spec.path.join(" ")),
        "description": spec.description,
        "input": spec.input,
        "stdin_fields": spec.stdin_fields,
        "selectors": spec.selectors,
        "readback": false,
        "destructive": false,
        "escape_hatch": spec.escape_hatch,
        "requires_context": spec.requires_context,
        "redacted_fields": spec.redacted_fields,
    })
}
