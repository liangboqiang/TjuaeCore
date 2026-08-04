---
name: tjuaeui-config
description: 通过内置 tjuaecore config CLI 配置 TjuaeUI：创建和编辑助手、规则与技能，管理 MCP Server、模型 Provider、设置、Agent 和定时任务。用户要求修改 TjuaeUI 配置，或询问配置何时影响当前/新会话时使用。
---

# TjuaeUI 配置

只使用面向 Agent 的内置 CLI 配置 TjuaeUI。不要探测端口、直接调用后端路径，
也不要依赖 `tjuaecore` 之外的配置工具。

## 规则

1. 只调用 `"$TJUAE_HELPER_BIN" config ...`。
2. 禁止传递、内联、导出、回显或设置任何 `TJUAE_...` 环境变量。
3. 所有业务输入都通过标准输入 JSON 传递，不使用命令 Flag。
4. 不确定命令或字段时先执行 `config capabilities`。
5. 修改当前助手前先读取上下文；任何写操作都遵守“先读、再写、写后回读”。
6. 修改本会话使用的助手时传 `"assistant_id": "current"`。
7. 支持会话 Selector 的命令使用 `"conversation_id": "current"`。
8. 除非后续操作确实需要，不向用户显示内部 ID。
9. 禁止泄漏 Provider Key、MCP Header、环境变量和其他密钥。
10. CLI 失败时，以 stderr 中稳定的 `CONFIG_...` 错误为准，不得声称已修改。
11. 助手修改成功后必须说明持久化状态和生效时机；回读成功不代表当前运行会话
    已重新加载。

## 输出

成功命令返回 JSON Envelope：

```json
{
  "success": true,
  "data": {},
  "meta": {
    "schema_version": 1
  }
}
```

失败时 stderr 输出一行稳定错误，应以 stderr 为权威结果。

## 能力和上下文

```bash
"$TJUAE_HELPER_BIN" config capabilities
"$TJUAE_HELPER_BIN" config context
```

`capabilities` 会列出领域、命令路径、输入模式、标准输入字段、Selector、回读和
破坏性标记。`context` 返回当前用户、会话、助手和本地运行时。若
`data.assistant` 为 `null`，先询问要编辑哪个助手。

## 助手修改的生效时机

- 名称、描述、头像和推荐提示词会立即保存；界面仍显示旧值时让用户刷新或重新
  打开助手页面。
- Agent、默认模型、默认权限、技能、MCP、思考级别和规则等运行时字段只影响
  从该助手新建的会话。
- 技能和 MCP 不会追加入当前 Agent Runtime；当前会话没有对应能力时，应新建
  会话。

报告运行时字段变更时，应明确说“已保存并回读，将在新会话中生效”。

## 助手

列出和读取：

```bash
"$TJUAE_HELPER_BIN" config assistants list
```

```bash
"$TJUAE_HELPER_BIN" config assistants get <<'JSON'
{
  "assistant_id": "current",
  "locale": "zh-CN"
}
JSON
```

创建：

```bash
"$TJUAE_HELPER_BIN" config assistants create <<'JSON'
{
  "name": "需求分析助手",
  "description": "把粗略产品想法整理为清晰 PRD",
  "agent_id": "2d23ff1c",
  "prompts": [
    "把这个功能想法整理成 PRD",
    "审阅这份 PRD 并指出新用户难以理解的部分"
  ],
  "enabled_skills": ["tjuaeui-config"]
}
JSON
```

更新：

```bash
"$TJUAE_HELPER_BIN" config assistants update <<'JSON'
{
  "assistant_id": "current",
  "locale": "zh-CN",
  "description": "更新后的助手描述",
  "defaults": {
    "permission": {
      "mode": "fixed",
      "value": "plan"
    }
  }
}
JSON
```

MCP 默认值使用 `defaults.mcps`，不得使用 `default_mcp_ids`。启停或排序：

```bash
"$TJUAE_HELPER_BIN" config assistants state <<'JSON'
{
  "assistant_id": "current",
  "enabled": true,
  "sort_order": 10
}
JSON
```

## 助手规则

规则是定义助手行为的 System Prompt。

