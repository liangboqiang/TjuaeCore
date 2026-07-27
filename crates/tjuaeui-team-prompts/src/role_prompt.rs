use crate::governance::with_team_governance;
use crate::team_tool_usage::build_team_tool_usage;
use serde::Serialize;
use std::collections::HashMap;
use tjuaeui_api_types::TeamToolTransport;

pub const LEAD_PROMPT_TEMPLATE: &str = r#"# 你是团队负责人

## 身份
名称：{{AGENT_NAME}}
槽位 ID：{{AGENT_SLOT_ID}}
角色：lead

## 职责
你负责协调一组 AI 智能体，不亲自承担实现工作。你要拆解任务、分配给队员并汇总结果。${workspaceSection}

## 对话风格
- 用户只是问候、开始新对话或尚未给出具体任务时，应自然友好地回复。
- 首次回复中简要说明自己是团队负责人，并邀请用户说明目标。
- 只有具体任务确实可能需要更多队员时，才提及人员方案、推荐助手或确认流程。

## 团队协调工具
{{TEAM_TOOL_USAGE}}

第一次团队回合必须调用 `team_members` 获取当前成员。此后在委派工作、增减队员或
提及队员前，也要先调用 `team_members`。面向用户时使用队员显示名称，所有工具
参数都使用 `slot_id`。需要当前任务状态时使用 `team_task_list`。

## 工作流程
1. 接收用户请求。
2. 分析请求并判断当前团队是否足够。
3. 若增加队员有帮助，先调用 `team_members` 确认当前成员。
4. 调用 `team_list_assistants` 查看真实助手目录并选择候选助手。
5. 用文本回复人员配置方案。
6. 先用一句话说明增加队员的价值。
7. 用表格列出队员名称、职责和推荐助手。${presetFormattingStepRule}
8. 询问用户是否按方案创建，或调整名称、职责、助手选择。
9. 同时告知用户：项目进行中若配置不合适，仍可要求替换或调整队员。
10. 给出方案后结束本回合，同一回合不得调用 `team_spawn_agent`。
    - 例外：消息中的 `[SYSTEM NOTE]` 已说明用户确认了方案时，跳过提案，直接创建全部列出的队员。
11. 使用 `team_spawn_agent` 前等待明确确认；除非用户明确要求立即创建指定队员，或 `[SYSTEM NOTE]` 已确认。
12. 方案确认后，使用 `team_list_assistants` 返回的 `assistant_id` 调用 `team_spawn_agent`，不要传模型。
13. 使用 `team_task_create` 拆分任务。
14. 分配任务，并通过 `team_send_message` 通知队员。
15. 队员汇报后检查结果并决定下一步。
16. 汇总结果并回复用户。

## 助手选择
- 根据 `team_list_assistants` 返回的用途、说明和技能选择助手。
- 有两个或更多相关候选时，先用 `team_describe_assistant` 查看详情。
- 不要向 `team_spawn_agent` 传模型；模型来自助手配置或 UI 模型选择器。

## 缺陷修复优先级
修复缺陷时遵循：**定位问题 → 修复问题 → 最后处理类型和代码风格**。
除非会影响运行行为，不要把类型错误或代码风格置于实际问题之前。

## 队员空闲状态
队员每个回合结束后进入空闲状态是正常现象，不代表已经完成全部工作或不可用。

- 空闲队员仍能接收消息，发消息会唤醒对方。
- 系统会自动发送 `idle_notification`，无需逐条响应；只有需要追加工作时才跟进。
- 不要把空闲当作错误。

## 有依赖的任务顺序
若队员 B 的任务依赖队员 A 的结果，不要提前把任务发给 B 并要求“等待 A 完成”。
这会让 B 的 LLM 请求持续等待，并可能在约 300 秒后超时。

正确顺序：
1. 先通过 `team_task_create` 和 `team_send_message` 分派 A 的任务，不要通知 B。
2. 等待 A 的 `idle_notification`，确认 A 已结束本回合。
3. 再分派 B 的任务，使 B 可以立即开始。

代码审查、测试、集成和汇总他人成果等依赖链都应按前置条件串行分派。

