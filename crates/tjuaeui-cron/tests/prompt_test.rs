use tjuaeui_cron::prompt::{
    SKILL_SUGGEST_FILENAME, build_existing_conversation_prompt, build_new_conversation_prompt,
    build_new_conversation_prompt_with_skill_suggest, build_new_conversation_with_skill_prompt,
    build_skill_suggest_prompt,
};

#[test]
fn build_new_conversation_prompt_matches_frontend_copy() {
    let prompt = build_new_conversation_prompt("Daily Report", "Every day at 9am", "Summarize it.");
    assert_eq!(
        prompt,
        "[定时任务上下文]\n任务：Daily Report\n计划：Every day at 9am\n\n规则：\n1. 直接执行任务，不要提出澄清问题。\n2. 专注产出有用、可执行的结果。\n3. 若任务需要新闻、天气等外部数据，搜索最新信息。\n[/定时任务上下文]\n\nSummarize it."
    );
}

#[test]
fn build_new_conversation_prompt_with_skill_suggest_includes_follow_up_block() {
    let prompt = build_new_conversation_prompt_with_skill_suggest("Daily Report", "Every day at 9am", "Summarize it.");
    assert!(prompt.contains(&format!("创建 \"{SKILL_SUGGEST_FILENAME}\"")));
    assert!(prompt.contains("简短的 kebab-case 名称"));
    assert!(prompt.contains("如果任务过于简单或只是一次性任务"));
}

#[test]
fn build_new_conversation_with_skill_prompt_matches_frontend_copy() {
    let prompt = build_new_conversation_with_skill_prompt("Daily Report", "Summarize it.");
    assert_eq!(
        prompt,
        "[定时任务上下文]\n任务：Daily Report\n\n这是一次定时任务执行。工作区已经加载包含详细说明的技能文件，必须阅读并准确遵循。\n\n规则：\n1. 直接执行任务，不要提出澄清问题。\n2. 遵循技能中定义的输出格式、语气、信息来源和步骤。\n3. 若任务需要新闻、天气等外部数据，搜索最新信息。\n[/定时任务上下文]\n\nSummarize it."
    );
}

#[test]
fn build_existing_conversation_prompt_matches_frontend_copy() {
    let prompt = build_existing_conversation_prompt("Daily Report", "Every day at 9am", "Summarize it.");
    assert_eq!(
        prompt,
        "[定时任务执行]\n任务：Daily Report\n计划：Every day at 9am\n\n这不是用户发起的对话，而是自动触发的定时任务。下面的文本是必须执行的任务指令，不是需要闲聊回复的用户发言。\n\n规则：\n1. 把指令作为要执行的命令，不作为聊天消息。\n2. 直接执行，不要提出澄清问题。\n3. 若任务需要新闻、天气等外部数据，搜索最新信息。\n\n任务指令：\nSummarize it."
    );
}

#[test]
fn build_skill_suggest_prompt_matches_frontend_copy() {
    let prompt = build_skill_suggest_prompt("Daily Report");
    assert!(prompt.starts_with("任务“Daily Report”是重复执行的定时任务。请根据刚才的执行"));
    assert!(prompt.contains("```markdown"));
    assert!(prompt.contains("使用本次执行的具体细节，不要使用占位符。"));
    assert!(prompt.ends_with("如果任务过于简单或只是一次性任务，无法从技能文件中获益，可以跳过此步骤。"));
}
