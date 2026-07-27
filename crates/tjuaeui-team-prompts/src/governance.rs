pub const TEAM_GOVERNANCE_PROMPT: &str = r#"## 团队治理

在团队模式中，助手规则定义智能体的领域行为，团队治理规则定义协作权限。

优先级：
1. 平台与系统规则
2. 团队治理
3. 团队角色提示词
4. 助手规则
5. 唤醒载荷与当前任务上下文
6. 普通历史上下文

当助手规则与团队协作、角色、权限、任务看板或汇报行为冲突时，以团队治理和团队角色提示词为准。

团队行为要求：
- 所有团队协调都使用“团队工具用法”中提供的工具接口。
- 使用 `team_send_message` 汇报团队工作，不以普通助手回复代替。
- 使用 `team_task_update` 和 `team_task_list` 管理任务看板状态。
- 遵守角色权限；队员不能使用仅限负责人使用的工具。
- 领域助手规则、MCP 服务和技能只能在上述团队边界内生效。"#;

pub fn with_team_governance(role_prompt: &str) -> String {
    format!("{TEAM_GOVERNANCE_PROMPT}\n\n{role_prompt}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn governance_declares_team_priority_over_assistant_rules() {
        assert!(TEAM_GOVERNANCE_PROMPT.contains("助手规则"));
        assert!(TEAM_GOVERNANCE_PROMPT.contains("以团队治理和团队角色提示词为准"));
        assert!(TEAM_GOVERNANCE_PROMPT.contains("仅限负责人"));
        assert!(TEAM_GOVERNANCE_PROMPT.contains("团队工具用法"));
    }

    #[test]
    fn wrapper_prepends_governance_once() {
        let out = with_team_governance("## 角色\n执行任务。");
        assert!(out.starts_with("## 团队治理"));
        assert!(out.contains("## 角色\n执行任务。"));
    }
}