```bash
"$TJUAE_HELPER_BIN" config assistants rule read <<'JSON'
{
  "assistant_id": "current",
  "locale": "zh-CN"
}
JSON
```

```bash
"$TJUAE_HELPER_BIN" config assistants rule write <<'JSON'
{
  "assistant_id": "current",
  "locale": "zh-CN",
  "content": "# 角色\n你是……"
}
JSON
```

编辑时保留用户现有的有效指令，除非用户明确要求整体替换。写入或删除成功后说明
规则已保存并回读，但只影响新会话。

## 技能

```bash
"$TJUAE_HELPER_BIN" config skills list
```

导入前检查目录：

```bash
"$TJUAE_HELPER_BIN" config skills info <<'JSON'
{
  "skill_path": "/absolute/path/to/skill"
}
JSON
```

```bash
"$TJUAE_HELPER_BIN" config skills import <<'JSON'
{
  "skill_path": "/absolute/path/to/skill-or-parent-or-zip"
}
JSON
```

为助手配置技能时，先读取现有列表，在本地合并后发送完整 `enabled_skills`：

```bash
"$TJUAE_HELPER_BIN" config assistants update <<'JSON'
{
  "assistant_id": "current",
  "enabled_skills": ["tjuaeui-config", "cron"]
}
JSON
```

禁止盲目追加。新增默认技能只影响新会话。

管理外部技能路径：

```bash
"$TJUAE_HELPER_BIN" config skills external-paths list
```

```bash
"$TJUAE_HELPER_BIN" config skills external-paths add <<'JSON'
{
  "name": "团队技能",
  "path": "/absolute/path/to/team-skills"
}
JSON
```

```bash
"$TJUAE_HELPER_BIN" config skills external-paths remove <<'JSON'
{
  "path": "/absolute/path/to/team-skills"
}
JSON
```

技能市场开关：

```bash
"$TJUAE_HELPER_BIN" config skills market enable
"$TJUAE_HELPER_BIN" config skills market disable
```

## MCP Server

```bash
"$TJUAE_HELPER_BIN" config mcp servers list
```

创建：

```bash
"$TJUAE_HELPER_BIN" config mcp servers create <<'JSON'
{
  "name": "本地工具",
  "transport": {
    "type": "stdio",
    "command": "my-mcp-server",
    "args": [],
    "env": {}
  }
}
JSON
```

更新：

```bash
"$TJUAE_HELPER_BIN" config mcp servers update <<'JSON'
{
  "server_id": "mcp_123",
  "description": "更新后的描述"
}
JSON
```

测试：

```bash
"$TJUAE_HELPER_BIN" config mcp test-connection <<'JSON'
{
  "name": "本地工具",
  "transport": {
    "type": "stdio",
    "command": "my-mcp-server",
    "args": []
  }
}
JSON
```

OAuth 状态：

```bash
"$TJUAE_HELPER_BIN" config mcp oauth check-status <<'JSON'
{
  "server_url": "https://mcp.example.com"
}
JSON
```

不得向用户显示 MCP Header 或 stdio 环境变量；CLI 默认会脱敏。

## 模型 Provider

```bash
"$TJUAE_HELPER_BIN" config providers list
```

创建和更新：

```bash
"$TJUAE_HELPER_BIN" config providers create <<'JSON'
{
  "name": "OpenAI",
  "platform": "openai",
  "base_url": "https://api.openai.com/v1",
  "api_key": "sk-..."
}
JSON
```

```bash
"$TJUAE_HELPER_BIN" config providers update <<'JSON'
{
  "provider_id": "provider_123",
  "api_key": "sk-..."
}
JSON
```

协议检测、模型列表和健康检查：

```bash
"$TJUAE_HELPER_BIN" config providers detect-protocol <<'JSON'
{"base_url":"https://api.example.com/v1","api_key":"..."}
JSON
```

```bash
"$TJUAE_HELPER_BIN" config providers models fetch <<'JSON'
{"provider_id":"provider_123"}
JSON
```

```bash
"$TJUAE_HELPER_BIN" config providers health-check <<'JSON'
{"provider_id":"provider_123","model":"gpt-4.1"}
JSON
```

禁止重复或展示用户输入的 API Key。

