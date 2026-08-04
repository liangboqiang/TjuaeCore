-- Official skills are Hub assets. Local assistant and conversation state now
-- records only the positive set of selected skill ids; the former builtin
-- exclusion model is deliberately discarded and is not migrated forward.
ALTER TABLE assistants DROP COLUMN disabled_builtin_skills;
ALTER TABLE assistant_definitions DROP COLUMN default_disabled_builtin_skill_ids;
ALTER TABLE assistant_preferences DROP COLUMN last_disabled_builtin_skill_ids;
ALTER TABLE conversation_assistant_snapshots DROP COLUMN resolved_disabled_builtin_skill_ids;
