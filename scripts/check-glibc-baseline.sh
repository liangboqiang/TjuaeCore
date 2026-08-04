#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "用法：$0 <二进制文件> <GLIBC 版本上限>" >&2
  exit 2
fi

binary="$1"
max_glibc="$2"

if [[ ! -f "${binary}" ]]; then
  echo "未找到二进制文件：${binary}" >&2
  exit 2
fi

if [[ ! "${max_glibc}" =~ ^GLIBC_[0-9]+[.][0-9]+$ ]]; then
  echo "GLIBC 版本上限无效：${max_glibc}" >&2
  exit 2
fi

symbols="$(objdump -T "${binary}")"
required_glibc="$(
  printf '%s\n' "${symbols}" \
    | grep -oE 'GLIBC_[0-9]+[.][0-9]+' \
    | sort -Vu \
    | tail -1 \
    || true
)"

if [[ -z "${required_glibc}" ]]; then
  echo "未在 ${binary} 中找到带版本的 GLIBC 符号" >&2
  exit 2
fi

highest="$(
  printf '%s\n%s\n' "${required_glibc}" "${max_glibc}" \
    | sort -Vu \
    | tail -1
)"

if [[ "${highest}" != "${max_glibc}" ]]; then
  echo "${binary} 需要的 GLIBC ${required_glibc} 超过了上限 ${max_glibc}" >&2
  exit 1
fi

echo "${binary} 需要的 GLIBC ${required_glibc} 未超过上限 ${max_glibc}"
