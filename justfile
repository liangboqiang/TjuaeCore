# 默认操作：列出所有可用任务
set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command"]

cargo_script := if os_family() == "windows" { "powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File scripts/just/cargo.ps1" } else { "bash scripts/just/cargo.sh" }
build_script := if os_family() == "windows" { "powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File scripts/just/build.ps1" } else { "bash scripts/just/build.sh" }
install_script := if os_family() == "windows" { "powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File scripts/just/install.ps1" } else { "bash scripts/just/install.sh" }
migration_check_script := if os_family() == "windows" { "powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File scripts/migration/check-immutability.ps1" } else { "bash scripts/migration/check-immutability.sh" }
migration_check_test_script := if os_family() == "windows" { "powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File scripts/migration/check-immutability.test.ps1" } else { "bash scripts/migration/check-immutability.test.sh" }
identity_check_script := if os_family() == "windows" { "powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File scripts/quality/check-identity.ps1" } else { "bash scripts/quality/check-identity.sh" }
auto_commit_script := if os_family() == "windows" { "powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File scripts/just/auto-commit-fixes.ps1" } else { "bash scripts/just/auto-commit-fixes.sh" }
update_tjuae_cli_script := if os_family() == "windows" { "powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File scripts/just/update-tjuae-cli.ps1" } else { "bash scripts/just/update-tjuae-cli.sh" }
cat_config_script := if os_family() == "windows" { "powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File scripts/just/cat-config.ps1" } else { "bash scripts/just/cat-config.sh" }

default:
    @just --list

# 启用提交前 Git 钩子（克隆后运行一次）
setup:
    git config core.hooksPath .githooks
    @echo "Git 钩子已启用"

# 运行 Cargo，并可按需使用本地 TjuaeCLI SDK 补丁
_cargo *ARGS:
    @{{cargo_script}} {{ARGS}}

# 以发布模式构建并安装到 Cargo 二进制目录
# 使用 `just build --force` 跳过缓存检查
build *FLAGS: lint-fix fmt
    @{{build_script}} release {{FLAGS}}

# 以调试模式构建
# 使用 `just build-debug --force` 跳过缓存检查
build-debug *FLAGS:
    @{{build_script}} debug {{FLAGS}}

install:
    @{{install_script}} release

# 运行全部测试
test:
    @just _cargo nextest run --workspace

# 确保已经发布的数据库迁移保持不可变
migration-check:
    @{{migration_check_script}}

# 测试数据库迁移不可变守卫
migration-check-test:
    @{{migration_check_test_script}}

# 阻止旧品牌、已移除黑盒能力和外部推广内容重新进入仓库
identity-check:
    @{{identity_check_script}}

# 静态检查（警告视为错误）
lint:
    @just _cargo clippy --workspace -- -D warnings

lint-fix:
    @just _cargo fix --allow-dirty --allow-staged
    @just _cargo clippy --fix --workspace --allow-dirty --allow-staged -- -D warnings

# 格式化代码
fmt:
    @cargo fmt --all

# 检查代码格式（持续集成）
fmt-check:
    @cargo fmt --all -- --check

# 身份检查、静态检查、格式检查、迁移检查和测试
check: identity-check migration-check lint fmt-check test

# 完整验证
verify: check

# 以调试模式运行服务
run *ARGS:
    @just _cargo run --bin tjuaecore -- {{ARGS}}

# 以发布模式运行服务
run-release *ARGS:
    @just _cargo run --release --bin tjuaecore -- {{ARGS}}

# 推送前门禁：迁移检查、格式化、静态检查、自动提交修复、测试，然后推送
push *ARGS: migration-check lint-fix fmt _auto-commit-fixes test
    git push {{ARGS}}

# 如格式化或静态检查产生修改，则自动提交
_auto-commit-fixes:
    @{{auto_commit_script}}

# 更新 TjuaeCLI 依赖（例如 `just update-tjuae-cli` 或 `just update-tjuae-cli v0.3.0`）
update-tjuae-cli *TAG:
    @{{update_tjuae_cli_script}} {{TAG}}

# 安全审计
audit:
    @cargo audit

# 清理构建产物
clean:
    @cargo clean

# 解码开发配置，并在可用时复制到剪贴板
cat-config:
    @{{cat_config_script}}
