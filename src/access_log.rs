//! One log line per request, and the `/stats` counters.
//! Never a secret: no token, no header, no query string.

use crate::{journal, server::AppState, stats};
use axum::{
    extract::{Request, State},
    http::Method,
    middleware::Next,
    response::Response,
};
use std::{sync::Arc, time::Instant};

/// Detail a cache route attaches to its response for the log line.
#[derive(Clone)]
pub struct CacheLog {
    pub workspace: String,
    pub hash: String,
    /// `hit`, `miss`, `stored`, `exists`, `denied` or `error`.
    pub outcome: &'static str,
    /// Short reason for a refusal or an error (`token`, `format`, Azure code…).
    pub detail: Option<String>,
    pub bytes: u64,
}

pub async fn middleware(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let start = Instant::now();
    let method = req.method().clone();
    let path = req.uri().path().to_owned();
    let res = next.run(req).await;
    let ms = start.elapsed().as_millis();
    let status = res.status().as_u16();
    match res.extensions().get::<CacheLog>() {
        Some(log) => {
            let workspace: String = log.workspace.chars().take(63).collect();
            let hash: String = log.hash.chars().take(12).collect();
            let detail = log
                .detail
                .as_deref()
                .map(|d| format!(" reason={d}"))
                .unwrap_or_default();
            // `{:?}` escapes the control characters of a malformed path.
            journal::line(&format!(
                "{method} workspace={workspace:?} hash={hash:?} {} {}B {ms}ms {status}{detail}",
                log.outcome, log.bytes
            ));
            stats::record(&state.stats, method == Method::PUT, status, log);
        }
        None => journal::line(&format!("{method} {path:?} {ms}ms {status}")),
    }
    res
}
