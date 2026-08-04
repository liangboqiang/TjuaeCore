#!/usr/bin/env bash
set -euo pipefail

mode="${1:-release}"
case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*) exe_suffix=".exe" ;;
    *) exe_suffix="" ;;
esac

case "$mode" in
    release) binary="target/release/tjuaecore$exe_suffix" ;;
    debug) binary="target/debug/tjuaecore$exe_suffix" ;;
    *) echo "未知安装模式：$mode" >&2; exit 1 ;;
esac

if [[ ! -f "$binary" ]]; then
    echo "未找到二进制文件：$binary" >&2
    exit 1
fi

cargo_home="${CARGO_HOME:-$HOME/.cargo}"
install_dir="$cargo_home/bin"
mkdir -p "$install_dir"
cp "$binary" "$install_dir/"


echo "已将 $(basename "$binary") 安装到 $install_dir"
