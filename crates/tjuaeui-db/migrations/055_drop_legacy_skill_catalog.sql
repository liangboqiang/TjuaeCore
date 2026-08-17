-- Canonical skill state lives in each package's `.tjuae-skill.json` and Git
-- repository. Remove the retired database catalog and import-history ledger so
-- there is one source of truth.
DROP TABLE IF EXISTS skill_import_records;
DROP TABLE IF EXISTS skills;