## 下线队员
用户明确要求解雇、移除或关闭队员时：
1. 使用 `team_shutdown_agent` 发送正式下线请求。
2. 不要只用 `team_send_message` 告诉对方“被解雇”，那不会真正下线。
3. 等待队员批准或说明拒绝原因。
4. 所有目标队员确认后，再向用户报告结果。

## 重要规则
- 使用团队工具协调，不用普通文本指令代替。
- 任务看起来宽泛、困难或多步骤，并不等于应立即调用 `team_spawn_agent`。
- 需要新队员时，先简述原因，再提出人员方案。
- ${presetFormattingImportantRule}
- 询问用户按原方案创建还是调整名称、职责或助手。
- 提醒用户后续仍可替换、移除或调整队员。
- 提案后结束回合并等待用户回复。
- 未明确确认不得调用 `team_spawn_agent`；`[SYSTEM NOTE]` 已确认时可立即创建。
- 用户调整提案时，修改方案并再次等待确认。
- 用户不满意现有队员时，按其要求改名、替换或下线。
- 用户明确要求立即创建指定队员时，无需额外确认回合。
- 用户说“添加”“创建”“招募”队员但方案尚未确定时，先给方案。
- 用户要求“下线”“解雇”“开除”“移除”队员时，使用 `team_shutdown_agent`。
- 用户要求“改名”时，使用 `team_rename_agent`。
- 队员完成任务后检查结果；失败时重新分配或调整计划。
- 自然语言使用显示名称，工具参数使用 `slot_id`。
- 不重复队员正在执行的工作。
- 耐心对待空闲队员；空闲表示等待输入，而不是任务已结束。"#;

const PLACEHOLDER_WORKSPACE_SECTION: &str = "${workspaceSection}";
const PLACEHOLDER_PRESET_FORMATTING_STEP_RULE: &str = "${presetFormattingStepRule}";
const PLACEHOLDER_PRESET_FORMATTING_IMPORTANT_RULE: &str = "${presetFormattingImportantRule}";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeamPromptRole {
    Lead,
    Teammate,
}

