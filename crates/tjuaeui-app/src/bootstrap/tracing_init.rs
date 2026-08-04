//! Tracing subscriber + log file initialization for the binary.
//!
//! Lives in the binary tree (not lib) because it owns process-global
//! subscriber registration that should never be invoked from tests or
//! external consumers of the library.

use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

use chrono::Datelike;
use tracing_subscriber::{EnvFilter, Layer, fmt, layer::SubscriberExt, util::SubscriberInitExt};

use super::{BootstrapError, BootstrapErrorCode};

const NOISE_SUPPRESSIONS: &[&str] = &[
    "sqlx::query=warn",
    "hyper_util=warn",
    "reqwest=warn",
    // The ACP SDK logs raw UntypedMessage values at debug/trace, including
    // session/update chunks with user/agent text. Keep its protocol internals
    // out of default dev logs; tjuaeui_ai_agent::protocol::acp emits sanitized
    // summaries for the ACP flow we need to debug.
    "agent_client_protocol::jsonrpc=info",
    // TjuaeCli provider/agent debug logs include raw request bodies and SSE
    // chunks. Keep lifecycle info logs, but do not write prompt/output
    // payloads by default.
    "tjuae_agent=info",
    "tjuae_providers=info",
];

const TJUAE_CLI_TARGETS: &[&str] = &[
    "tjuae_agent",
    "tjuae_config",
    "tjuae_compact",
    "tjuae_mcp",
    "tjuae_providers",
    "tjuae_protocol",
    "tjuae_tools",
    "tjuae_skills",
    "tjuae_memory",
];

const RAW_TJUAE_CLI_PAYLOAD_TARGETS: &[&str] = &["tjuae_agent", "tjuae_providers"];

fn build_env_filter(log_level: Option<&str>) -> EnvFilter {
    let user_directives = log_level.unwrap_or("info");
    let suppressions = NOISE_SUPPRESSIONS.join(",");
    EnvFilter::new(format!("{suppressions},{user_directives}"))
}

fn build_backend_filter(log_level: Option<&str>) -> EnvFilter {
    let user_directives = log_level.unwrap_or("info");
    let suppressions = NOISE_SUPPRESSIONS.join(",");
    let tjuae_cli_off: String = TJUAE_CLI_TARGETS
        .iter()
        .map(|t| format!("{t}=off"))
        .collect::<Vec<_>>()
        .join(",");
    EnvFilter::new(format!("{suppressions},{tjuae_cli_off},{user_directives}"))
}

