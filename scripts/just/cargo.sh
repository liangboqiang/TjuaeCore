#!/usr/bin/env bash
set -euo pipefail

cargo_config=()
restore_cargo_lock=false
cargo_lock_snapshot=""
tjuae_cli_root=""

restore_local_lockfile() {
    local status=$?

    if [[ -n "$cargo_lock_snapshot" && -f "$cargo_lock_snapshot" ]]; then
        if [[ "$restore_cargo_lock" == "true" || "$status" -ne 0 ]]; then
            cp "$cargo_lock_snapshot" Cargo.lock || status=$?
        fi
    fi
    if [[ -n "$cargo_lock_snapshot" ]]; then
        rm -f "$cargo_lock_snapshot"
    fi

    return "$status"
}
trap restore_local_lockfile EXIT

verify_local_tjuae_cli_patch() {
    local metadata_file
    metadata_file=$(mktemp)
    cargo "${cargo_config[@]}" metadata --format-version 1 > "$metadata_file"

    python3 - "$tjuae_cli_root" "$metadata_file" "${crates[@]}" <<'PY'
import json
import sys
from pathlib import Path

tjuae_cli_root = Path(sys.argv[1]).resolve()
metadata_path = Path(sys.argv[2])
crates = sys.argv[3:]
metadata = json.loads(metadata_path.read_text())
packages = {package["name"]: package for package in metadata["packages"]}

for crate in crates:
    package = packages.get(crate)
    expected = (tjuae_cli_root / "crates" / crate).resolve()
    if not package:
        print(f"{crate} 未使用 TJUAE_CLI 补丁。", file=sys.stderr)
        print("  实际解析：未找到包", file=sys.stderr)
        print(f"  期望路径：{expected}", file=sys.stderr)
        sys.exit(1)

    actual = Path(package["manifest_path"]).resolve().parent
    if actual != expected:
        print(f"{crate} 未使用 TJUAE_CLI 补丁。", file=sys.stderr)
        print(f"  实际解析：{actual}", file=sys.stderr)
        print(f"  期望路径：{expected}", file=sys.stderr)
        sys.exit(1)
PY

    rm -f "$metadata_file"
}

if [[ -n "${TJUAE_CLI:-}" ]]; then
    if [[ ! -d "$TJUAE_CLI" ]]; then
        echo "TJUAE_CLI 不存在或不是目录：$TJUAE_CLI" >&2
        exit 1
    fi

    tjuae_cli_root=$(cd "$TJUAE_CLI" && pwd -P)
    crates=(
        tjuae-agent
        tjuae-compact
        tjuae-config
        tjuae-mcp
        tjuae-memory
        tjuae-process
        tjuae-protocol
        tjuae-providers
        tjuae-skills
        tjuae-tools
        tjuae-types
    )

    for crate in "${crates[@]}"; do
        crate_dir="$tjuae_cli_root/crates/$crate"
        if [[ ! -f "$crate_dir/Cargo.toml" ]]; then
            echo "TJUAE_CLI 缺少 $crate：$crate_dir/Cargo.toml" >&2
            exit 1
        fi

        toml_path=${crate_dir//\\/\\\\}
        toml_path=${toml_path//\"/\\\"}
        cargo_config+=(--config "patch.'https://github.com/liangboqiang/TjuaeCLI.git'.$crate.path = \"$toml_path\"")
    done

    echo "正在使用本地 TjuaeCLI SDK：$tjuae_cli_root" >&2

    if [[ -f Cargo.lock ]]; then
        cargo_lock_snapshot=$(mktemp)
        cp Cargo.lock "$cargo_lock_snapshot"

        if git diff --quiet -- Cargo.lock && git diff --cached --quiet -- Cargo.lock; then
            restore_cargo_lock=true
        else
            echo "Cargo.lock 已有变更；将保留成功解析 TJUAE_CLI 后的锁文件更新。" >&2
        fi
    fi

    echo "正在针对本地 TjuaeCLI SDK 解析 Cargo.lock" >&2
    cargo "${cargo_config[@]}" update \
        -p tjuae-agent \
        -p tjuae-compact \
        -p tjuae-config \
        -p tjuae-mcp \
        -p tjuae-memory \
        -p tjuae-process \
        -p tjuae-protocol \
        -p tjuae-providers \
        -p tjuae-skills \
        -p tjuae-tools \
        -p tjuae-types
    verify_local_tjuae_cli_patch
fi

if ((${#cargo_config[@]})); then
    cargo "${cargo_config[@]}" "$@"
else
    cargo "$@"
fi
