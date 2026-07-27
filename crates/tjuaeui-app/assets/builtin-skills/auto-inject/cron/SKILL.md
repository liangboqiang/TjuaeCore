---
name: cron
description: 管理定时任务：查询、创建和更新当前会话的任务，并在指定时间自动执行。
---

# 定时任务

通过内置的 Agent 配置 CLI 管理当前会话的定时任务。

## 规则

1. 每个会话最多有一个定时任务。
2. 创建或更新前必须先查询现有任务。
3. 用户已明确要求变更时，不要再次确认。
4. 禁止传递、内联、导出、回显或设置任何 `TJUAE_...` 环境变量。
5. 命令必须直接调用 `"$TJUAE_HELPER_BIN" config cron current ...`。
6. 创建和更新 JSON 通过命令的标准输入 heredoc 传入，不写入磁盘。
7. `job_id` 放在更新 JSON 中，不放在命令参数里。
8. 成功后只给普通用户能理解的简短确认，包含任务名和时间描述，不显示
   `cron_...` 等内部 ID。
9. CLI 失败时，用正常语言转述 stderr/stdout，不得声称任务已创建。

## 流程

1. 执行 `"$TJUAE_HELPER_BIN" config cron current list`。
2. 返回的 `data` 为空时，用 `... current create <<'JSON'` 创建。
3. 已有任务且用户要修改时，用 `... current update <<'JSON'` 更新。
4. 已有任务而用户要求新增另一任务时，询问如何处理原任务。
5. 根据 CLI 输出报告结果，并遵守上述确认规则。

## 请求体

创建字段：

- `name`：简短任务名。
- `schedule`：标准五字段 Cron 表达式。
- `schedule_description`：用户可读的时间描述。
- `message`：任务触发时发送给 AI 的完整、自包含指令。

更新使用相同字段，并额外要求：

- `job_id`：查询命令返回的现有任务 ID。

`message` 必须准确说明触发时要做什么，不能只复述“设置一个定时任务”。

| 用户要求 | 错误 message | 正确 message |
| --------------------------------- | ------------------------ | -------------------------------------------------------------------------------------------- |
| 每天 10 点发一句你好 | 发一句你好 | 只回复：你好！ |
| 每天提醒喝水 | 提醒喝水 | 用友好的语气提醒用户喝水。 |
| 每周一总结 AI 新闻 | 总结 AI 新闻 | 搜索本周最新 AI 新闻并生成简洁要点。 |

## 示例

真实任务中的名称、时间描述和消息应使用用户语言。

查询：

```bash
"$TJUAE_HELPER_BIN" config cron current list
```

创建：

```bash
"$TJUAE_HELPER_BIN" config cron current create <<'JSON'
{
  "name": "每周会议提醒",
  "schedule": "0 9 * * MON",
  "schedule_description": "每周一上午 9:00",
  "message": "回复一条简短的每周会议提醒，并包含当前日期和时间。"
}
JSON
```

更新：

```bash
"$TJUAE_HELPER_BIN" config cron current update <<'JSON'
{
  "job_id": "cron_123",
  "name": "每日总结",
  "schedule": "0 18 * * MON-FRI",
  "schedule_description": "工作日下午 6:00",
  "message": "回顾今天的会话上下文并生成简洁的日终总结。"
}
JSON
```

多行消息示例：

```json
{
  "name": "每日总结",
  "schedule": "0 9 * * *",
  "schedule_description": "每天上午 9:00",
  "message": "第一段。\n第二段。\n第三段。"
}
```

## Cron 表达式

格式：`minute hour day-of-month month day-of-week`。

例如 `0 9 * * MON-FRI` 表示工作日上午 9:00。

只能使用后端解析器支持的标准字段和范围；禁止 Quartz 扩展，如 `L`、`L-N`、
`W`、`LW`、`#`、`?`。
