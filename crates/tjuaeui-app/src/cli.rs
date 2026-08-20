//! CLI argument definitions for the `tjuaecore` binary.
//!
//! Kept separate from `main.rs` to isolate the clap surface (struct + enum +
//! attribute soup) from the runtime entry point. Visibility is `pub(crate)`
//! because only `main.rs` consumes it.

use std::ffi::OsString;
use std::path::PathBuf;

use clap::{Arg, ArgAction, Args, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};

const CHINESE_HELP_TEMPLATE: &str = "{about-with-newline}\n用法：{usage}\n\n{all-args}{after-help}";

fn localize_command(mut command: clap::Command) -> clap::Command {
    let has_version = command.get_version().is_some();
    command = command
        .disable_help_subcommand(true)
        .disable_help_flag(true)
        .disable_version_flag(true)
        .subcommand_help_heading("命令")
        .help_template(CHINESE_HELP_TEMPLATE)
        .arg(
            Arg::new("help")
                .short('h')
                .long("help")
                .action(ArgAction::Help)
                .help("显示帮助")
                .help_heading("选项"),
        );

    if has_version {
        command = command.arg(
            Arg::new("version")
                .short('V')
                .long("version")
                .action(ArgAction::Version)
                .help("显示版本")
                .help_heading("选项"),
        );
    }

    let arguments = command
        .get_arguments()
        .map(|argument| (argument.get_id().clone(), argument.get_index().is_some()))
        .collect::<Vec<_>>();
    for (id, positional) in arguments {
        command = command.mut_arg(id.clone(), move |argument| {
            let mut argument = argument.help_heading(if positional { "参数" } else { "选项" });
            if id == "help" {
                argument = argument.help("显示帮助");
            } else if id == "version" {
                argument = argument.help("显示版本");
            }
            argument
        });
    }

    for subcommand in command.get_subcommands_mut() {
        *subcommand = localize_command(subcommand.clone());
    }

    command
}

#[derive(Parser)]
#[command(name = "tjuaecore", about = "TjuaeUI 后端服务", version)]
pub(crate) struct Cli {
    /// 监听地址。
    #[arg(long, default_value_t = String::from(tjuaeui_common::constants::DEFAULT_HOST))]
    pub host: String,

    /// 监听端口。
    #[arg(long, default_value_t = tjuaeui_common::constants::DEFAULT_PORT)]
    pub port: u16,

    /// 数据库和文件存储目录。
    #[arg(long, default_value = "data")]
    pub data_dir: PathBuf,

    /// 父进程 ID；桌面应用退出时用于终止后端。
    #[arg(long)]
    pub parent_pid: Option<u32>,

    /// 对话工作区目录。
    /// 未指定时依次回退到 TJUAE_WORK_DIR 环境变量和数据目录。
    #[arg(long)]
    pub work_dir: Option<PathBuf>,

    /// 用于扩展引擎兼容性检查的宿主应用版本。
    #[arg(long, default_value_t = env!("CARGO_PKG_VERSION").to_string())]
    pub app_version: String,

    /// 以本地嵌入模式运行（跳过认证并使用 system_default_user）。
    #[arg(long)]
    pub local: bool,

    /// 日志目录，默认为 {data-dir}/logs/。
    #[arg(long)]
    pub log_dir: Option<PathBuf>,

    /// 日志级别过滤器，例如 "info"、"debug" 或 "info,tjuaeui_mcp=trace"。
    #[arg(long)]
    pub log_level: Option<String>,

    /// 将提示词诊断信息写入 {data-dir}/prompt-dumps。
    #[arg(long)]
    pub dump_prompts: bool,

    /// 启动时显式备份疑似损坏的本地数据库并创建新数据库。
    #[arg(long)]
    pub recover_corrupted_database: bool,

    /// 托管运行时资源来源。
    #[arg(long, value_enum, default_value_t = ManagedResourcesModeArg::Download)]
    pub managed_resources_mode: ManagedResourcesModeArg,

    #[command(subcommand)]
    pub command: Option<Command>,
}

