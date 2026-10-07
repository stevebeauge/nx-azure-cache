//! `GET /health`: always `200`, no token, no network call, nothing sensitive.
//! Backward compatible: fields are added, never removed.

use crate::server::AppState;
use axum::{Json, extract::State};
use serde_json::{Value, json};
use std::sync::Arc;

pub async fn handler(State(state): State<Arc<AppState>>) -> Json<Value> {
    // Reads a state kept up to date in the background: never a network call here.
    let identity = state.identity.read().unwrap().status.clone();
    Json(json!({
        "service": "nx-azure-cache",
        "version": env!("CARGO_PKG_VERSION"),
        "identity": identity,
        "write": crate::cache::write_label(&state),
    }))
}
