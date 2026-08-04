#!/usr/bin/env bash
set -euo pipefail

config_file="${TJUAE_CONFIG_DEV_FILE:-$HOME/.tjuaeui-config-dev/tjuaeui-config.txt}"
if [[ ! -f "$config_file" ]]; then
    echo "未找到配置文件：$config_file" >&2
    exit 1
fi

if base64 --help 2>&1 | grep -q -- "-D"; then
    decoded=$(base64 -D -i "$config_file")
else
    decoded=$(base64 -d "$config_file")
fi

plain=$(printf "%s" "$decoded" | python3 -c 'import sys, urllib.parse; print(urllib.parse.unquote(sys.stdin.read()))')

if command -v pbcopy >/dev/null 2>&1; then
    printf "%s" "$plain" | pbcopy
    echo "配置已复制到剪贴板"
elif command -v wl-copy >/dev/null 2>&1; then
    printf "%s" "$plain" | wl-copy
    echo "配置已复制到剪贴板"
elif command -v xclip >/dev/null 2>&1; then
    printf "%s" "$plain" | xclip -selection clipboard
    echo "配置已复制到剪贴板"
else
    printf "%s\n" "$plain"
fi
