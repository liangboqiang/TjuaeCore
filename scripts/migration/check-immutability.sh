#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

duplicate_versions="$(
    find crates/tjuaeui-db/migrations -maxdepth 1 -type f -name '*.sql' -print \
        | awk -F/ '
            {
                name = $NF
                if (name ~ /^[0-9]+_/) {
                    version = name
                    sub(/_.*/, "", version)
                    version += 0
                    count[version]++
                    files[version] = files[version] (files[version] == "" ? "" : ", ") name
                }
            }
            END {
                for (version in count) {
                    if (count[version] > 1) {
                        print version ": " files[version]
                    }
                }
            }
        ' \
        | sort
)"

if [[ -n "$duplicate_versions" ]]; then
    cat >&2 <<'EOF'
不允许数据库迁移版本号重复。

请将后添加的迁移重命名为下一个未使用的数字前缀。

重复版本：
EOF
    echo "$duplicate_versions" >&2
    exit 1
fi

if [[ "${TJUAECORE_ALLOW_MAIN_MIGRATION_EDIT:-}" == "1" ]]; then
    echo "TJUAECORE_ALLOW_MAIN_MIGRATION_EDIT=1；已显式允许修改主分支迁移，跳过不可变检查"
    exit 0
fi

base_ref="${TJUAECORE_MIGRATION_BASE_REF:-}"
if [[ -z "$base_ref" ]]; then
    if git rev-parse --verify --quiet origin/main >/dev/null; then
        base_ref="origin/main"
    elif git rev-parse --verify --quiet main >/dev/null; then
        base_ref="main"
    else
        echo "未找到 origin/main 或 main 引用，跳过迁移不可变检查"
        exit 0
    fi
fi

if ! git rev-parse --verify --quiet "$base_ref" >/dev/null; then
    echo "未找到迁移不可变检查的基准引用：$base_ref" >&2
    exit 1
fi

base_commit="$(git merge-base HEAD "$base_ref")"
changed="$(
    git diff --name-status --diff-filter=DMR "$base_commit" -- 'crates/tjuaeui-db/migrations/*.sql'
)"

if [[ -n "$changed" ]]; then
    cat >&2 <<'EOF'
不得修改或删除主分支已有的迁移文件。

请还原对已有迁移文件的修改，并改为添加下一编号的新迁移。
如果这是有意执行的高风险例外，请设置 TJUAECORE_ALLOW_MAIN_MIGRATION_EDIT=1 后重试。

已变更的现有迁移：
EOF
    echo "$changed" >&2
    exit 1
fi

echo "迁移不可变检查通过"
