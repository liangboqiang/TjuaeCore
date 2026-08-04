-- Publishing has one durable ledger: github_publish_operations.
-- asset_publications was introduced but never read or written by runtime code.
-- Remove it instead of maintaining two divergent operation histories.
DROP TABLE IF EXISTS asset_publications;

-- Generic local-library operations never executed publishing. Remove any
-- development-era placeholder rows so GitHub publishing has exactly one
-- authoritative ledger and one state machine.
DELETE FROM asset_operations WHERE kind = 'publish';

-- Editable assistants now live exclusively in AssetCatalog workspaces, while
-- runtime state lives in assistant_definitions/assistant_overlays. The old
-- writable mirror tables must not survive in the final schema.
DROP TABLE IF EXISTS assistant_overrides;
DROP TABLE IF EXISTS assistants;