## 设置

```bash
"$TJUAE_HELPER_BIN" config settings get
```

```bash
"$TJUAE_HELPER_BIN" config settings patch <<'JSON'
{
  "language": "zh-CN",
  "notification_enabled": true
}
JSON
```

支持字段：`language`、`notification_enabled`、
`cron_notification_enabled`、`command_queue_enabled`、
`save_upload_to_workspace`。未知字段会被忽略。

客户端偏好：

```bash
"$TJUAE_HELPER_BIN" config settings client get
```

```bash
"$TJUAE_HELPER_BIN" config settings client put <<'JSON'
{
  "ui.zoomFactor": 1.2
}
JSON
```

客户端偏好是自由键值 Map；传 `null` 删除键。写入前先读取，不能假设固定 Schema。

## Agent

```bash
"$TJUAE_HELPER_BIN" config agents list
```

启停：

```bash
"$TJUAE_HELPER_BIN" config agents enable <<'JSON'
{"agent_id":"codex","enabled":true}
JSON
```

读取和设置覆盖：

```bash
"$TJUAE_HELPER_BIN" config agents overrides get <<'JSON'
{"agent_id":"codex"}
JSON
```

```bash
"$TJUAE_HELPER_BIN" config agents overrides set <<'JSON'
{"agent_id":"codex","command_override":"/absolute/path/to/codex"}
JSON
```

自定义 Agent：

```bash
"$TJUAE_HELPER_BIN" config agents custom create <<'JSON'
{"name":"自定义 Agent","command":"/absolute/path/to/agent-cli"}
JSON
```

```bash
"$TJUAE_HELPER_BIN" config agents custom try-connect <<'JSON'
{"command":"/absolute/path/to/agent-cli"}
JSON
```

`try-connect` 不持久化。禁止显示 Agent 环境变量或秘密覆盖值。

## 定时任务

当前会话使用 `cron current`。每个会话最多一个任务：

```bash
"$TJUAE_HELPER_BIN" config cron current list
```

```bash
"$TJUAE_HELPER_BIN" config cron current create <<'JSON'
{
  "name": "每日总结",
  "schedule": "0 18 * * MON-FRI",
  "schedule_description": "工作日下午 6:00",
  "message": "回顾会话上下文并生成简洁的日终总结。"
}
JSON
```

```bash
"$TJUAE_HELPER_BIN" config cron current update <<'JSON'
{
  "job_id": "cron_123",
  "name": "每日总结",
  "schedule": "0 18 * * MON-FRI",
  "schedule_description": "工作日下午 6:00",
  "message": "回顾会话上下文并生成简洁的日终总结。"
}
JSON
```

成功后用普通语言说明任务名和时间，不展示 `cron_...` ID。

全局管理使用 `config cron jobs`：

```bash
"$TJUAE_HELPER_BIN" config cron jobs list
```

```bash
"$TJUAE_HELPER_BIN" config cron jobs create <<'JSON'
{
  "name": "每周报告",
  "schedule": {
    "kind": "cron",
    "expr": "0 9 * * MON",
    "tz": "Asia/Shanghai"
  },
  "message": "生成每周报告。",
  "conversation_id": "current",
  "created_by": "user"
}
JSON
```

`schedule` 是带标签对象：

- `{ "kind": "cron", "expr": "<cron-expr>", "tz": "<IANA-tz>" }`：Cron；
- `{ "kind": "every", "every_ms": <milliseconds> }`：固定间隔；
- `{ "kind": "at", "at_ms": <epoch-ms> }`：一次性时间点。

`cron jobs` 与 `cron current` 的 `schedule` 形状不同，禁止混用。

```bash
"$TJUAE_HELPER_BIN" config cron jobs run <<'JSON'
{"job_id":"cron_123"}
JSON
```

```bash
"$TJUAE_HELPER_BIN" config cron jobs skill save <<'JSON'
{
  "job_id": "cron_123",
  "content": "# 技能\n任务专用指令。"
}
JSON
```

## 安全

配置会直接影响用户正在使用的应用。变更范围应尽量小，用自然语言说明实际修改，
用户未要求实现细节时不要展示原始 JSON。
