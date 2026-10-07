//! `serve` command: bind on `127.0.0.1` (the bind acts as the instance lock), local token,
//! then the HTTP server.

use crate::{
    access_log, cache, config::Config, health, identity, journal, stats, store::Store,
    token::LocalToken,
};
use axum::{
    Router,
    extract::DefaultBodyLimit,
    middleware,
    routing::{get, post},
};
use std::{
    io::Write,
    net::SocketAddr,
    sync::{Arc, RwLock, atomic::AtomicU8},
};
use tokio::{net::TcpListener, sync::Semaphore};

/// State shared by the handlers.
pub struct AppState {
    pub config: Config,
    pub token: LocalToken,
    /// Current Identity and its Blob client, replaced together (`identity::start`).
    pub identity: RwLock<Identity>,
    /// Serialises Identity (re)loads (`identity::start`), keyring read included.
    pub reload: tokio::sync::Mutex<()>,
    /// Write state (`cache::WRITE_*`), exposed in `/health.write`.
    pub write: AtomicU8,
    /// Cap on concurrent Azure calls; extra requests wait.
    pub azure: Arc<Semaphore>,
    /// Per-Workspace counters since startup, read by `/stats`.
    pub stats: stats::Stats,
}

impl AppState {
    pub fn new(config: Config, token: LocalToken, store: Option<Arc<dyn Store>>) -> AppState {
        AppState {
            config,
            token,
            identity: RwLock::new(Identity {
                status: identity::Status::down(None, "Identity not initialised"),
                store,
                task: None,
                user: None,
            }),
            reload: Default::default(),
            write: AtomicU8::new(cache::WRITE_UNKNOWN),
            azure: Arc::new(Semaphore::new(16)),
            stats: Default::default(),
        }
    }

    /// Storage of the current Identity.
    pub fn store(&self) -> Option<Arc<dyn Store>> {
        self.identity.read().unwrap().store.clone()
    }
}

/// Current Identity: state read by `/health`, Blob storage built with its credential
/// (absent as long as the config does not allow building one) and renewal task.
/// An Identity change replaces all three (`identity::start`).
pub struct Identity {
    pub status: identity::Status,
    pub store: Option<Arc<dyn Store>>,
    pub task: Option<tokio::task::AbortHandle>,
    /// Adapter of the `user` Identity, read by `/stats` for the UPN.
    pub user: Option<Arc<crate::user::UserCredential>>,
}

/// Runs the Gateway in the foreground; returns the process exit code.
pub async fn serve() -> i32 {
    match run().await {
        Ok(code) => code,
        Err(e) => {
            writeln!(std::io::stderr(), "nx-azure-cache: {e}").ok();
            1
        }
    }
}

async fn run() -> Result<i32, String> {
    let dir = crate::config::config_dir()?;
    let config = Config::load(&dir)?;
    let addr = SocketAddr::from(([127, 0, 0, 1], config.port));

    let listener = match TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(e) => {
            if is_gateway(config.port).await {
                writeln!(
                    std::io::stdout(),
                    "nx-azure-cache already running on {addr}"
                )
                .ok();
                return Ok(0);
            }
            return Err(format!(
                "port {} is used by something else: {e}",
                config.port
            ));
        }
    };

    // After the bind: a single instance writes the log and creates the token.
    if let Err(e) = crate::config::log_dir().and_then(journal::init) {
        writeln!(std::io::stderr(), "nx-azure-cache: file log disabled: {e}").ok();
    }
    let token = LocalToken::load_or_create(&dir)?;
    // An unavailable Identity does not prevent serving: everything becomes a miss.
    let state = Arc::new(AppState::new(config, token, None));
    identity::start(&state, &|k| std::env::var(k).ok()).await;
    let app = Router::new()
        .route("/health", get(health::handler))
        .route("/stats", get(stats::handler))
        .route("/reload", post(identity::reload))
        .fallback(cache::handler)
        // An Entry can weigh hundreds of MB; the body is read as a stream.
        .layer(DefaultBodyLimit::disable())
        .layer(middleware::from_fn_with_state(
            state.clone(),
            access_log::middleware,
        ))
        .with_state(state);

    journal::line(&format!(
        "nx-azure-cache {} listening on http://{addr}",
        env!("CARGO_PKG_VERSION")
    ));
    axum::serve(listener, app)
        .await
        .map_err(|e| e.to_string())?;
    Ok(0)
}

/// The port is taken: is it a Gateway (recognised `/health` response)?
pub async fn is_gateway(port: u16) -> bool {
    matches!(
        crate::status::get(port, "/health", None).await,
        Some((_, body)) if body["service"] == "nx-azure-cache"
    )
}
