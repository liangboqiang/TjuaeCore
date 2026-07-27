---
name: tjuaeui-troubleshooting
description: 诊断正在运行的 TjuaeUI：检查卡住或报错的会话、Provider 健康、定时任务、MCP Server、团队成员、后端状态和 tjuaecore 日志。用户报告会话卡住、模型调用失败、任务未执行、MCP 无工具或团队成员无响应时使用。
---

# TjuaeUI 故障诊断

只使用内置的只读命令 `tjuaecore diagnose`。它会读取当前会话注入的运行时上下文，
不要自行猜测端口或直接拼接后端接口。

## 规则

1. 只调用 `"$TJUAE_HELPER_BIN" diagnose ...`。
2. 对“哪里坏了”这类宽泛问题，先执行 `diagnose overview`。
3. 优先使用命名诊断命令；只有现有命令无法覆盖时才用 `diagnose http get`。
4. 本技能只诊断，不修改配置。配置变更使用 `tjuaeui-config`。
5. 不输出 Provider、MCP Header、Token、密码或密钥原值。
6. 单次看到 `running` 不能证明会话卡住；应重复检查并比较运行时字段、消息进度
   和日志。

## 能力发现

不确定命令或标准输入字段时执行：

```bash
"$TJUAE_HELPER_BIN" diagnose capabilities
```

输出是 Agent 可读契约，包含领域、命令、JSON 字段、Selector、脱敏字段和受控
HTTP 读取入口。

## 当前会话 Selector

```json
{
  "conversation_id": "current"
}
```

`current` 由 `TJUAE_CONVERSATION_ID` 解析。CLI 还会从运行时上下文读取
`TJUAE_BASE_URL` 和 `TJUAE_USER_ID`，禁止手工回显或修改这些变量。

## 从全局概览开始

```bash
"$TJUAE_HELPER_BIN" diagnose overview
```

- `providers.unhealthy`：检查 Provider。
- `mcp.enabled_but_no_tools`：检查 MCP 启动和工具注册。
- `cron.failing`：检查定时任务。若为空但仍怀疑 Cron，执行
  `diagnose cron summary` 并检查 `all[].state.last_status`。
- `running_conversations`：重复检查相应会话的运行时和消息。

## 按症状深入

### 会话卡住或报错

```bash
"$TJUAE_HELPER_BIN" diagnose conversations get <<'JSON'
{
  "conversation_id": "current"
}
JSON
```

读取最近错误消息：

```bash
"$TJUAE_HELPER_BIN" diagnose conversations messages <<'JSON'
{
  "conversation_id": "current",
  "limit": 30,
  "errors_only": true
}
JSON
```

只有多次检查都看不到 `turn_id`、运行时或消息变化时，才能确认疑似卡住。
`state=waiting_confirmation` 或 `pending_confirmations > 0` 表示等待用户批准，
不是卡住。

### Provider 或模型失败

```bash
"$TJUAE_HELPER_BIN" diagnose providers summary
```

检查非 `healthy` 状态、过旧的 `last_check`、高延迟和错误摘要。命名命令足够时
不要索取原始 Provider JSON。

### 定时任务未执行

```bash
"$TJUAE_HELPER_BIN" diagnose cron summary
```

基本字段为 `enabled`、`name`、`id`；运行状态位于 `state.last_status`、
`state.last_error`、`state.run_count`、`state.retry_count`、
`state.next_run_at_ms`、`state.last_run_at_ms`。时间戳是毫秒 Epoch。

### MCP Server 没有工具

```bash
"$TJUAE_HELPER_BIN" diagnose mcp summary
```

已启用但 `tool_count=0` 通常表示启动失败、命令不可用、凭据错误，或在工具注册
前崩溃。

### 团队成员无响应

```bash
"$TJUAE_HELPER_BIN" diagnose teams summary
```

找到成员的 `conversation_id` 后，再执行 `diagnose conversations get` 检查该会话。

### 后端健康

```bash
"$TJUAE_HELPER_BIN" diagnose health
```

用于确认后端可达，并读取 Core 版本和构建信息。

### 日志

```bash
"$TJUAE_HELPER_BIN" diagnose logs tail <<'JSON'
{
  "lines": 100,
  "errors_only": true,
  "conversation_id": "current"
}
JSON
```

若运行时没有 `TJUAE_LOG_DIR`，而用户提供了日志目录，可在 JSON 中传入
`log_dir`。`No onPostToolUseHook found for tool use ID` 可能是工具调用附近的
已知无害噪声，不能自动认定为根因。

## 受控 HTTP 读取

只有命名命令无法覆盖时才使用：

```bash
"$TJUAE_HELPER_BIN" diagnose http get <<'JSON'
{
  "path": "/api/teams",
  "reason": "检查 teams summary 未覆盖的团队字段。"
}
JSON
```

约束：

- 只允许 GET。
- 路径必须是 `/health` 或以 `/api/` 开头。
- 输出会脱敏，且可能截断。
- 同类读取频繁出现时，应后续增加命名 CLI 命令。

## 命令映射

| 诊断目标 | 命令 |
| --- | --- |
| 后端存活与版本 | `diagnose health` |
| 跨领域概览 | `diagnose overview` |
| 会话运行时 | `diagnose conversations get` |
| 会话消息 | `diagnose conversations messages` |
| Provider 健康 | `diagnose providers summary` |
| 定时任务 | `diagnose cron summary` |
| MCP Server | `diagnose mcp summary` |
| 团队和成员 | `diagnose teams summary` |
| 日志 | `diagnose logs tail` |
| 未覆盖的只读接口 | `diagnose http get` |

## 报告要求

- 本技能只诊断，不修复。
- 配置修改切换到 `tjuaeui-config`；定时任务变更使用 `cron`。
- 说明证据和不确定性：一次快照只能写“疑似卡住”，多次快照无变化后才能写
  “确认卡住”。
