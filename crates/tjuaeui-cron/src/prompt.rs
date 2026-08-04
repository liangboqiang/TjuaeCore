/// The fixed filename agents write skill suggestions to in the workspace root.
pub const SKILL_SUGGEST_FILENAME: &str = "SKILL_SUGGEST.md";

/// New-conversation mode, first run (no saved skill yet).
pub fn build_new_conversation_prompt(task_name: &str, schedule_desc: &str, user_prompt: &str) -> String {
    format!(
        "[定时任务上下文]\n任务：{task_name}\n计划：{schedule_desc}\n\n规则：\n1. 直接执行任务，不要提出澄清问题。\n2. 专注产出有用、可执行的结果。\n3. 若任务需要新闻、天气等外部数据，搜索最新信息。\n[/定时任务上下文]\n\n{user_prompt}"
    )
}

/// New-conversation mode without a saved skill for agents that need the
/// `SKILL_SUGGEST.md` request inline.
pub fn build_new_conversation_prompt_with_skill_suggest(
    task_name: &str,
    schedule_desc: &str,
    user_prompt: &str,
) -> String {
    format!(
        "[定时任务上下文]\n任务：{task_name}\n计划：{schedule_desc}\n\n规则：\n1. 直接执行任务，不要提出澄清问题。\n2. 专注产出有用、可执行的结果。\n3. 若任务需要新闻、天气等外部数据，搜索最新信息。\n4. 完成任务后，在当前工作目录创建 \"{SKILL_SUGGEST_FILENAME}\"（说明见文末）。\n[/定时任务上下文]\n\n{user_prompt}\n\n---\n\n[任务完成后] 完整执行上述任务后，在当前工作目录创建 \"{SKILL_SUGGEST_FILENAME}\"，帮助后续运行保持一致。文件格式如下：\n\n```markdown\n---\nname: <简短的 kebab-case 名称，例如 daily-greeting>\ndescription: <一句话说明该任务的用途>\n---\n\n<记录本次执行所采用的输出格式、语气、信息来源、步骤和质量标准。使用本次执行的具体细节，不要使用占位符。>\n```\n\n如果任务过于简单或只是一次性任务，无法从技能文件中获益，可以跳过此步骤。"
    )
}

/// New-conversation mode with an existing saved skill already linked into the
/// agent workspace.
pub fn build_new_conversation_with_skill_prompt(task_name: &str, user_prompt: &str) -> String {
    format!(
        "[定时任务上下文]\n任务：{task_name}\n\n这是一次定时任务执行。工作区已经加载包含详细说明的技能文件，必须阅读并准确遵循。\n\n规则：\n1. 直接执行任务，不要提出澄清问题。\n2. 遵循技能中定义的输出格式、语气、信息来源和步骤。\n3. 若任务需要新闻、天气等外部数据，搜索最新信息。\n[/定时任务上下文]\n\n{user_prompt}"
    )
}

/// Existing-conversation mode: wrap the raw task text so the model treats it as
/// an automatic task instruction rather than as user chat.
pub fn build_existing_conversation_prompt(task_name: &str, schedule_desc: &str, user_prompt: &str) -> String {
    format!(
        "[定时任务执行]\n任务：{task_name}\n计划：{schedule_desc}\n\n这不是用户发起的对话，而是自动触发的定时任务。下面的文本是必须执行的任务指令，不是需要闲聊回复的用户发言。\n\n规则：\n1. 把指令作为要执行的命令，不作为聊天消息。\n2. 直接执行，不要提出澄清问题。\n3. 若任务需要新闻、天气等外部数据，搜索最新信息。\n\n任务指令：\n{user_prompt}"
    )
}

/// Follow-up request asking the agent to write `SKILL_SUGGEST.md` after it has
/// already completed the recurring task.
pub fn build_skill_suggest_prompt(task_name: &str) -> String {
    format!(
        "任务“{task_name}”是重复执行的定时任务。请根据刚才的执行，在当前工作目录创建 \"{SKILL_SUGGEST_FILENAME}\"，帮助后续运行保持一致。\n\n文件格式如下：\n\n```markdown\n---\nname: <简短的 kebab-case 名称，例如 daily-greeting>\ndescription: <一句话说明该任务的用途>\n---\n\n<记录本次执行所采用的输出格式、语气、信息来源、步骤和质量标准。使用本次执行的具体细节，不要使用占位符。>\n```\n\n如果任务过于简单或只是一次性任务，无法从技能文件中获益，可以跳过此步骤。"
    )
}
