#!/usr/bin/env bash
set -euo pipefail

workflows=(
  ".github/workflows/release.yml"
  ".github/workflows/build-manual.yml"
)

arm64_cross_rev="29d00c7803f221f1b3f35e561b03792368fb8339"
arm64_cross_image="ghcr.io/cross-rs/aarch64-unknown-linux-gnu@sha256:99e041b94e7d4f31477c6ddede176688562c3762ba3833b75de3316100afc39d"

grep -Fq "${arm64_cross_image}" Cross.toml \
  || {
    echo "Cross.toml 必须将 Linux ARM64 固定到 v0.1.39/v0.1.40 cross 镜像摘要" >&2
    exit 1
  }

grep -Fq 'target: x86_64-unknown-linux-gnu' ".github/workflows/release.yml" \
  && grep -Fq 'os: ubuntu-22.04' ".github/workflows/release.yml" \
  || {
    echo ".github/workflows/release.yml 必须使 Linux x64 继续使用 ubuntu-22.04" >&2
    exit 1
  }

grep -Fq '"platform":"linux-x64","os":"ubuntu-22.04","target":"x86_64-unknown-linux-gnu"' ".github/workflows/build-manual.yml" \
  || {
    echo ".github/workflows/build-manual.yml 必须使 Linux x64 继续使用 ubuntu-22.04" >&2
    exit 1
  }

for workflow in "${workflows[@]}"; do
  if [[ ! -f "${workflow}" ]]; then
    echo "未找到工作流：${workflow}" >&2
    exit 1
  fi

  grep -Fq 'LINUX_X64_GLIBC_MAX: "GLIBC_2.34"' "${workflow}" \
    || {
      echo "${workflow} 必须将 Linux x64 GLIBC 上限固定为 GLIBC_2.34" >&2
      exit 1
    }

  grep -Fq "CROSS_GIT_REV: \"${arm64_cross_rev}\"" "${workflow}" \
    || {
      echo "${workflow} 必须将 cross 固定到 v0.1.39/v0.1.40 Git 修订版" >&2
      exit 1
    }

  grep -Fq 'cargo install cross --git https://github.com/cross-rs/cross --rev "${CROSS_GIT_REV}" --locked' "${workflow}" \
    || {
      echo "${workflow} 必须从固定的 Git 修订版安装 cross" >&2
      exit 1
    }

  grep -Fq "docker pull ${arm64_cross_image}" "${workflow}" \
    || {
      echo "${workflow} 必须预先拉取固定的 Linux ARM64 cross 镜像" >&2
      exit 1
    }

  grep -Fq "matrix.target == 'x86_64-unknown-linux-gnu'" "${workflow}" \
    || {
      echo "${workflow} 必须验证 Linux x64 GLIBC 基线" >&2
      exit 1
    }

  grep -Fq '${LINUX_X64_GLIBC_MAX}' "${workflow}" \
    || {
      echo "${workflow} 必须将 LINUX_X64_GLIBC_MAX 传给 GLIBC 检查器" >&2
      exit 1
    }
done

echo "Linux GLIBC 工作流配置已针对 x64 和 arm64 固定"
