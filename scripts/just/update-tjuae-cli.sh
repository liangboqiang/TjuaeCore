#!/usr/bin/env bash
set -euo pipefail

tag="${1:-}"
if [[ -z "$tag" ]]; then
    tag=$(
        git ls-remote --tags https://github.com/liangboqiang/TjuaeCLI.git |
            python3 -c 'import re, sys; tags=[]; [tags.append(m.group(1)) for line in sys.stdin for m in [re.search(r"refs/tags/(v[0-9]+(?:\.[0-9]+)*(?:[-+][0-9A-Za-z.-]+)?)$", line)] if m]; print(sorted(tags, key=lambda t: [int(p) if p.isdigit() else p for p in re.split(r"[.-]", t.lstrip("v"))])[-1])'
    )
    echo "使用最新标签：$tag"
fi

python3 - "$tag" <<'PY'
from pathlib import Path
import re
import sys

tag = sys.argv[1]
path = Path("Cargo.toml")
text = path.read_text()
pattern = r'git = "https://github\.com/liangboqiang/TjuaeCLI\.git", tag = "[^"]*"'
if re.search(pattern, text) is None:
    raise SystemExit("Cargo.toml 中没有找到 TjuaeCLI Git 依赖标签")
updated = re.sub(
    pattern,
    f'git = "https://github.com/liangboqiang/TjuaeCLI.git", tag = "{tag}"',
    text,
)
path.write_text(updated)
PY

cargo check --workspace
