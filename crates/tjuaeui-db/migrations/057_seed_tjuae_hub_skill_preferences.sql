-- The original TjuaeHub bundle shipped these skills enabled. This is the
-- complete one-time product preset: future Hub skills have no row and keep
-- the canonical disabled / no-auto-inject defaults.
INSERT INTO skill_user_preferences
    (source, namespace, slug, selected_version, follow_latest, enabled, auto_inject, updated_at)
VALUES
    ('tjuae-hub', 'official', 'cron',                     '1.0.0', 1, 1, 1, 0),
    ('tjuae-hub', 'official', 'mermaid',                  '1.0.0', 1, 1, 0, 0),
    ('tjuae-hub', 'official', 'pdf',                      '1.0.0', 1, 1, 0, 0),
    ('tjuae-hub', 'official', 'skill-creator',            '1.0.0', 1, 1, 1, 0),
    ('tjuae-hub', 'official', 'story-roleplay',           '1.0.0', 1, 1, 0, 0),
    ('tjuae-hub', 'official', 'tjuaeui-config',           '1.0.0', 1, 1, 1, 0),
    ('tjuae-hub', 'official', 'tjuaeui-troubleshooting',  '1.0.0', 1, 1, 0, 0),
    ('tjuae-hub', 'official', 'tjuaeui-webui-public',     '1.0.0', 1, 1, 0, 0),
    ('tjuae-hub', 'official', 'tjuaeui-webui-setup',      '1.0.0', 1, 1, 0, 0),
    ('tjuae-hub', 'official', 'weixin-file-send',         '1.0.0', 1, 1, 0, 0),
    ('tjuae-hub', 'official', 'x-recruiter',              '1.0.0', 1, 1, 0, 0),
    ('tjuae-hub', 'official', 'xiaohongshu-recruiter',    '1.0.0', 1, 1, 0, 0)
ON CONFLICT(source, namespace, slug) DO NOTHING;
