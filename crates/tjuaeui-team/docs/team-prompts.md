# 团队提示词体系

本文档说明 TjuaeCore 当前团队提示词的职责边界。提示词正文以
`crates/tjuaeui-team-prompts/src/` 为唯一事实来源，工具说明以
`crates/tjuaeui-api-types/src/team_tools.rs` 为唯一事实来源；本文不复制整段提示词，
避免文档与运行时代码漂移。

## 角色层次

- 负责人提示词：规划人员方案、征得用户批准、创建队员、拆分任务、跟踪进度并汇总结果。
- 队员提示词：接收分配、独立执行、通过 `team_*` 工具汇报、处理下线请求并结束回合。
- 恢复提示词：在进程重启或会话恢复时重新注入身份、治理规则、当前任务和待处理消息。

团队协调只能通过 `team_*` MCP 工具完成，不得用普通文本假装已创建队员、分配任务
或发送消息。

## 人员确认流程

1. 判断增加队员是否有明确价值。
2. 用 `team_list_assistants` 获取真实助手目录；存在多个相关候选时再调用
   `team_describe_assistant`。
3. 向用户说明价值，并用表格列出名称、职责和推荐助手。
4. 等待用户明确批准；提出方案的同一回合不得调用 `team_spawn_agent`。
5. 使用目录返回的 `assistant_id` 创建队员，不猜测后端名称，不直接传模型。
6. 用 `team_task_create` 建立任务，再用 `team_send_message` 分配工作。
7. 汇总队员结果并向用户交付；需要下线时使用 `team_shutdown_agent` 完成握手。

## 工具职责

| 工具 | 用途 | 权限 |
| --- | --- | --- |
| `team_list_assistants` | 列出可用于创建队员的助手 | 全体成员 |
| `team_describe_assistant` | 查看助手完整说明、技能和示例任务 | 全体成员 |
| `team_spawn_agent` | 创建并加入新队员 | 仅负责人 |
| `team_send_message` | 单播或广播团队消息 | 全体成员 |
| `team_task_create` | 创建共享任务 | 全体成员 |
| `team_task_update` | 更新任务状态、负责人和结果 | 全体成员 |
| `team_task_list` | 查询共享任务看板 | 全体成员 |
| `team_members` | 查询成员、角色和运行状态 | 全体成员 |
| `team_rename_agent` | 重命名队员 | 仅负责人 |
| `team_shutdown_agent` | 发起队员下线握手 | 仅负责人 |

## 状态与时序

队员的一次标准工作流为：收到任务 → 标记进行中 → 执行 → 更新任务结果 →
向负责人汇报 → 停止生成并结束回合。没有待办任务时进入等待状态；新消息、任务变更或
下线请求会再次唤醒队员。

## 维护要求

- 修改角色提示词后，运行 `tjuaeui-team-prompts` 和 `tjuaeui-team` 测试。
- 修改工具说明后，同时运行 `tjuaeui-app` 中的
  `team_stdio_descriptions_match_prompt_registry`，保证标准输入 MCP 门面与注册表一致。
- 不在本文记录历史 UI 路径、已移除工具或旧版兼容流程。
