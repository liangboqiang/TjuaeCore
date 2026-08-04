//! Bootstrap layers shared by non-MCP subcommands.

use std::time::Instant;

use tracing::info;

use tjuaeui_app::AppConfig;
use tjuaeui_db::Database;

use crate::cli::Cli;

use super::tracing_init::{LogGuards, init_tracing};
use super::work_dir::resolve_work_dir;
use super::{BootstrapError, BootstrapErrorCode};

/// Resolved environment needed by all non-MCP subcommands.
pub struct ServerEnvironment {
    /// Must be held alive for the process lifetime to flush log buffers.
    pub _log_guard: LogGuards,
    pub config: AppConfig,
}

/// Layer 1: Logging + config resolution.
///
/// Cheap, synchronous, no IO beyond creating the log directory.
/// All subcommands that need logging and config should call this first.
pub fn init_environment(cli: &Cli, merged_path: &str) -> Result<ServerEnvironment, BootstrapError> {
    let log_dir = cli.log_dir.clone().unwrap_or_else(|| cli.data_dir.join("logs"));
    let log_guard = init_tracing(&log_dir, cli.log_level.as_deref())?;

    info!(
        path_segments = merged_path.split(if cfg!(windows) { ';' } else { ':' }).count(),
        path_len = merged_path.len(),
        "startup: PATH ready"
    );

    let work_dir = resolve_work_dir(cli.work_dir.clone(), &cli.data_dir);

    // SAFETY: called before any service initialization; no concurrent reads.
    unsafe {
        std::env::set_var("TJUAE_WORK_DIR", &work_dir);
    }

    let config = AppConfig {
        host: cli.host.clone(),
        port: cli.port,
        data_dir: cli.data_dir.clone(),
        work_dir,
        app_version: cli.app_version.clone(),
        local: cli.local,
        dump_prompts: cli.dump_prompts,
        recover_corrupted_database: cli.recover_corrupted_database,
    };
    info!(
        "Running in {} mode — authentication is {}",
        if config.local { "local" } else { "remote" },
        if config.local { "disabled" } else { "enabled" }
    );

    Ok(ServerEnvironment {
        _log_guard: log_guard,
        config,
    })
}

/// Layer 2: Initialize the database.
///
/// Requires only `data_dir`. Subcommands that need persistent state
/// (database, skill files) should call this after `init_environment`.
pub async fn init_data_layer(config: &AppConfig) -> Result<Database, BootstrapError> {
    let boot = Instant::now();

    let db_path = config.database_path();
    info!("Initializing database at {}", db_path.display());
    let database = tjuaeui_db::init_database_staged_with_options(
        &db_path,
        tjuaeui_db::DatabaseInitOptions {
            recover_corrupted_database: config.recover_corrupted_database,
        },
    )
    .await
    .map_err(|e| {
        let stage = e.stage();
        BootstrapError::new(
            BootstrapErrorCode::DataInitFailed,
            stage,
            "failed to initialize application data",
        )
        .with_source(e.into_source())
        .with_field("databasePath", db_path.display().to_string())
    })?;
    info!(elapsed_ms = boot.elapsed().as_millis(), "startup: database initialized");

    Ok(database)
}

#[cfg(test)]
mod tests {
    #[test]
    fn database_stage_comes_from_db_boundary_error() {
        let err = tjuaeui_db::DatabaseInitError::new(
            "database.migration",
            tjuaeui_db::DbError::Migration(sqlx::migrate::MigrateError::VersionMismatch(42)),
        );

        assert_eq!(err.stage(), "database.migration");
    }

    #[test]
    fn database_schema_repair_stage_comes_from_db_boundary_error() {
        let err = tjuaeui_db::DatabaseInitError::new(
            "database.schema_repair",
            tjuaeui_db::DbError::Init("repair failed".into()),
        );

        assert_eq!(err.stage(), "database.schema_repair");
    }

    #[test]
    fn database_recoverable_corruption_stage_comes_from_db_boundary_error() {
        let err = tjuaeui_db::DatabaseInitError::new(
            "database.recoverable_corruption",
            tjuaeui_db::DbError::Migration(sqlx::migrate::MigrateError::ExecuteMigration(
                sqlx::Error::Protocol("database disk image is malformed".into()),
                13,
            )),
        );

        assert_eq!(err.stage(), "database.recoverable_corruption");
    }
}
