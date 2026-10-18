mod api;
mod config;
mod control;
mod cpu;
mod db;
mod drives;
mod hwmon;
mod paths;

use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::{Mutex, RwLock};

use crate::config::Config;
use crate::control::{AppState, Status, WearSnapshot};
use crate::cpu::CpuControl;
use crate::db::Db;
use crate::hwmon::Hardware;
use crate::paths::Paths;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "fancurve=info".into()),
        )
        .init();

    let paths = Paths::from_env();
    let config_path = Config::path();
    let snapshot_path = config_path.with_file_name("chip-snapshot.json");
    let hw = Hardware::discover(&paths, &snapshot_path)?;

    let config = Config::load_or_default(&config_path)?;
    tracing::info!("config loaded from {}", config_path.display());

    let cpu = CpuControl::discover(&paths);
    // Re-apply the saved profile: the BIOS/kernel may have reset the knobs.
    if let Some(profile) = config.cpu.profile {
        match cpu.apply(profile) {
            Ok(warnings) => {
                tracing::info!("cpu profile {profile:?} applied at start-up");
                for warning in warnings {
                    tracing::warn!("cpu: {warning}");
                }
            }
            Err(e) => tracing::error!("applying cpu profile {profile:?}: {e:#}"),
        }
    }

    let drives = drives::discover(&paths);
    if drives.is_empty() {
        tracing::warn!(
            "no NVMe controllers under {}: drive wear monitoring unavailable",
            paths.nvme.display()
        );
    } else {
        tracing::info!(
            "nvme drives: {}",
            drives
                .iter()
                .map(|d| d.device.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let db = Db::open_env();

    let state = Arc::new(AppState {
        hw,
        config: RwLock::new(config),
        status: RwLock::new(Status::default()),
        paths,
        cpu,
        drives,
        wear: RwLock::new(WearSnapshot::default()),
        db,
        sample_lock: Mutex::new(()),
    });

    // Control loop.
    tokio::spawn(control::run(state.clone()));
    // NVMe wear sampler (initial sample + periodic sampling + pruning).
    tokio::spawn(control::run_wear_sampler(state.clone()));

    // HTTP server.
    let listen = std::env::var("FANCURVE_LISTEN").unwrap_or_else(|_| "0.0.0.0:8090".into());
    let listener = tokio::net::TcpListener::bind(&listen)
        .await
        .with_context(|| format!("binding {listen}"))?;
    tracing::info!("listening on http://{listen}");

    axum::serve(listener, api::router(state.clone()))
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    // On exit, hand the fans back to the hardware's automatic control.
    state.hw.restore_auto();
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.ok();
    };
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler")
            .recv()
            .await;
    };
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown signal received, restoring automatic fan control");
}
