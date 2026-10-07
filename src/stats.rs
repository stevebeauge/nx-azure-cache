//! Per-Workspace counters since startup, and `GET /stats` (local token required, `401`
//! otherwise).

use crate::access_log::CacheLog;
use crate::server::AppState;
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc, sync::Mutex};

#[derive(Default, Clone, Serialize)]
pub struct Counters {
    pub hits: u64,
    pub misses: u64,
    pub writes: u64,
    /// PUTs answered `409` (Entry already present).
    pub conflicts: u64,
    /// PUTs answered `403`, whatever the reason.
    pub forbidden: u64,
    pub azure_errors: u64,
    pub bytes_read: u64,
    pub bytes_written: u64,
}

pub type Stats = Mutex<BTreeMap<String, Counters>>;

/// Counts a cache response from the status Nx receives.
pub fn record(stats: &Stats, put: bool, status: u16, log: &CacheLog) {
    // An invalid Workspace does not open a row: nothing arbitrary in the table.
    if !crate::cache::valid_workspace(&log.workspace) {
        return;
    }
    let mut map = stats.lock().unwrap_or_else(|e| e.into_inner());
    let c = map.entry(log.workspace.clone()).or_default();
    match (put, status) {
        (false, 200) => {
            c.hits += 1;
            c.bytes_read += log.bytes;
        }
        (false, _) => c.misses += 1,
        (true, 200) => {
            c.writes += 1;
            c.bytes_written += log.bytes;
        }
        (true, 409) => c.conflicts += 1,
        (true, _) => c.forbidden += 1,
    }
    if log.outcome == "error" {
        c.azure_errors += 1;
    }
}

fn identity_name(state: &AppState) -> Option<String> {
    let identity = state.identity.read().unwrap();
    let upn = identity.user.as_ref().and_then(|u| u.upn());
    upn.or(identity.status.kind.map(str::to_owned))
}

pub async fn handler(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if !state.token.matches_header(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let workspaces = state
        .stats
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    Json(json!({
        "service": "nx-azure-cache",
        "version": env!("CARGO_PKG_VERSION"),
        // UPN of the `user` Identity once known, otherwise name of the selected Identity
        // (`kind`), `null` if none.
        "identity": identity_name(&state),
        "workspaces": workspaces,
    }))
    .into_response()
}