impl Cli {
    pub(crate) fn parse_localized() -> Self {
        let matches = localize_command(Self::command()).get_matches();
        Self::from_arg_matches(&matches).unwrap_or_else(|error| error.exit())
    }
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ManagedResourcesModeArg {
    Bundled,
    Download,
}

impl From<ManagedResourcesModeArg> for tjuaeui_runtime::ManagedResourcesMode {
    fn from(value: ManagedResourcesModeArg) -> Self {
        match value {
            ManagedResourcesModeArg::Bundled => Self::Bundled,
            ManagedResourcesModeArg::Download => Self::Download,
        }
    }
}

// `Mcp` prefix is load-bearing on Mcp* variants — clap derives kebab-case
// subcommand names (`mcp-bridge`, `mcp-team-stdio`)
// that external callers (ACP agent CLI, team MCP bridge spec) depend on
// verbatim.
#[derive(Subcommand, Debug)]
pub(crate) enum Command {
    /// 输出面向智能体的顶层 CLI 能力索引。
    Capabilities,
    /// 面向智能体的 TjuaeUI 配置自动化命令。
    Config(ConfigArgs),
    /// 面向智能体的 TjuaeUI 只读诊断命令。
    Diagnose(DiagnoseArgs),
    /// 面向智能体的团队协作备用命令。
    Team(TeamArgs),
    /// 团队 MCP 服务的标准输入输出与 TCP 桥接器（由 ACP 智能体 CLI 启动）。
    McpBridge,
    /// 团队工具的 MCP 标准输入输出服务（由 ACP 智能体 CLI 启动）。
    McpTeamStdio,
    /// 自检：加载智能体注册表，探测 `$PATH` 中的每个 CLI，并输出可用性表。
    /// 用户反馈“所有智能体都不可用”时，可从应用启动所用的同一终端执行，
    /// 先确认各后端是否可检测，再排查服务日志。
    Doctor,
    /// 在指定打包输出根目录下准备当前平台的托管运行时资源。
    PrepareManagedResources(PrepareManagedResourcesArgs),
}

impl Command {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Capabilities => "capabilities",
            Self::Config(_) => "config",
            Self::Diagnose(_) => "diagnose",
            Self::Team(_) => "team",
            Self::McpBridge => "mcp-bridge",
            Self::McpTeamStdio => "mcp-team-stdio",
            Self::Doctor => "doctor",
            Self::PrepareManagedResources(_) => "prepare-managed-resources",
        }
    }

    pub(crate) fn need_runtime(&self) -> bool {
        matches!(self, Self::Doctor | Self::PrepareManagedResources(_))
    }
}

#[derive(Args, Debug, Clone)]
pub(crate) struct ConfigArgs {
    #[command(subcommand)]
    pub command: ConfigCommand,
}

#[derive(Args, Debug, Clone)]
pub(crate) struct DiagnoseArgs {
    #[command(subcommand)]
    pub command: DiagnoseCommand,
}

