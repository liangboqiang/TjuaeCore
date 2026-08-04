#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../.." && pwd)"
script="$repo_root/scripts/migration/check-immutability.sh"

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

run_in_repo() {
    local cwd="$1"
    local expected_status="$2"
    local expected_text="$3"
    shift 3

    set +e
    output="$(cd "$cwd" && "$@" 2>&1)"
    status=$?
    set -e

    if [[ "$status" != "$expected_status" ]]; then
        echo "期望状态码 $expected_status，实际为 $status" >&2
        echo "$output" >&2
        exit 1
    fi

    if [[ -n "$expected_text" && "$output" != *"$expected_text"* ]]; then
        echo "期望输出包含：$expected_text" >&2
        echo "$output" >&2
        exit 1
    fi
}

init_case_repo() {
    local name="$1"
    local dir="$tmpdir/$name"

    mkdir -p "$dir/crates/tjuaeui-db/migrations"
    (
        cd "$dir"
        git init -q -b main
        git config user.email test@example.com
        git config user.name "Migration Test"
        printf '%s\n' '-- 001 initial' > crates/tjuaeui-db/migrations/001_initial_schema.sql
        printf '%s\n' '-- 002 data fix' > crates/tjuaeui-db/migrations/002_data_fix.sql
        printf '%s\n' '-- auxiliary sql' > crates/tjuaeui-db/migrations/manual_fixture.sql
        git add crates/tjuaeui-db/migrations
        git commit -q -m "seed migrations"
        git checkout -q -b feature
    )

    printf '%s\n' "$dir"
}

modified_repo="$(init_case_repo modified)"
printf '%s\n' '-- modified' >> "$modified_repo/crates/tjuaeui-db/migrations/001_initial_schema.sql"
run_in_repo "$modified_repo" 1 "不得修改或删除主分支已有的迁移文件" \
    env TJUAECORE_MIGRATION_BASE_REF=main bash "$script"

deleted_repo="$(init_case_repo deleted)"
rm "$deleted_repo/crates/tjuaeui-db/migrations/002_data_fix.sql"
run_in_repo "$deleted_repo" 1 "不得修改或删除主分支已有的迁移文件" \
    env TJUAECORE_MIGRATION_BASE_REF=main bash "$script"

auxiliary_repo="$(init_case_repo auxiliary)"
printf '%s\n' '-- modified auxiliary sql' >> "$auxiliary_repo/crates/tjuaeui-db/migrations/manual_fixture.sql"
run_in_repo "$auxiliary_repo" 1 "不得修改或删除主分支已有的迁移文件" \
    env TJUAECORE_MIGRATION_BASE_REF=main bash "$script"

added_repo="$(init_case_repo added)"
printf '%s\n' '-- 003 new migration' > "$added_repo/crates/tjuaeui-db/migrations/003_new_change.sql"
run_in_repo "$added_repo" 0 "迁移不可变检查通过" \
    env TJUAECORE_MIGRATION_BASE_REF=main bash "$script"

duplicate_repo="$(init_case_repo duplicate)"
printf '%s\n' '-- duplicate 002 migration' > "$duplicate_repo/crates/tjuaeui-db/migrations/002_duplicate_change.sql"
run_in_repo "$duplicate_repo" 1 "不允许数据库迁移版本号重复" \
    env TJUAECORE_MIGRATION_BASE_REF=main bash "$script"

override_repo="$(init_case_repo override)"
printf '%s\n' '-- modified with explicit override' >> "$override_repo/crates/tjuaeui-db/migrations/001_initial_schema.sql"
run_in_repo "$override_repo" 0 "跳过不可变检查" \
    env TJUAECORE_MIGRATION_BASE_REF=main TJUAECORE_ALLOW_MAIN_MIGRATION_EDIT=1 bash "$script"

echo "迁移不可变检查脚本测试通过"
