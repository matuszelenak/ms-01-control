//! HTTP API and static frontend serving.

use axum::extract::{Query, Request, State};
use axum::http::{header, HeaderValue, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};

use crate::config::{Config, CpuProfile};
use crate::control::{self, SharedState};
use crate::cpu::CpuState;

#[derive(RustEmbed)]
#[folder = "../frontend/dist"]
struct Assets;

pub fn router(state: SharedState) -> Router {
    Router::new()
        .route("/api/status", get(get_status))
        .route("/api/config", get(get_config).put(put_config))
        .route("/api/cpu", put(put_cpu))
        .route("/api/drives/refresh", post(refresh_drives))
        .route("/api/history", get(get_history))
        .layer(middleware::from_fn(no_store_api))
        .fallback(static_handler)
        .with_state(state)
}

/// Never let a browser cache API responses: the UI polls them every couple of
/// seconds and stale values would be misleading (a headless client replaying a
/// cached `/api/status` made an applied CPU profile look unapplied).
async fn no_store_api(request: Request, next: Next) -> Response {
    let is_api = request.uri().path().starts_with("/api/");
    let mut response = next.run(request).await;
    if is_api {
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    response
}

async fn get_status(State(state): State<SharedState>) -> Response {
    Json(state.status.read().await.clone()).into_response()
}

async fn get_config(State(state): State<SharedState>) -> Response {
    Json(state.config.read().await.clone()).into_response()
}

async fn put_config(State(state): State<SharedState>, Json(new_config): Json<Config>) -> Response {
    if let Err(e) = new_config.validate() {
        return (StatusCode::UNPROCESSABLE_ENTITY, format!("{e:#}")).into_response();
    }
    let path = Config::path();
    if let Err(e) = new_config.save(&path) {
        tracing::error!("saving config: {e:#}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to save config: {e:#}"),
        )
            .into_response();
    }
    *state.config.write().await = new_config.clone();
    tracing::info!("config updated and saved to {}", path.display());
    Json(new_config).into_response()
}

#[derive(Debug, Deserialize)]
struct CpuUpdate {
    /// `null` stops managing the CPU (the current settings are left as-is).
    profile: Option<CpuProfile>,
}

#[derive(Debug, Serialize)]
struct CpuUpdateResponse {
    cpu: CpuState,
    /// Settings that could not be applied (unsupported knob, permission, ...).
    warnings: Vec<String>,
}

/// Apply a CPU power profile immediately and remember it in the config.
///
/// This deliberately bypasses the "Save & Apply" flow of the fan settings:
/// switching profiles is an interactive action, and the UI shows the result
/// immediately.
async fn put_cpu(State(state): State<SharedState>, Json(update): Json<CpuUpdate>) -> Response {
    let mut warnings = Vec::new();

    if let Some(profile) = update.profile {
        if !state.cpu.supports_profile(profile) {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                format!(
                    "this CPU does not support the '{}' profile",
                    profile_name(profile)
                ),
            )
                .into_response();
        }
        match state.cpu.apply(profile) {
            Ok(w) => warnings = w,
            Err(e) => {
                tracing::error!("applying cpu profile {:?}: {e:#}", profile);
                return (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response();
            }
        }
    }

    // Persist the choice so it is re-applied after a reboot.
    let mut config = state.config.read().await.clone();
    config.cpu.profile = update.profile;
    let path = Config::path();
    if let Err(e) = config.save(&path) {
        tracing::error!("saving config: {e:#}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("profile applied but saving it failed: {e:#}"),
        )
            .into_response();
    }
    *state.config.write().await = config.clone();

    // Refresh the cached status right away: /api/status is otherwise only
    // rebuilt on the next control-loop tick, and a client polling immediately
    // after the switch would still see the previous profile.
    let cpu = state.cpu.state(config.cpu.profile);
    state.status.write().await.cpu = cpu.clone();

    Json(CpuUpdateResponse { cpu, warnings }).into_response()
}

fn profile_name(profile: CpuProfile) -> &'static str {
    match profile {
        CpuProfile::Performance => "performance",
        CpuProfile::Balanced => "balanced",
        CpuProfile::Efficiency => "efficiency",
    }
}

/// Read SMART data right now (also appends to the history database).
async fn refresh_drives(State(state): State<SharedState>) -> Response {
    control::sample_wear(&state).await;
    Json(control::drive_statuses(&state).await).into_response()
}

#[derive(Debug, Deserialize)]
struct HistoryQuery {
    /// Window length in days (1-3650).
    #[serde(default = "default_history_days")]
    days: u32,
    /// Optional device filter (e.g. `nvme1`).
    device: Option<String>,
    /// Points to keep per drive after thinning.
    #[serde(default = "default_history_points")]
    max_points: usize,
}

fn default_history_days() -> u32 {
    90
}

fn default_history_points() -> usize {
    400
}

#[derive(Debug, Serialize)]
struct HistorySeries {
    device: String,
    points: Vec<crate::db::WearSample>,
}

#[derive(Debug, Serialize)]
struct HistoryResponse {
    days: u32,
    /// Unix seconds of the start of the window.
    since: i64,
    series: Vec<HistorySeries>,
}

async fn get_history(
    State(state): State<SharedState>,
    Query(query): Query<HistoryQuery>,
) -> Response {
    let Some(db) = state.db.clone() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "wear history is disabled (no database configured)",
        )
            .into_response();
    };
    let days = query.days.clamp(1, 3650);
    let max_points = query.max_points.clamp(2, 5_000);
    let since = control::now_secs() - days as i64 * 86_400;

    // SQLite access is blocking; keep it off the async runtime threads.
    let filter = query.device.clone();
    let rows = match tokio::task::spawn_blocking(move || db.history(since, max_points)).await {
        Ok(Ok(rows)) => rows,
        Ok(Err(e)) => {
            tracing::error!("reading wear history: {e:#}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to read history: {e:#}"),
            )
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("history query failed: {e}"),
            )
                .into_response();
        }
    };

    // Group per device, preserving the device ordering from the database.
    let mut series: Vec<HistorySeries> = Vec::new();
    for row in rows {
        if filter.as_deref().is_some_and(|d| d != row.device) {
            continue;
        }
        match series.last_mut() {
            Some(last) if last.device == row.device => last.points.push(row),
            _ => series.push(HistorySeries { device: row.device.clone(), points: vec![row] }),
        }
    }

    Json(HistoryResponse { days, since, series }).into_response()
}

async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    match Assets::get(path) {
        Some(file) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            ([(header::CONTENT_TYPE, mime.as_ref())], file.data).into_response()
        }
        // SPA fallback: unknown non-API paths serve the app shell.
        None if !path.starts_with("api/") => match Assets::get("index.html") {
            Some(file) => ([(header::CONTENT_TYPE, "text/html")], file.data).into_response(),
            None => (StatusCode::NOT_FOUND, "frontend not built").into_response(),
        },
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