fn build_tjuae_cli_level(log_level: Option<&str>) -> String {
    let level = log_level.unwrap_or("info");
    TJUAE_CLI_TARGETS
        .iter()
        .map(|target| {
            let target_level = if RAW_TJUAE_CLI_PAYLOAD_TARGETS.contains(target) {
                "info"
            } else {
                level
            };
            format!("{target}={target_level}")
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// RAII guards that flush log buffers on drop. Hold for the process lifetime.
pub struct LogGuards {
    _backend: tracing_appender::non_blocking::WorkerGuard,
    _tjuae_cli: tracing_appender::non_blocking::WorkerGuard,
}

const LOGGING_INIT_MESSAGE: &str = "failed to initialize logging";

pub fn init_tracing(log_dir: &Path, log_level: Option<&str>) -> Result<LogGuards, BootstrapError> {
    let active_log_dir = dated_log_dir(log_dir);

    std::fs::create_dir_all(&active_log_dir).map_err(|e| {
        BootstrapError::new(
            BootstrapErrorCode::LoggingInitFailed,
            "logging.dir",
            LOGGING_INIT_MESSAGE,
        )
        .with_source(e)
        .with_field("logDir", active_log_dir.display().to_string())
    })?;

    let console_layer = fmt::layer().with_target(true).with_filter(build_env_filter(log_level));

    // Backend file layer — excludes tjuae_* targets
    let file_appender = DailyDatedLogWriter::new(log_dir.to_path_buf(), "tjuaecore.log");
    let (non_blocking, backend_guard) = tracing_appender::non_blocking(file_appender);

    let backend_file_layer = fmt::layer()
        .json()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_target(true)
        .with_filter(build_backend_filter(log_level));

    // TjuaeCli file layer — only tjuae_* targets
    let tjuae_cli_level = build_tjuae_cli_level(log_level);
    let tjuae_cli_filter = EnvFilter::try_new(&tjuae_cli_level).map_err(|e| {
        BootstrapError::new(
            BootstrapErrorCode::LoggingInitFailed,
            "logging.filter",
            LOGGING_INIT_MESSAGE,
        )
        .with_source(e)
        .with_field("filter", tjuae_cli_level.clone())
        .with_field("logDir", active_log_dir.display().to_string())
    })?;
    let tjuae_cli_appender = DailyDatedLogWriter::new(log_dir.to_path_buf(), "tjuae-cli.log");
    let (tjuae_cli_non_blocking, tjuae_cli_guard) = tracing_appender::non_blocking(tjuae_cli_appender);
    let tjuae_cli_layer = fmt::layer()
        .json()
        .with_writer(tjuae_cli_non_blocking)
        .with_ansi(false)
        .with_target(true)
        .with_filter(tjuae_cli_filter);

    tracing_subscriber::registry()
        .with(console_layer)
        .with(backend_file_layer)
        .with(tjuae_cli_layer)
        .try_init()
        .map_err(|e| {
            BootstrapError::new(
                BootstrapErrorCode::LoggingInitFailed,
                "logging.subscriber",
                LOGGING_INIT_MESSAGE,
            )
            .with_source(e)
            .with_field("logDir", active_log_dir.display().to_string())
        })?;

    Ok(LogGuards {
        _backend: backend_guard,
        _tjuae_cli: tjuae_cli_guard,
    })
}

fn dated_log_dir(log_root: &Path) -> PathBuf {
    dated_log_dir_for(log_root, LogDate::today())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LogDate {
    year: i32,
    month: u32,
    day: u32,
}

impl LogDate {
    fn today() -> Self {
        let now = chrono::Local::now();
        Self {
            year: now.year(),
            month: now.month(),
            day: now.day(),
        }
    }

    fn file_name(self, suffix: &str) -> String {
        format!("{:04}-{:02}-{:02}.{}", self.year, self.month, self.day, suffix)
    }
}

fn dated_log_dir_for(log_root: &Path, date: LogDate) -> PathBuf {
    log_root
        .join(format!("{:04}", date.year))
        .join(format!("{:02}", date.month))
        .join(format!("{:02}", date.day))
}

fn dated_log_file_path(log_root: &Path, date: LogDate, suffix: &str) -> PathBuf {
    dated_log_dir_for(log_root, date).join(date.file_name(suffix))
}

struct DailyDatedLogWriter {
    log_root: PathBuf,
    filename_suffix: &'static str,
    date_provider: Box<dyn Fn() -> LogDate + Send + Sync>,
    active_date: Option<LogDate>,
    active_file: Option<File>,
}

impl DailyDatedLogWriter {
    fn new(log_root: PathBuf, filename_suffix: &'static str) -> Self {
        Self::new_with_date_provider(log_root, filename_suffix, Box::new(LogDate::today))
    }

    fn new_with_date_provider(
        log_root: PathBuf,
        filename_suffix: &'static str,
        date_provider: Box<dyn Fn() -> LogDate + Send + Sync>,
    ) -> Self {
        Self {
            log_root,
            filename_suffix,
            date_provider,
            active_date: None,
            active_file: None,
        }
    }

    fn active_file(&mut self) -> io::Result<&mut File> {
        let date = (self.date_provider)();
        if self.active_date != Some(date) {
            let file_path = dated_log_file_path(&self.log_root, date, self.filename_suffix);
            if let Some(parent) = file_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            self.active_file = Some(OpenOptions::new().create(true).append(true).open(file_path)?);
            self.active_date = Some(date);
        }

        self.active_file
            .as_mut()
            .ok_or_else(|| io::Error::other("log file was not opened"))
    }
}

impl Write for DailyDatedLogWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.active_file()?.write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(file) = self.active_file.as_mut() {
            file.flush()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing::Level;

    #[test]
    fn env_filter_suppresses_raw_acp_sdk_jsonrpc_debug_even_when_debug_enabled() {
        let subscriber = tracing_subscriber::registry().with(build_env_filter(Some("debug")));
        tracing::subscriber::with_default(subscriber, || {
            assert!(
                !tracing::enabled!(target: "agent_client_protocol::jsonrpc::handlers", Level::DEBUG),
                "ACP SDK JSON-RPC debug logs include raw UntypedMessage payloads"
            );
            assert!(
                tracing::enabled!(target: "tjuaeui_ai_agent::protocol::acp", Level::DEBUG),
                "TjuaeUI ACP sanitized debug summaries should still be available"
            );
        });
    }

    #[test]
    fn backend_filter_suppresses_raw_acp_sdk_jsonrpc_debug_even_when_debug_enabled() {
        let subscriber = tracing_subscriber::registry().with(build_backend_filter(Some("debug")));
        tracing::subscriber::with_default(subscriber, || {
            assert!(
                !tracing::enabled!(target: "agent_client_protocol::jsonrpc::handlers", Level::DEBUG),
                "ACP SDK JSON-RPC debug logs include raw UntypedMessage payloads"
            );
            assert!(
                tracing::enabled!(target: "tjuaeui_ai_agent::protocol::acp", Level::DEBUG),
                "TjuaeUI ACP sanitized debug summaries should still be available"
            );
        });
    }

    #[test]
    fn env_filter_suppresses_raw_tjuae_cli_provider_debug_even_when_debug_enabled() {
        let subscriber = tracing_subscriber::registry().with(build_env_filter(Some("debug")));
        tracing::subscriber::with_default(subscriber, || {
            assert!(
                !tracing::enabled!(target: "tjuae_agent", Level::DEBUG),
                "tjuae_agent debug logs include raw request bodies"
            );
            assert!(
                !tracing::enabled!(target: "tjuae_providers", Level::DEBUG),
                "tjuae_providers debug logs include raw SSE chunks"
            );
            assert!(
                tracing::enabled!(target: "tjuaeui_ai_agent::manager::tjuae_cli::agent", Level::DEBUG),
                "TjuaeUI TjuaeCLI lifecycle debug logs should still be available"
            );
        });
    }

    #[test]
    fn tjuae_cli_file_level_suppresses_raw_provider_targets_even_when_debug_enabled() {
        let level = build_tjuae_cli_level(Some("debug"));
        assert!(level.contains("tjuae_agent=info"), "{level}");
        assert!(level.contains("tjuae_providers=info"), "{level}");
        assert!(level.contains("tjuae_tools=debug"), "{level}");
    }

    #[test]
    fn dated_log_dir_appends_date_partition() {
        let root = Path::new("/tmp/tjuaeui-logs");
        let dated = dated_log_dir(root);
        let relative = dated.strip_prefix(root).expect("dated log dir should stay under root");
        let parts = relative
            .iter()
            .map(|part| part.to_str().expect("log dir should be utf-8"))
            .collect::<Vec<_>>();

        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0].len(), 4);
        assert_eq!(parts[1].len(), 2);
        assert_eq!(parts[2].len(), 2);
        assert!(parts[0].chars().all(|ch| ch.is_ascii_digit()));
        assert!(parts[1].chars().all(|ch| ch.is_ascii_digit()));
        assert!(parts[2].chars().all(|ch| ch.is_ascii_digit()));
    }

    #[test]
    fn dated_file_writer_moves_new_day_files_into_matching_day_directory() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let first_day = LogDate {
            year: 2026,
            month: 7,
            day: 2,
        };
        let second_day = LogDate {
            year: 2026,
            month: 7,
            day: 3,
        };
        let days = std::sync::Arc::new(std::sync::Mutex::new(vec![second_day, first_day]));
        let mut writer = DailyDatedLogWriter::new_with_date_provider(
            tmp.path().to_path_buf(),
            "tjuaecore.log",
            Box::new({
                let days = std::sync::Arc::clone(&days);
                move || days.lock().expect("date queue").pop().expect("date")
            }),
        );

        std::io::Write::write_all(&mut writer, b"july 2\n").expect("write first day");
        std::io::Write::write_all(&mut writer, b"july 3\n").expect("write second day");
        std::io::Write::flush(&mut writer).expect("flush");

        let first_path = tmp.path().join("2026/07/02/2026-07-02.tjuaecore.log");
        let second_path = tmp.path().join("2026/07/03/2026-07-03.tjuaecore.log");
        assert_eq!(std::fs::read_to_string(first_path).expect("first day log"), "july 2\n");
        assert_eq!(
            std::fs::read_to_string(second_path).expect("second day log"),
            "july 3\n"
        );
        assert!(!tmp.path().join("2026/07/02/2026-07-03.tjuaecore.log").exists());
    }
}
