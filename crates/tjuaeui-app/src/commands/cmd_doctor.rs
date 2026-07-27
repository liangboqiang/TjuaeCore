//! `tjuaecore doctor` subcommand: agent CLI detection self-check.
//!
//! Hydrates the agent registry against the real on-disk database and
//! prints a per-agent availability table to stdout. Mirrors the
//! server's PATH probing path exactly — `main` runs the same
//! `tjuaeui_runtime::init` + `enhance_process_path` for `Doctor` as it
//! does for the server, so managed runtimes and CLI commands resolve
//! through the same paths the server uses.
//!
//! Writes to stdout (not the rolling tjuaecore.log) — the user
//! typically runs `doctor` interactively after reporting "no agent
//! works", and the answer needs to be visible in their terminal
//! without grepping logs. We deliberately skip `init_environment` to
//! avoid installing a tracing subscriber that would redirect
//! diagnostic output to a log file, and skip `init_data_layer` to
//! avoid materializing the builtin-skills tree as a side effect of a
//! read-only diagnostic run.

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use tjuaeui_ai_agent::{AgentRegistry, UnavailableReason};
use tjuaeui_db::{IAgentMetadataRepository, SqliteAgentMetadataRepository, init_database};
use tjuaeui_runtime::doctor_snapshot;

use crate::cli::Cli;
use crate::commands::error::{CliBoundaryCode, CliBoundaryError};

const SUBCOMMAND: &str = "doctor";

pub async fn run_doctor(cli: &Cli, merged_path: &str) -> Result<ExitCode, CliBoundaryError> {
    print_environment(merged_path, &cli.data_dir);

    // Use the real on-disk DB so the report reflects the user's actual
    // catalog (including custom agents they've added via the UI).
    let db_path = cli.data_dir.join("tjuaeui-backend.db");
    let database = init_database(&db_path).await.map_err(|_| doctor_database_error())?;

    let repo: Arc<dyn IAgentMetadataRepository> = Arc::new(SqliteAgentMetadataRepository::new(database.pool().clone()));
    let registry = AgentRegistry::new(repo);
    registry.hydrate().await.map_err(|_| doctor_registry_hydrate_error())?;

    let snapshot = registry.diagnostic_snapshot().await;
    print_snapshot(&snapshot);

    database.close().await;

    Ok(ExitCode::SUCCESS)
}

fn doctor_database_error() -> CliBoundaryError {
    CliBoundaryError::new(
        CliBoundaryCode::CliDoctorDatabaseFailed,
        SUBCOMMAND,
        "doctor failed to open the application database",
    )
}

fn doctor_registry_hydrate_error() -> CliBoundaryError {
    CliBoundaryError::new(
        CliBoundaryCode::CliDoctorRegistryHydrateFailed,
        SUBCOMMAND,
        "doctor failed to hydrate the agent registry",
    )
}

fn print_environment(merged_path: &str, data_dir: &Path) {
    let path_segments = merged_path.split(if cfg!(windows) { ';' } else { ':' }).count();
    println!("TjuaeUI 后端诊断——智能体 CLI 检测自检");
    println!("  data-dir       : {}", data_dir.display());
    println!("  PATH segments  : {path_segments}");
    println!("  PATH length    : {}", merged_path.len());
    for line in runtime_snapshot_lines() {
        println!("{line}");
    }
    println!();
}

fn runtime_snapshot_lines() -> Vec<String> {
    let node_rows = doctor_snapshot();
    if node_rows.is_empty() {
        return Vec::new();
    }

    let mut lines = Vec::new();
    lines.push("  node runtime   :".to_owned());
    lines.extend(
        node_rows
            .into_iter()
            .map(|row| format!("    {:<16} {:<10} {}", row.tool, row.source, row.detail)),
    );
    lines
}

fn print_snapshot(snapshot: &[(tjuaeui_api_types::AgentMetadata, Option<UnavailableReason>)]) {
    let total = snapshot.len();
    let available = snapshot.iter().filter(|(m, _)| m.available).count();
    let unavailable = total - available;

    println!("目录中的智能体：{total}  可用：{available}  不可用：{unavailable}");
    println!();
    println!(
        "{:<32} {:<10} {:<14} {:<10} REASON / RESOLVED",
        "ID", "后端", "来源", "状态"
    );
    println!("{}", "-".repeat(110));

    for (meta, reason) in snapshot {
        let backend = meta.backend.as_deref().unwrap_or("-");
        let source = format!("{:?}", meta.agent_source);
        let status = if meta.available { "available" } else { "missing" };
        let trailer = match (meta.available, reason) {
            (true, _) => meta
                .resolved_command
                .as_deref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "<internal>".to_owned()),
            (false, Some(r)) => describe_reason(r),
            (false, None) => "unavailable (no reason recorded — registry bug)".to_owned(),
        };
        println!(
            "{:<32} {:<10} {:<14} {:<10} {}",
            meta.id, backend, source, status, trailer
        );
    }

    if unavailable > 0 {
        println!();
        println!("提示：标记为 `missing` 的条目无法从当前终端解析或运行其 CLI。");
        println!("     If a CLI is installed but missing here, the Electron app may inherit a different PATH —");
        println!("     reproduce by launching the app from this same shell or check launchctl/setenv setup.");
    }
}

fn describe_reason(reason: &UnavailableReason) -> String {
    match reason {
        UnavailableReason::Disabled => "disabled by user".to_owned(),
        UnavailableReason::NoCommand => "no spawn command configured (seed data bug)".to_owned(),
        UnavailableReason::BridgeMissing { bridge } => format!("bridge `{bridge}` not on $PATH"),
        UnavailableReason::PrimaryMissing { binary } => format!("$PATH 中找不到 CLI `{binary}`"),
        UnavailableReason::PrimaryUnusable { binary, detail } => {
            format!("CLI `{binary}` 无法运行：{detail}")
        }
        UnavailableReason::CommandMissing { command } => format!("`{command}` not on $PATH"),
        UnavailableReason::ManagedRuntimeUnavailable { resource, detail } => {
            format!("managed `{resource}` unavailable: {detail}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_snapshot_lines_have_header_and_rows() {
        let lines = runtime_snapshot_lines();
        if lines.is_empty() {
            return;
        }

        assert_eq!(lines[0], "  node runtime   :");
        assert!(lines.iter().skip(1).any(|line| line.contains("node")));
    }

    #[test]
    fn doctor_database_error_uses_stable_code_without_raw_path() {
        let err = doctor_database_error();

        assert_eq!(err.code(), CliBoundaryCode::CliDoctorDatabaseFailed);
        assert!(
            err.stderr_line()
                .starts_with("CLI_DOCTOR_DATABASE_FAILED subcommand=doctor")
        );
        assert!(!err.stderr_line().contains("/Users/secret/tjuaeui-backend.db"));
    }

    #[test]
    fn doctor_registry_error_uses_stable_code() {
        let err = doctor_registry_hydrate_error();

        assert_eq!(err.code(), CliBoundaryCode::CliDoctorRegistryHydrateFailed);
        assert!(
            err.stderr_line()
                .starts_with("CLI_DOCTOR_REGISTRY_HYDRATE_FAILED subcommand=doctor")
        );
    }
}
