#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CHECKER="${ROOT_DIR}/scripts/check-glibc-baseline.sh"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

FAKE_OBJDUMP="${TMP_DIR}/objdump"
FAKE_BINARY="${TMP_DIR}/tjuaecore"
touch "${FAKE_BINARY}"

write_fake_objdump() {
  local glibc_version="$1"
  cat > "${FAKE_OBJDUMP}" <<EOF
#!/usr/bin/env bash
cat <<'SYMBOLS'
0000000000000000      DF *UND*  0000000000000000 (GLIBC_2.17) pthread_self
0000000000000000      DF *UND*  0000000000000000 (${glibc_version}) pidfd_spawnp
SYMBOLS
EOF
  chmod +x "${FAKE_OBJDUMP}"
}

write_fake_objdump "GLIBC_2.30"
PATH="${TMP_DIR}:${PATH}" "${CHECKER}" "${FAKE_BINARY}" "GLIBC_2.30"

write_fake_objdump "GLIBC_2.39"
if PATH="${TMP_DIR}:${PATH}" "${CHECKER}" "${FAKE_BINARY}" "GLIBC_2.30" >"${TMP_DIR}/fail.out" 2>&1; then
  echo "期望 GLIBC_2.39 在 GLIBC_2.30 上限下检查失败" >&2
  exit 1
fi

grep -q "超过了上限 GLIBC_2.30" "${TMP_DIR}/fail.out"

cat > "${FAKE_OBJDUMP}" <<'EOF'
#!/usr/bin/env bash
cat <<'SYMBOLS'
0000000000000000      DF *UND*  0000000000000000 pthread_self
SYMBOLS
EOF
chmod +x "${FAKE_OBJDUMP}"

if PATH="${TMP_DIR}:${PATH}" "${CHECKER}" "${FAKE_BINARY}" "GLIBC_2.30" >"${TMP_DIR}/empty.out" 2>&1; then
  echo "期望缺少 GLIBC 符号时检查失败" >&2
  exit 1
fi

grep -q "未在.*中找到带版本的 GLIBC 符号" "${TMP_DIR}/empty.out"
