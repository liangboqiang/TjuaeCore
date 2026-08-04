-- 发布账本只记录 TjuaeHub 原子资产包身份，不再保留旧扩展包术语。
ALTER TABLE github_publish_operations
    RENAME COLUMN extension_name TO package_name;