#[derive(Args, Debug, Clone)]
#[command(disable_help_subcommand = true)]
pub(crate) struct TeamArgs {
    #[command(subcommand)]
    pub command: TeamCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum TeamCommand {
    Capabilities,
    Help,
    Context,
    Members,
    SendMessage,
    Task(TeamTaskArgs),
    ListAssistants,
    DescribeAssistant,
    SpawnAgent,
    RenameAgent,
    ShutdownAgent,
    #[command(external_subcommand)]
    Unknown(Vec<OsString>),
}

#[derive(Args, Debug, Clone)]
pub(crate) struct TeamTaskArgs {
    #[command(subcommand)]
    pub command: TeamTaskCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum TeamTaskCommand {
    Create,
    Update,
    List,
    #[command(external_subcommand)]
    Unknown(Vec<OsString>),
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum DiagnoseCommand {
    /// 输出智能体可读的诊断 CLI 能力契约。
    Capabilities,
    /// 输出当前智能体运行时上下文。
    Context,
    /// 读取后端健康状态。
    Health,
    /// 读取跨领域诊断快照。
    Overview,
    /// 检查对话状态和消息。
    Conversations(DiagnoseConversationsArgs),
    /// 检查模型提供商健康摘要。
    Providers(DiagnoseProvidersArgs),
    /// 检查 MCP 服务摘要。
    Mcp(DiagnoseMcpArgs),
    /// 检查定时任务摘要。
    Cron(DiagnoseCronArgs),
    /// 检查团队摘要。
    Teams(DiagnoseTeamsArgs),
    /// 读取 tjuaecore 日志。
    Logs(DiagnoseLogsArgs),
    /// 执行受控的 HTTP 只读请求。
    Http(DiagnoseHttpArgs),
}

#[derive(Args, Debug, Clone)]
pub(crate) struct DiagnoseConversationsArgs {
    #[command(subcommand)]
    pub command: DiagnoseConversationsCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum DiagnoseConversationsCommand {
    List,
    Get,
    Messages,
}

#[derive(Args, Debug, Clone)]
pub(crate) struct DiagnoseProvidersArgs {
    #[command(subcommand)]
    pub command: DiagnoseSummaryCommand,
}

#[derive(Args, Debug, Clone)]
pub(crate) struct DiagnoseMcpArgs {
    #[command(subcommand)]
    pub command: DiagnoseSummaryCommand,
}

#[derive(Args, Debug, Clone)]
pub(crate) struct DiagnoseCronArgs {
    #[command(subcommand)]
    pub command: DiagnoseSummaryCommand,
}

#[derive(Args, Debug, Clone)]
pub(crate) struct DiagnoseTeamsArgs {
    #[command(subcommand)]
    pub command: DiagnoseSummaryCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum DiagnoseSummaryCommand {
    Summary,
}

#[derive(Args, Debug, Clone)]
pub(crate) struct DiagnoseLogsArgs {
    #[command(subcommand)]
    pub command: DiagnoseLogsCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum DiagnoseLogsCommand {
    Tail,
}

#[derive(Args, Debug, Clone)]
pub(crate) struct DiagnoseHttpArgs {
    #[command(subcommand)]
    pub command: DiagnoseHttpCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum DiagnoseHttpCommand {
    Get,
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum ConfigCommand {
    /// 输出智能体可读的配置 CLI 能力契约。
    Capabilities,
    /// 输出当前智能体运行时上下文。
    Context,
    /// 管理对话。
    Conversation(ConfigConversationArgs),
    /// 管理助手及其行为。
    Assistants(ConfigAssistantsArgs),
    /// 管理 TjuaeUI 技能。
    Skills(ConfigSkillsArgs),
    /// 管理 MCP 服务与 OAuth 状态。
    Mcp(ConfigMcpArgs),
    /// 管理模型提供商。
    Providers(ConfigProvidersArgs),
    /// 管理后端和客户端设置。
    Settings(ConfigSettingsArgs),
    /// 管理智能体目录和自定义智能体。
    Agents(ConfigAgentsArgs),
    /// 管理定时任务。
    Cron(ConfigCronArgs),
}

#[derive(Args, Debug, Clone)]
pub(crate) struct ConfigAssistantsArgs {
    #[command(subcommand)]
    pub command: ConfigAssistantsCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum ConfigAssistantsCommand {
    List,
    Get,
    Create,
    Settings,
    Delete,
    Copy,
    Preferences,
    File(ConfigAssistantFileArgs),
    Prepare,
    Activate,
    Export,
    Publish,
}

#[derive(Args, Debug, Clone)]
pub(crate) struct ConfigAssistantFileArgs {
    #[command(subcommand)]
    pub command: ConfigAssistantFileCommand,
}

#[derive(Subcommand, Debug, Clone, Copy)]
pub(crate) enum ConfigAssistantFileCommand {
    Read,
    Write,
}

#[derive(Args, Debug, Clone)]
pub(crate) struct ConfigSkillsArgs {
    #[command(subcommand)]
    pub command: ConfigSkillsCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum ConfigSkillsCommand {
    List,
    Create,
    Import,
    Delete,
    Copy,
    Preferences,
}

#[derive(Args, Debug, Clone)]
pub(crate) struct ConfigMcpArgs {
    #[command(subcommand)]
    pub command: ConfigMcpCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum ConfigMcpCommand {
    Servers(ConfigMcpServersArgs),
    TestConnection,
    AgentConfigs,
    Oauth(ConfigMcpOauthArgs),
}

#[derive(Args, Debug, Clone)]
pub(crate) struct ConfigMcpServersArgs {
    #[command(subcommand)]
    pub command: ConfigMcpServersCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum ConfigMcpServersCommand {
    List,
    Get,
    Create,
    Update,
    Delete,
    Toggle,
    Import,
}

#[derive(Args, Debug, Clone)]
pub(crate) struct ConfigMcpOauthArgs {
    #[command(subcommand)]
    pub command: ConfigMcpOauthCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum ConfigMcpOauthCommand {
    CheckStatus,
    Login,
    Logout,
    Authenticated,
}

#[derive(Args, Debug, Clone)]
pub(crate) struct ConfigProvidersArgs {
    #[command(subcommand)]
    pub command: ConfigProvidersCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum ConfigProvidersCommand {
    List,
    Create,
    Update,
    Delete,
    DetectProtocol,
    FetchModels,
    Models(ConfigProviderModelsArgs),
    HealthCheck,
}

#[derive(Args, Debug, Clone)]
pub(crate) struct ConfigProviderModelsArgs {
    #[command(subcommand)]
    pub command: ConfigProviderModelsCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum ConfigProviderModelsCommand {
    Fetch,
}

#[derive(Args, Debug, Clone)]
pub(crate) struct ConfigSettingsArgs {
    #[command(subcommand)]
    pub command: ConfigSettingsCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum ConfigSettingsCommand {
    Get,
    Patch,
    Client(ConfigSettingsClientArgs),
}

#[derive(Args, Debug, Clone)]
pub(crate) struct ConfigSettingsClientArgs {
    #[command(subcommand)]
    pub command: ConfigSettingsClientCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum ConfigSettingsClientCommand {
    Get,
    Put,
}

#[derive(Args, Debug, Clone)]
pub(crate) struct ConfigAgentsArgs {
    #[command(subcommand)]
    pub command: ConfigAgentsCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum ConfigAgentsCommand {
    List,
    Enable,
    Overrides(ConfigAgentOverridesArgs),
    Custom(ConfigAgentCustomArgs),
}

#[derive(Args, Debug, Clone)]
pub(crate) struct ConfigAgentOverridesArgs {
    #[command(subcommand)]
    pub command: ConfigAgentOverridesCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum ConfigAgentOverridesCommand {
    Get,
    Set,
}

#[derive(Args, Debug, Clone)]
pub(crate) struct ConfigAgentCustomArgs {
    #[command(subcommand)]
    pub command: ConfigAgentCustomCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum ConfigAgentCustomCommand {
    Create,
    Update,
    Delete,
    TryConnect,
}

#[derive(Args, Debug, Clone)]
pub(crate) struct ConfigCronArgs {
    #[command(subcommand)]
    pub command: ConfigCronCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum ConfigCronCommand {
    Jobs(ConfigCronJobsArgs),
    Current(ConfigCronCurrentArgs),
}

#[derive(Args, Debug, Clone)]
pub(crate) struct ConfigCronJobsArgs {
    #[command(subcommand)]
    pub command: ConfigCronJobsCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum ConfigCronJobsCommand {
    List,
    Get,
    Create,
    Update,
    Delete,
    Run,
    Skill(ConfigCronJobSkillArgs),
}

#[derive(Args, Debug, Clone)]
pub(crate) struct ConfigCronJobSkillArgs {
    #[command(subcommand)]
    pub command: ConfigCronJobSkillCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum ConfigCronJobSkillCommand {
    Get,
    Save,
    Delete,
}

#[derive(Args, Debug, Clone)]
pub(crate) struct ConfigCronCurrentArgs {
    #[command(subcommand)]
    pub command: ConfigCronCurrentCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum ConfigCronCurrentCommand {
    List,
    Create,
    Update,
}

#[derive(Args, Debug, Clone)]
pub(crate) struct ConfigConversationArgs {
    #[command(subcommand)]
    pub command: ConfigConversationCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum ConfigConversationCommand {
    Rename,
}

#[derive(Args, Debug, Clone)]
pub(crate) struct PrepareManagedResourcesArgs {
    /// 打包输出根目录。TjuaeCore 会把托管资源写入
    /// `<bundle-out>/{node,acp}/...`，供后续打包使用。
    #[arg(long)]
    pub bundle_out: PathBuf,
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use clap::error::ErrorKind;
    use clap::{CommandFactory, Parser};

    use super::{
        Cli, Command, ConfigArgs, ConfigCommand, ManagedResourcesModeArg, PrepareManagedResourcesArgs, localize_command,
    };

    #[test]
    fn help_is_localized_for_root_and_nested_commands() {
        for arguments in [vec!["tjuaecore", "--help"], vec!["tjuaecore", "config", "--help"]] {
            let error = localize_command(Cli::command())
                .try_get_matches_from(arguments)
                .expect_err("帮助参数应通过 clap 的 DisplayHelp 退出");
            assert_eq!(error.kind(), ErrorKind::DisplayHelp);

            let rendered = error.to_string();
            assert!(rendered.contains("用法："), "帮助缺少中文用法标题：{rendered}");
            assert!(rendered.contains("命令:"), "帮助缺少中文命令标题：{rendered}");
            assert!(rendered.contains("选项:"), "帮助缺少中文选项标题：{rendered}");
            assert!(rendered.contains("显示帮助"), "帮助项未汉化：{rendered}");
            assert!(!rendered.contains("Usage:"), "帮助仍包含英文用法标题：{rendered}");
            assert!(!rendered.contains("Print help"), "帮助仍包含英文帮助项：{rendered}");
        }
    }

    #[test]
    fn long_version_flag_uses_workspace_package_version() {
        let result = Cli::try_parse_from(["tjuaecore", "--version"]);
        let err = match result {
            Ok(_) => panic!("expected --version to exit through clap DisplayVersion"),
            Err(err) => err,
        };

        assert_eq!(err.kind(), ErrorKind::DisplayVersion);
        let rendered = err.to_string();
        assert!(
            rendered.contains("tjuaecore"),
            "version output should contain binary name, got: {rendered:?}"
        );
        assert!(
            rendered.contains(env!("CARGO_PKG_VERSION")),
            "version output should contain package version {}, got: {rendered:?}",
            env!("CARGO_PKG_VERSION")
        );
    }

    #[test]
    fn short_version_flag_uses_workspace_package_version() {
        let result = Cli::try_parse_from(["tjuaecore", "-V"]);
        let err = match result {
            Ok(_) => panic!("expected -V to exit through clap DisplayVersion"),
            Err(err) => err,
        };

        assert_eq!(err.kind(), ErrorKind::DisplayVersion);
        let rendered = err.to_string();
        assert!(
            rendered.contains("tjuaecore"),
            "version output should contain binary name, got: {rendered:?}"
        );
        assert!(
            rendered.contains(env!("CARGO_PKG_VERSION")),
            "version output should contain package version {}, got: {rendered:?}",
            env!("CARGO_PKG_VERSION")
        );
    }

    #[test]
    fn prepare_managed_resources_accepts_bundle_out() {
        let cli = Cli::parse_from([
            "tjuaecore",
            "prepare-managed-resources",
            "--bundle-out",
            "/tmp/tjuaecore-bundle",
        ]);

        match cli.command {
            Some(Command::PrepareManagedResources(args)) => {
                assert_eq!(args.bundle_out, std::path::Path::new("/tmp/tjuaecore-bundle"));
            }
            other => panic!("unexpected command parsed: {other:?}"),
        }
    }

    #[test]
    fn managed_resources_mode_defaults_to_download() {
        let cli = Cli::parse_from(["tjuaecore"]);
        assert_eq!(cli.managed_resources_mode, ManagedResourcesModeArg::Download);
    }

    #[test]
    fn managed_resources_mode_accepts_download() {
        let cli = Cli::parse_from(["tjuaecore", "--managed-resources-mode", "download"]);
        assert_eq!(cli.managed_resources_mode, ManagedResourcesModeArg::Download);
    }

    #[test]
    fn parent_pid_accepts_positive_integer() {
        let cli = Cli::parse_from(["tjuaecore", "--parent-pid", "4242"]);
        assert_eq!(cli.parent_pid, Some(4242));
    }

    #[test]
    fn dump_prompts_defaults_to_false() {
        let cli = Cli::parse_from(["tjuaecore"]);
        assert!(!cli.dump_prompts);
    }

    #[test]
    fn dump_prompts_accepts_flag() {
        let cli = Cli::parse_from(["tjuaecore", "--dump-prompts"]);
        assert!(cli.dump_prompts);
    }

    #[test]
    fn recover_corrupted_database_flag_defaults_to_false() {
        let cli = Cli::parse_from(["tjuaecore"]);
        assert!(!cli.recover_corrupted_database);
    }

    #[test]
    fn recover_corrupted_database_flag_is_accepted() {
        let cli = Cli::parse_from(["tjuaecore", "--recover-corrupted-database"]);
        assert!(cli.recover_corrupted_database);
    }

    #[test]
    fn command_as_str_returns_clap_subcommand_names() {
        let prepare_args = PrepareManagedResourcesArgs {
            bundle_out: PathBuf::from("/tmp/tjuaecore-bundle"),
        };

        let cases = [
            (
                Command::Config(ConfigArgs {
                    command: ConfigCommand::Context,
                }),
                "config",
            ),
            (Command::McpBridge, "mcp-bridge"),
            (Command::McpTeamStdio, "mcp-team-stdio"),
            (Command::Doctor, "doctor"),
            (
                Command::PrepareManagedResources(prepare_args),
                "prepare-managed-resources",
            ),
        ];

        for (command, expected) in cases {
            assert_eq!(command.as_str(), expected);
        }
    }

    #[test]
    fn config_cli_accepts_agent_facing_design_command_paths() {
        let commands: &[&[&str]] = &[
            &["tjuaecore", "config", "capabilities"],
            &["tjuaecore", "config", "context"],
            &["tjuaecore", "config", "conversation", "rename"],
            &["tjuaecore", "config", "assistants", "list"],
            &["tjuaecore", "config", "assistants", "get"],
            &["tjuaecore", "config", "assistants", "create"],
            &["tjuaecore", "config", "assistants", "settings"],
            &["tjuaecore", "config", "assistants", "delete"],
            &["tjuaecore", "config", "assistants", "copy"],
            &["tjuaecore", "config", "assistants", "preferences"],
            &["tjuaecore", "config", "assistants", "file", "read"],
            &["tjuaecore", "config", "assistants", "file", "write"],
            &["tjuaecore", "config", "assistants", "prepare"],
            &["tjuaecore", "config", "assistants", "activate"],
            &["tjuaecore", "config", "assistants", "export"],
            &["tjuaecore", "config", "assistants", "publish"],
            &["tjuaecore", "config", "mcp", "servers", "list"],
            &["tjuaecore", "config", "mcp", "servers", "get"],
            &["tjuaecore", "config", "mcp", "servers", "create"],
            &["tjuaecore", "config", "mcp", "servers", "update"],
            &["tjuaecore", "config", "mcp", "servers", "delete"],
            &["tjuaecore", "config", "mcp", "servers", "toggle"],
            &["tjuaecore", "config", "mcp", "servers", "import"],
            &["tjuaecore", "config", "mcp", "test-connection"],
            &["tjuaecore", "config", "mcp", "agent-configs"],
            &["tjuaecore", "config", "mcp", "oauth", "check-status"],
            &["tjuaecore", "config", "mcp", "oauth", "login"],
            &["tjuaecore", "config", "mcp", "oauth", "logout"],
            &["tjuaecore", "config", "mcp", "oauth", "authenticated"],
            &["tjuaecore", "config", "providers", "list"],
            &["tjuaecore", "config", "providers", "create"],
            &["tjuaecore", "config", "providers", "update"],
            &["tjuaecore", "config", "providers", "delete"],
            &["tjuaecore", "config", "providers", "detect-protocol"],
            &["tjuaecore", "config", "providers", "fetch-models"],
            &["tjuaecore", "config", "providers", "models", "fetch"],
            &["tjuaecore", "config", "providers", "health-check"],
            &["tjuaecore", "config", "settings", "get"],
            &["tjuaecore", "config", "settings", "patch"],
            &["tjuaecore", "config", "settings", "client", "get"],
            &["tjuaecore", "config", "settings", "client", "put"],
            &["tjuaecore", "config", "agents", "list"],
            &["tjuaecore", "config", "agents", "enable"],
            &["tjuaecore", "config", "agents", "overrides", "get"],
            &["tjuaecore", "config", "agents", "overrides", "set"],
            &["tjuaecore", "config", "agents", "custom", "create"],
            &["tjuaecore", "config", "agents", "custom", "update"],
            &["tjuaecore", "config", "agents", "custom", "delete"],
            &["tjuaecore", "config", "agents", "custom", "try-connect"],
            &["tjuaecore", "config", "cron", "jobs", "list"],
            &["tjuaecore", "config", "cron", "jobs", "get"],
            &["tjuaecore", "config", "cron", "jobs", "create"],
            &["tjuaecore", "config", "cron", "jobs", "update"],
            &["tjuaecore", "config", "cron", "jobs", "delete"],
            &["tjuaecore", "config", "cron", "jobs", "run"],
            &["tjuaecore", "config", "cron", "jobs", "skill", "get"],
            &["tjuaecore", "config", "cron", "jobs", "skill", "save"],
            &["tjuaecore", "config", "cron", "jobs", "skill", "delete"],
            &["tjuaecore", "config", "skills", "list"],
            &["tjuaecore", "config", "skills", "create"],
            &["tjuaecore", "config", "skills", "import"],
            &["tjuaecore", "config", "skills", "delete"],
            &["tjuaecore", "config", "skills", "copy"],
            &["tjuaecore", "config", "skills", "preferences"],
        ];

        for command in commands {
            let result = Cli::try_parse_from(*command);
            assert!(result.is_ok(), "command should parse: {command:?}");
        }
    }

    #[test]
    fn team_cli_accepts_agent_facing_command_paths() {
        let commands: &[&[&str]] = &[
            &["tjuaecore", "team", "capabilities"],
            &["tjuaecore", "team", "help"],
            &["tjuaecore", "team", "context"],
            &["tjuaecore", "team", "members"],
            &["tjuaecore", "team", "send-message"],
            &["tjuaecore", "team", "task", "create"],
            &["tjuaecore", "team", "task", "update"],
            &["tjuaecore", "team", "task", "list"],
            &["tjuaecore", "team", "list-assistants"],
            &["tjuaecore", "team", "describe-assistant"],
            &["tjuaecore", "team", "spawn-agent"],
            &["tjuaecore", "team", "rename-agent"],
            &["tjuaecore", "team", "shutdown-agent"],
        ];

        for command in commands {
            let result = Cli::try_parse_from(*command);
            assert!(result.is_ok(), "command should parse: {command:?}");
        }
    }

    #[test]
    fn prepare_managed_resources_requires_bundle_out() {
        let err = match Cli::try_parse_from(["tjuaecore", "prepare-managed-resources"]) {
            Ok(_) => panic!("prepare-managed-resources should require --bundle-out"),
            Err(err) => err,
        };
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
    }
}
