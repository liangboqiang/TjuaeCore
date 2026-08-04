-- Core 资产的用户私有凭据与一次性试跑回执。
--
-- 密文由领域服务使用按 user/asset/slot/keyVersion 派生的 AES-256-GCM
-- 密钥生成；数据库层永远不接收明文。两张表都引用 Definition 所有者，
-- 因此删除本地资产时会由外键级联清理，不会遗留孤立凭据或回执。

CREATE TABLE IF NOT EXISTS asset_credentials (
    user_id TEXT NOT NULL,
    asset_owner_id TEXT NOT NULL,
    asset_id TEXT NOT NULL,
    slot TEXT NOT NULL CHECK (
        length(trim(slot)) BETWEEN 1 AND 128
        AND instr(slot, char(0)) = 0
    ),
    ciphertext TEXT NOT NULL CHECK (length(ciphertext) > 0),
    key_version INTEGER NOT NULL CHECK (key_version > 0),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, asset_id, slot),
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE,
    FOREIGN KEY (asset_owner_id, asset_id)
        REFERENCES asset_records(user_id, id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_asset_credentials_owner
    ON asset_credentials(asset_owner_id, asset_id);

CREATE TABLE IF NOT EXISTS asset_try_run_receipts (
    user_id TEXT NOT NULL,
    asset_owner_id TEXT NOT NULL,
    asset_id TEXT NOT NULL,
    receipt_id TEXT NOT NULL,
    idempotency_key TEXT NOT NULL CHECK (
        length(trim(idempotency_key)) BETWEEN 1 AND 128
    ),
    definition_digest TEXT NOT NULL,
    overlay_version INTEGER NOT NULL CHECK (overlay_version >= 0),
    runtime_id TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, asset_id),
    UNIQUE (user_id, receipt_id),
    UNIQUE (user_id, idempotency_key),
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE,
    FOREIGN KEY (asset_owner_id, asset_id)
        REFERENCES asset_records(user_id, id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_asset_try_run_receipts_owner
    ON asset_try_run_receipts(asset_owner_id, asset_id);

ALTER TABLE asset_runtime_bindings
    ADD COLUMN try_run_receipt_id TEXT;
