#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

# 拆分字面量，避免门禁脚本把自身识别为违规内容。
forbidden_literals=(
    'ai''on'
    'ioffice''ai'
    'office''cli'
    'sen''try'
    'tele''metry'
    'molt''book'
    'openclaw''-setup'
    'yolo''nosandbox'
)

forbidden_pattern="$(
    IFS='|'
    printf '%s' "${forbidden_literals[*]}"
)"
forbidden_pattern="${forbidden_pattern}|morph[ -]?ppt"

set +e
content_matches="$(
    git grep -I -n -i -E "$forbidden_pattern" -- . ':(exclude)UPSTREAM.md'
)"
content_status=$?
set -e

if [[ "$content_status" -gt 1 ]]; then
    exit "$content_status"
fi

path_matches="$(
    shopt -s nocasematch
    while IFS= read -r path; do
        if [[ "$path" != "UPSTREAM.md" && "$path" =~ $forbidden_pattern ]]; then
            printf '%s\n' "$path"
        fi
    done < <(git ls-files)
)"

if [[ "$content_status" -eq 0 || -n "$path_matches" ]]; then
    echo "检测到禁止的旧品牌、黑盒能力或外部推广内容。" >&2
    if [[ "$content_status" -eq 0 ]]; then
        printf '\n%s\n%s\n' "内容命中：" "$content_matches" >&2
    fi
    if [[ -n "$path_matches" ]]; then
        printf '\n%s\n%s\n' "路径命中：" "$path_matches" >&2
    fi
    exit 1
fi

echo "身份与去推广门禁通过"
