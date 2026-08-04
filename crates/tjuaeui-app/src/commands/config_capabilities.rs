//! Agent-readable capability contract for `tjuaecore config`.

use serde_json::{Value, json};

const RUNTIME_ENV: [&str; 3] = ["TJUAE_BASE_URL", "TJUAE_CONVERSATION_ID", "TJUAE_USER_ID"];

pub(crate) fn data() -> Value {
    json!({
        "schema_version": 1,
        "contract": "agent-facing-config-cli",
        "stability": "stable",
        "input": {
            "default_mode": "stdin_json",
            "business_flags": false,
            "selectors": {
                "assistant_id": {
                    "current": "通过 TJUAE_CONVERSATION_ID 解析",
                    "literal": "按助手 ID 处理"
                },
                "conversation_id": {
                    "current": "从 TJUAE_CONVERSATION_ID 解析",
                    "literal": "按对话 ID 处理"
                },
                "user_id": {
                    "current": "从 TJUAE_USER_ID 解析",
                    "literal": "按用户 ID 处理"
                }
            }
        },
        "output": {
            "stdout": "JSON 信封",
            "stderr": "单行稳定 CONFIG_... 错误",
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
            "environment": RUNTIME_ENV
        },
        "safety": {
            "redacted_by_default": [
                "模型提供商 API 密钥",
                "Bedrock 访问密钥和秘密",
                "MCP 请求头",
                "MCP 标准输入输出环境变量值",
                "智能体环境变量值",
                "显式读取命令之外的提示词和规则内容"
            ],
            "read_before_write": true
        },
        "domains": [
            domain("core", &[
                no_input(&["capabilities"], "输出智能体可读的能力契约。", false),
                no_input(&["context"], "读取当前运行时上下文和当前对话助手。", false),
            ]),
            domain("conversation", &[
                stdin(&["conversation", "rename"], "重命名对话。", &["conversation_id", "name"], &["conversation_id"], true, false),
            ]),
            domain("assistants", &[
                no_input(&["assistants", "list"], "列出助手。", false),
                stdin(&["assistants", "get"], "读取一个助手。", &["assistant_id", "locale"], &["assistant_id"], false, false),
                stdin_redacted(&["assistants", "rule", "read"], "读取助手规则。", &["assistant_id", "locale"], &["assistant_id"], false, false, &[]),
            ]),
            domain("skills", &[
                no_input(&["skills", "list"], "列出可用技能。", false),
            ]),
            domain("mcp", &[
                no_input_redacted(&["mcp", "servers", "list"], "列出 MCP 服务。", false, &["transport.headers", "transport.env"]),
                stdin_redacted(&["mcp", "servers", "get"], "读取一个 MCP 服务。", &["server_id"], &[], false, false, &["transport.headers", "transport.env"]),
                no_input_redacted(&["mcp", "agent-configs"], "列出智能体 MCP 配置状态。", false, &["transport.headers", "transport.env"]),
            ]),
            domain("providers", &[
                no_input_redacted(&["providers", "list"], "列出模型提供商。", false, &["api_key", "access_key", "secret_key"]),
                stdin_redacted(&["providers", "create"], "创建模型提供商。", &["name", "platform", "base_url", "api_key"], &[], true, false, &["api_key", "access_key", "secret_key"]),
                stdin_redacted(&["providers", "update"], "更新模型提供商。", &["provider_id"], &[], true, false, &["api_key", "access_key", "secret_key"]),
                stdin_redacted(&["providers", "delete"], "删除模型提供商。", &["provider_id"], &[], true, true, &["api_key", "access_key", "secret_key"]),
                stdin_redacted(&["providers", "detect-protocol"], "检测模型提供商协议。", &["base_url", "api_key"], &[], false, false, &["api_key", "access_key", "secret_key"]),
                stdin_redacted(&["providers", "fetch-models"], "从原始模型提供商配置获取模型。", &["platform", "base_url", "api_key"], &[], false, false, &["api_key", "access_key", "secret_key"]),
                stdin_redacted(&["providers", "models", "fetch"], "为已配置的模型提供商获取并保存模型。", &["provider_id"], &[], true, false, &["api_key", "access_key", "secret_key"]),
                stdin_redacted(&["providers", "health-check"], "运行模型提供商健康检查。", &["provider_id", "model"], &[], false, false, &["api_key", "access_key", "secret_key"]),
            ]),
            domain("settings", &[
                no_input(&["settings", "get"], "读取后端设置。", false),
                stdin(&["settings", "patch"], "局部更新后端设置。", &["language", "notification_enabled", "cron_notification_enabled", "command_queue_enabled", "save_upload_to_workspace"], &["user_id"], true, false),
                no_input_redacted(&["settings", "client", "get"], "读取客户端偏好。", false, &["secrets"]),
                stdin_redacted(&["settings", "client", "put"], "替换客户端偏好（自由键值映射；null 值删除对应键）。", &[], &["user_id"], true, false, &["secrets"]),
            ]),
            domain("agents", &[
                no_input_redacted(&["agents", "list"], "列出智能体目录和自定义智能体。", false, &["env"]),
                stdin_redacted(&["agents", "overrides", "get"], "读取智能体覆盖配置。", &["agent_id"], &[], false, false, &["env", "secret overrides"]),
            ]),
            domain("cron", &[
                no_input(&["cron", "jobs", "list"], "列出定时任务。", false),
                stdin(&["cron", "jobs", "get"], "读取一个定时任务。", &["job_id"], &[], false, false),
                stdin(&["cron", "jobs", "create"], "创建定时任务。", &["name", "schedule", "message", "conversation_id", "created_by"], &["conversation_id"], true, false),
                stdin(&["cron", "jobs", "update"], "更新定时任务。", &["job_id"], &["conversation_id", "user_id"], true, false),
                stdin(&["cron", "jobs", "delete"], "删除定时任务。", &["job_id"], &[], true, true),
                stdin(&["cron", "jobs", "run"], "立即运行定时任务。", &["job_id"], &[], false, false),
                stdin(&["cron", "jobs", "skill", "get"], "读取定时任务技能状态。", &["job_id"], &[], false, false),
                stdin_redacted(&["cron", "jobs", "skill", "save"], "保存定时任务技能内容。", &["job_id", "content"], &[], true, false, &["content"]),
                stdin_redacted(&["cron", "jobs", "skill", "delete"], "删除定时任务技能内容。", &["job_id"], &[], true, true, &["content"]),
                no_input(&["cron", "current", "list"], "列出当前对话的定时任务。", false),
                stdin(&["cron", "current", "create"], "为当前对话创建定时任务。", &["name", "schedule", "schedule_description", "message"], &["conversation_id"], true, false),
                stdin(&["cron", "current", "update"], "更新当前对话的定时任务。", &["job_id"], &["conversation_id"], true, false),
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

fn no_input(path: &[&str], description: &str, readback: bool) -> Value {
    command(CommandDescriptor {
        path,
        description,
        input: "none",
        stdin_fields: &[],
        selectors: &[],
        readback,
        destructive: false,
        redacted_fields: &[],
    })
}

fn no_input_redacted(path: &[&str], description: &str, readback: bool, redacted_fields: &[&str]) -> Value {
    command(CommandDescriptor {
        path,
        description,
        input: "none",
        stdin_fields: &[],
        selectors: &[],
        readback,
        destructive: false,
        redacted_fields,
    })
}

fn stdin(
    path: &[&str],
    description: &str,
    stdin_fields: &[&str],
    selectors: &[&str],
    readback: bool,
    destructive: bool,
) -> Value {
    command(CommandDescriptor {
        path,
        description,
        input: "stdin_json",
        stdin_fields,
        selectors,
        readback,
        destructive,
        redacted_fields: &[],
    })
}

fn stdin_redacted(
    path: &[&str],
    description: &str,
    stdin_fields: &[&str],
    selectors: &[&str],
    readback: bool,
    destructive: bool,
    redacted_fields: &[&str],
) -> Value {
    command(CommandDescriptor {
        path,
        description,
        input: "stdin_json",
        stdin_fields,
        selectors,
        readback,
        destructive,
        redacted_fields,
    })
}

struct CommandDescriptor<'a> {
    path: &'a [&'a str],
    description: &'a str,
    input: &'a str,
    stdin_fields: &'a [&'a str],
    selectors: &'a [&'a str],
    readback: bool,
    destructive: bool,
    redacted_fields: &'a [&'a str],
}

fn command(spec: CommandDescriptor<'_>) -> Value {
    let requires_context: &[&str] = if spec.path == ["capabilities"] {
        &[]
    } else {
        &RUNTIME_ENV
    };

    json!({
        "path": spec.path,
        "command": format!("config {}", spec.path.join(" ")),
        "description": spec.description,
        "input": spec.input,
        "stdin_fields": spec.stdin_fields,
        "selectors": spec.selectors,
        "readback": spec.readback,
        "destructive": spec.destructive,
        "requires_context": requires_context,
        "redacted_fields": spec.redacted_fields,
    })
}