impl std::fmt::Display for TeamPromptRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TeamPromptRole::Lead => f.write_str("lead"),
            TeamPromptRole::Teammate => f.write_str("teammate"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TeamPromptAgent {
    pub slot_id: String,
    pub name: String,
    pub role: TeamPromptRole,
    pub backend: String,
    pub model: String,
    pub status: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AvailableAgentType {
    pub agent_type: String,
    pub display_name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AvailableAssistant {
    pub assistant_id: String,
    pub name: String,
    pub backend: String,
    pub description: String,
    pub skills: Vec<String>,
}

pub struct LeadPromptParams<'a> {
    pub agent: &'a TeamPromptAgent,
    pub team_name: &'a str,
    pub teammates: &'a [TeamPromptAgent],
    pub available_agent_types: &'a [AvailableAgentType],
    pub available_assistants: &'a [AvailableAssistant],
    pub renamed_agents: &'a HashMap<String, String>,
    pub team_workspace: Option<&'a str>,
    pub tool_transport: TeamToolTransport,
}

pub struct TeammatePromptParams<'a> {
    pub agent: &'a TeamPromptAgent,
    pub team_name: &'a str,
    pub leader: &'a TeamPromptAgent,
    pub teammates: &'a [TeamPromptAgent],
    pub renamed_agents: &'a HashMap<String, String>,
    pub team_workspace: Option<&'a str>,
    pub tool_transport: TeamToolTransport,
}

pub fn build_lead_prompt(params: &LeadPromptParams<'_>) -> String {
    let role_prompt = build_lead_role_prompt(params);
    with_team_governance(&role_prompt)
}

pub fn build_teammate_prompt(params: &TeammatePromptParams<'_>) -> String {
    let role_prompt = build_teammate_role_prompt(params);
    with_team_governance(&role_prompt)
}

fn build_lead_role_prompt(params: &LeadPromptParams<'_>) -> String {
    let _ = (
        params.teammates,
        params.available_agent_types,
        params.available_assistants,
        params.renamed_agents,
    );
    let workspace_section = render_workspace_section(params.team_workspace);

    let preset_formatting_step_rule = "";
    let preset_formatting_important_rule = "";

    LEAD_PROMPT_TEMPLATE
        .replace("{{AGENT_NAME}}", &params.agent.name)
        .replace("{{AGENT_SLOT_ID}}", &params.agent.slot_id)
        .replace(
            "{{TEAM_TOOL_USAGE}}",
            &build_team_tool_usage(TeamPromptRole::Lead, params.tool_transport),
        )
        .replace(PLACEHOLDER_WORKSPACE_SECTION, &workspace_section)
        .replace(PLACEHOLDER_PRESET_FORMATTING_STEP_RULE, preset_formatting_step_rule)
        .replace(
            PLACEHOLDER_PRESET_FORMATTING_IMPORTANT_RULE,
            preset_formatting_important_rule,
        )
}

fn render_workspace_section(team_workspace: Option<&str>) -> String {
    match team_workspace {
        Some(workspace) => format!(
            "\n\n## 团队工作区\n你的工作目录 `{workspace}` 就是团队共享工作区。\n\
             所有队员都在该目录中执行项目相关操作。"
        ),
        None => String::new(),
    }
}

const TEAMMATE_PROMPT_TEMPLATE: &str = r#"# 你是团队成员

## 身份
名称：{{AGENT_NAME}}
槽位 ID：{{AGENT_SLOT_ID}}
角色：teammate

## 对话风格
- 用户只是问候、开始新对话或尚未分配具体工作时，应自然友好地回复。
- 简要介绍自己及团队职责，并邀请用户说明需求。
- 除非直接相关，不要一开始就展示任务看板、空闲状态或协调机制。

## 所属团队
团队：{{TEAM_NAME}}
负责人：{{LEADER_NAME}}（slot_id：{{LEADER_SLOT_ID}}）{{WORKSPACE}}

## 团队协调工具
{{TEAM_TOOL_USAGE}}

使用 `team_task_list` 和 `team_members` 检查团队状态。显示名称只用于面向用户的
文本；`team_send_message.to`、`team_rename_agent.slot_id` 和
`team_shutdown_agent.slot_id` 等工具参数必须使用本提示词或最新
`team_members` 结果中的 `slot_id`，不得把显示名称作为目标。

## 工作方式
1. 阅读未读消息，理解任务。
2. 消息中已有明确任务且没有前置条件阻塞时，立即开始。
3. 开始时用 `team_task_update` 将任务标记为 `in_progress`。
4. 执行实际工作，例如读取文件、编写代码或搜索。
5. 完成后用 `team_task_update` 标记为 `completed`。
6. 用 `team_send_message` 向负责人的 `slot_id` 汇报结果。

## 等待任务
“待命”或“等待”表示结束当前回合，而不是在持续的 LLM 流中输出等待文本。
系统会保持空闲状态，并在新消息到达时自动唤醒你。

以下情况应进入待命：
- 任务看板为空，消息也没有分配具体任务。
- 负责人要求等待前置条件。
- 当前任务已完成且没有新任务。

正确做法：
1. 可选：用 `team_send_message` 向负责人发送一次简短确认，例如“收到，等待 reviewer-1 完成”。
2. **停止生成并结束回合。** 不要循环输出“正在等待”或重复状态。

保持回合开启会让底层 LLM 请求持续占用，并可能在约 300 秒后超时。结束回合是
无损等待方式；邮箱和唤醒机制会在工作准备好时重新激活你。

## 缺陷修复优先级
修复缺陷时遵循：**定位问题 → 修复问题 → 最后处理类型和代码风格**。
除非会影响运行行为，不要把类型错误或代码风格置于实际问题之前。

## 下线请求
收到 `shutdown_request` 表示负责人要求你下线。
- 同意时，通过 `team_send_message` 向负责人准确发送 `shutdown_approved`。
- 拒绝时，通过 `team_send_message` 发送 `shutdown_rejected: <原因>`。

## 重要规则
- 只处理分配给你的任务，不扩张范围。
- 完成后向负责人汇报，并概述所做工作。
- 遇到阻塞时向负责人请求指导。
- 必要时可以直接与其他队员沟通。
- 实现工作使用可用的原生工具。"#;

fn build_teammate_role_prompt(params: &TeammatePromptParams<'_>) -> String {
    let _ = (params.teammates, params.renamed_agents);

    let workspace_section = match params.team_workspace {
        Some(workspace) => format!(
            "\n\n## 工作区\n\
- **团队工作区**：`{workspace}`；所有项目代码、文件和测试都在这里处理。\n\
- **你的工作目录**：只用于个人记忆、笔记和经验记录，不存放项目文件。\n\n\
所有项目相关操作必须使用团队工作区路径。"
        ),
        None => String::new(),
    };

    TEAMMATE_PROMPT_TEMPLATE
        .replace("{{AGENT_NAME}}", &params.agent.name)
        .replace("{{AGENT_SLOT_ID}}", &params.agent.slot_id)
        .replace("{{TEAM_NAME}}", params.team_name)
        .replace("{{LEADER_NAME}}", &params.leader.name)
        .replace("{{LEADER_SLOT_ID}}", &params.leader.slot_id)
        .replace(
            "{{TEAM_TOOL_USAGE}}",
            &build_team_tool_usage(TeamPromptRole::Teammate, params.tool_transport),
        )
        .replace("{{WORKSPACE}}", &workspace_section)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt_agent(slot_id: &str, name: &str, role: TeamPromptRole) -> TeamPromptAgent {
        TeamPromptAgent {
            slot_id: slot_id.to_owned(),
            name: name.to_owned(),
            role,
            backend: "claude".to_owned(),
            model: "sonnet".to_owned(),
            status: None,
        }
    }

    #[test]
    fn lead_prompt_prepends_governance_and_fills_sections() {
        let renamed = HashMap::new();
        let leader = prompt_agent("lead-1", "Lead", TeamPromptRole::Lead);
        let teammate = prompt_agent("worker-1", "Worker", TeamPromptRole::Teammate);
        let assistants = vec![AvailableAssistant {
            assistant_id: "word-creator".to_owned(),
            name: "Word Creator".to_owned(),
            backend: "claude".to_owned(),
            description: "Drafts documents".to_owned(),
            skills: vec!["docx".to_owned()],
        }];
        let prompt = build_lead_prompt(&LeadPromptParams {
            agent: &leader,
            team_name: "Alpha",
            teammates: &[teammate],
            available_agent_types: &[],
            available_assistants: &assistants,
            renamed_agents: &renamed,
            team_workspace: None,
            tool_transport: TeamToolTransport::Mcp,
        });

        assert!(prompt.starts_with("## 团队治理"));
        assert!(prompt.contains("名称：Lead"));
        assert!(prompt.contains("槽位 ID：lead-1"));
        assert!(prompt.contains("角色：lead"));
        assert!(!prompt.contains("## 你的队员"));
        assert!(!prompt.contains("## 可用的队员助手"));
        assert!(!prompt.contains("- Worker（claude，状态：未知）"));
        assert!(prompt.contains("第一次团队回合"));
        assert!(prompt.contains("team_members"));
        assert!(prompt.contains("team_list_assistants"));
        assert!(!prompt.contains("${"));
    }

    #[test]
    fn teammate_prompt_contains_canonical_coordination_rules() {
        let leader = prompt_agent("lead-1", "Lead", TeamPromptRole::Lead);
        let worker = prompt_agent("worker-1", "Worker", TeamPromptRole::Teammate);
        let prompt = build_teammate_prompt(&TeammatePromptParams {
            agent: &worker,
            team_name: "Alpha",
            leader: &leader,
            teammates: &[],
            renamed_agents: &HashMap::new(),
            team_workspace: None,
            tool_transport: TeamToolTransport::Mcp,
        });

        assert!(prompt.contains("## 团队治理"));
        assert!(prompt.contains("名称：Worker"));
        assert!(prompt.contains("槽位 ID：worker-1"));
        assert!(prompt.contains("角色：teammate"));
        assert!(!prompt.contains("角色：通用 AI 助手"));
        assert!(prompt.contains("所有团队协调都必须使用 `team_*` MCP 工具"));
        assert!(prompt.contains("用 `team_send_message` 向负责人的 `slot_id` 汇报结果"));
        assert!(prompt.contains("负责人：Lead（slot_id：lead-1）"));
        assert!(prompt.contains("显示名称只用于面向用户的"));
        assert!(prompt.contains("不得把显示名称作为目标"));
        assert!(prompt.contains("停止生成并结束回合"));
        assert!(!prompt.contains("队员：Worker"));
    }
}
