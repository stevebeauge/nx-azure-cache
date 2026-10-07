//! Identity: credential selection, then a `ready` + `kind` + `reason` state, kept up to date
//! by a background task and read by `/health` without a network call.

use crate::{config::Config, server::AppState, user::UserCredential};
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
};
use azure_core::{credentials::TokenCredential, time::OffsetDateTime};
use azure_identity::{
    AzureCliCredential, AzurePipelinesCredential, ManagedIdentityCredential,
    ManagedIdentityCredentialOptions, UserAssignedId, WorkloadIdentityCredential,
};
use serde::Serialize;
use std::{sync::Arc, time::Duration};

pub const SCOPE: &str = "https://storage.azure.com/.default";
/// An acquisition that does not answer (IMDS outside Azure, network down) is abandoned.
const ACQUIRE: Duration = Duration::from_secs(60);
/// Delay before retrying after a failure.
pub const RETRY: Duration = Duration::from_secs(30);
/// Renewal margin: the SDK credential cache renews within 5 min of expiry; it is called at
/// 4 min, hence inside its window.
const MARGIN: i64 = 240;

/// Environment lookup, injectable for tests.
pub type Env<'a> = &'a (dyn Fn(&str) -> Option<String> + Sync);

/// Identity state exposed by `/health.identity`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Status {
    pub ready: bool,
    pub kind: Option<&'static str>,
    /// Readable reason when `ready` is false, empty otherwise.
    pub reason: String,
}

impl Status {
    pub fn down(kind: Option<&'static str>, reason: impl Into<String>) -> Status {
        Status {
            ready: false,
            kind,
            reason: reason.into(),
        }
    }
}

/// Credential to use. `auto` picks a single one, with no chain at runtime: `pipelines`,
/// then `workload`, then `user` if a refresh token is stored (`user_stored`).
/// `managed` and `cli` are never picked by `auto` (no slow IMDS probe outside Azure).
/// `Ok(None)`: no Identity.
pub fn select(
    credential: &str,
    env: Env,
    user_stored: bool,
) -> Result<Option<&'static str>, String> {
    let set = |k: &str| env(k).is_some_and(|v| !v.is_empty());
    Ok(Some(match credential {
        "auto" if set("SYSTEM_OIDCREQUESTURI") => "pipelines",
        "auto" if set("AZURE_FEDERATED_TOKEN_FILE") => "workload",
        "auto" if user_stored => "user",
        "auto" => return Ok(None),
        "user" => "user",
        "pipelines" => "pipelines",
        "workload" => "workload",
        "managed" => "managed",
        "cli" => "cli",
        other => {
            return Err(format!(
                "unknown credential: {other:?} (auto, user, pipelines, workload, managed or cli)"
            ));
        }
    }))
}

/// Built credential, and for `user` the concrete adapter that knows the UPN.
type Built = (Arc<dyn TokenCredential>, Option<Arc<UserCredential>>);

/// Builds the `kind` credential. No network call here.
pub fn build(kind: &str, config: &Config, env: Env) -> Result<Built, String> {
    let sdk = |e: azure_core::Error| e.to_string();
    let cred: Arc<dyn TokenCredential> = match kind {
        "pipelines" => {
            let var = |k: &str| env(k).filter(|v| !v.is_empty());
            let Some(access_token) = var("SYSTEM_ACCESSTOKEN") else {
                return Err(
                    "SYSTEM_ACCESSTOKEN missing: a secret is never exported by default, \
                            add `SYSTEM_ACCESSTOKEN: $(System.AccessToken)` to the step's `env:`"
                        .into(),
                );
            };
            let ids = [
                "AZURESUBSCRIPTION_TENANT_ID",
                "AZURESUBSCRIPTION_CLIENT_ID",
                "AZURESUBSCRIPTION_SERVICE_CONNECTION_ID",
            ]
            .map(|k| var(k).ok_or(k));
            let [Ok(tenant), Ok(client), Ok(connection)] = ids.clone() else {
                let missing: Vec<_> = ids.iter().filter_map(|r| r.clone().err()).collect();
                return Err(format!(
                    "{} missing: run `serve` from an AzureCLI@2 step on the service connection",
                    missing.join(", ")
                ));
            };
            // The SDK reads SYSTEM_OIDCREQUESTURI itself.
            AzurePipelinesCredential::new(tenant, client, &connection, access_token, None)
                .map_err(sdk)?
        }
        "workload" => WorkloadIdentityCredential::new(None).map_err(sdk)?,
        "managed" => ManagedIdentityCredential::new(Some(ManagedIdentityCredentialOptions {
            user_assigned_id: config
                .managed_client_id
                .clone()
                .map(UserAssignedId::ClientId),
            ..Default::default()
        }))
        .map_err(sdk)?,
        "cli" => AzureCliCredential::new(None).map_err(sdk)?,
        "user" => {
            let user = Arc::new(UserCredential::new(crate::user::Auth::new(config)?));
            return Ok((user.clone(), Some(user)));
        }
        other => return Err(format!("unknown credential: {other:?}")),
    };
    Ok((cred, None))
}

/// (Re)builds the configured Identity: credential, Blob client, renewal task, and resets the
/// write state to `unknown`. Called at startup and on every `login` or `logout`
/// (`POST /reload`): the Blob client holds the pipeline token cache, changing the credential
/// alone would not be enough.
pub async fn start(state: &Arc<AppState>, env: Env<'_>) {
    // Two concurrent `/reload`s: without this lock taken before the keyring read, the first
    // could apply an already stale state after the second.
    let _reload = state.reload.lock().await;
    let config = &state.config;
    // The refresh token storage is only read if `auto` found nothing else.
    let user_stored = select(&config.credential, env, false) == Ok(None)
        && match crate::user::Auth::new(config) {
            Ok(auth) => auth.stored().await.unwrap_or(false),
            Err(_) => false,
        };
    let built = match &config.account {
        None => Err(Status::down(None, "account missing from the config")),
        Some(account) => match select(&config.credential, env, user_stored) {
            Err(e) => Err(Status::down(None, e)),
            Ok(None) => Err(Status::down(
                None,
                "no Identity available: no SYSTEM_OIDCREQUESTURI, no \
                 AZURE_FEDERATED_TOKEN_FILE, no `nx-azure-cache login`; or set \
                 `credential` in the config",
            )),
            Ok(Some(kind)) => build(kind, config, env)
                .and_then(|(cred, user)| {
                    let store = crate::store::azure(account, &config.container, cred.clone())?;
                    Ok((kind, cred, store, user))
                })
                .map_err(|e| Status::down(Some(kind), format!("{kind}: {e}"))),
        },
    };
    let mut identity = state.identity.write().unwrap();
    if let Some(task) = identity.task.take() {
        task.abort();
    }
    state.write.store(
        crate::cache::WRITE_UNKNOWN,
        std::sync::atomic::Ordering::Relaxed,
    );
    match built {
        Ok((kind, cred, store, user)) => {
            identity.status = Status::down(Some(kind), "acquiring token");
            identity.store = Some(store);
            identity.user = user;
            // Spawned under the lock: it only writes the state once `task` is set.
            let task = tokio::spawn(keep_fresh(state.clone(), kind, cred));
            identity.task = Some(task.abort_handle());
        }
        Err(status) => {
            crate::journal::line(&format!("Identity unavailable: {}", status.reason));
            identity.status = status;
            identity.store = None;
            identity.user = None;
        }
    }
}

/// `POST /reload`: called by `login` and `logout` to pick up the new Identity without a
/// restart. Local token required.
pub async fn reload(State(state): State<Arc<AppState>>, headers: HeaderMap) -> StatusCode {
    if !state.token.matches_header(&headers) {
        return StatusCode::UNAUTHORIZED;
    }
    start(&state, &|k| std::env::var(k).ok()).await;
    StatusCode::OK
}

/// Gets a token right at startup, then renews it before expiry, keeping
/// `state.identity.status` up to date. The SDK credential caches the token.
pub async fn keep_fresh(state: Arc<AppState>, kind: &'static str, cred: Arc<dyn TokenCredential>) {
    loop {
        let (status, wait) =
            match tokio::time::timeout(ACQUIRE, cred.get_token(&[SCOPE], None)).await {
                Ok(Ok(token)) => {
                    let left = (token.expires_on - OffsetDateTime::now_utc()).whole_seconds();
                    let wait = Duration::from_secs((left - MARGIN).max(10) as u64);
                    let status = Status {
                        ready: true,
                        kind: Some(kind),
                        reason: String::new(),
                    };
                    (status, wait)
                }
                Ok(Err(e)) => {
                    let reason = explain(kind, &state.config, &e.to_string());
                    (Status::down(Some(kind), reason), RETRY)
                }
                Err(_) => {
                    let reason = format!("{kind}: no response within {} s", ACQUIRE.as_secs());
                    (Status::down(Some(kind), reason), RETRY)
                }
            };
        if !status.ready {
            crate::journal::line(&format!("Identity unavailable: {}", status.reason));
        }
        {
            let mut identity = state.identity.write().unwrap();
            // A replaced task (Identity change) does not overwrite the state of the next one.
            if identity.task.as_ref().map(|t| t.id()) != Some(tokio::task::id()) {
                return;
            }
            // Token renewed: a transient write refusal (RBAC propagation, firewall) is
            // re-evaluated on the next PUT instead of blocking until restart.
            if status.ready {
                let (denied, unknown) = (crate::cache::WRITE_DENIED, crate::cache::WRITE_UNKNOWN);
                let relaxed = std::sync::atomic::Ordering::Relaxed;
                let _ = state
                    .write
                    .compare_exchange(denied, unknown, relaxed, relaxed);
            }
            identity.status = status;
        }
        tokio::time::sleep(wait).await;
    }
}

/// Readable reason for an acquisition failure, with the likely cause of known pitfalls.
/// SDK messages contain neither token nor assertion.
fn explain(kind: &str, config: &Config, error: &str) -> String {
    let hint = if error.contains("AADSTS700016") || error.contains("AADSTS900023") {
        Some(
            "client id or tenant of another identity: they must designate the one holding \
             the federated credential (app registration or UAMI)",
        )
    } else if kind == "pipelines" && error.contains("401 response from the OIDC endpoint") {
        Some("SYSTEM_ACCESSTOKEN rejected: is it mapped to $(System.AccessToken)?")
    } else if kind == "managed"
        && config.managed_client_id.is_none()
        && error.contains("has not been assigned")
    {
        Some("does the machine have several managed identities? set managed_client_id")
    } else {
        None
    };
    // A single line: the Entra message spans several.
    let error = error.split_whitespace().collect::<Vec<_>>().join(" ");
    match hint {
        Some(hint) => format!("{kind}: {hint} ({error})"),
        None => format!("{kind}: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::LocalToken;
    use azure_core::credentials::{AccessToken, Secret, TokenRequestOptions};

    fn env_of(vars: &[&'static str]) -> impl Fn(&str) -> Option<String> {
        move |k: &str| vars.contains(&k).then(|| "x".to_owned())
    }

    #[test]
    fn auto_on_every_combination() {
        let (oidc, fed) = ("SYSTEM_OIDCREQUESTURI", "AZURE_FEDERATED_TOKEN_FILE");
        let cases: &[(&[&str], bool, Option<&str>)] = &[
            (&[], false, None),
            (&[], true, Some("user")),
            (&[fed], false, Some("workload")),
            (&[fed], true, Some("workload")),
            (&[oidc], false, Some("pipelines")),
            (&[oidc], true, Some("pipelines")),
            (&[oidc, fed], false, Some("pipelines")),
            (&[oidc, fed], true, Some("pipelines")),
        ];
        for (vars, user, expected) in cases {
            assert_eq!(
                select("auto", &env_of(vars), *user),
                Ok(*expected),
                "{vars:?} user={user}"
            );
        }
        // Variable set but empty: same as missing.
        let empty = |k: &str| (k == "SYSTEM_OIDCREQUESTURI").then(String::new);
        assert_eq!(select("auto", &empty, false), Ok(None));
        // An explicit choice ignores the environment; `managed` and `cli` only this way.
        assert_eq!(select("cli", &env_of(&[oidc]), true), Ok(Some("cli")));
        assert_eq!(select("managed", &env_of(&[]), false), Ok(Some("managed")));
        assert!(select("az", &env_of(&[]), false).is_err());
    }

    #[test]
    fn pipelines_pitfalls_and_messages() {
        let config = Config::default();
        let all = [
            "SYSTEM_OIDCREQUESTURI",
            "AZURESUBSCRIPTION_TENANT_ID",
            "AZURESUBSCRIPTION_CLIENT_ID",
            "AZURESUBSCRIPTION_SERVICE_CONNECTION_ID",
        ];
        let e = build("pipelines", &config, &env_of(&all)).unwrap_err();
        assert!(
            e.contains("SYSTEM_ACCESSTOKEN: $(System.AccessToken)"),
            "{e}"
        );
        let e = build("pipelines", &config, &env_of(&["SYSTEM_ACCESSTOKEN"])).unwrap_err();
        assert!(
            e.contains("AZURESUBSCRIPTION_TENANT_ID, AZURESUBSCRIPTION_CLIENT_ID"),
            "{e}"
        );
        assert!(e.contains("AzureCLI@2"), "{e}");

        let entra = "AADSTS700016: Application with identifier 'x' was not found\r\nTrace ID: 1";
        let m = explain("pipelines", &config, entra);
        assert!(m.contains("another identity") && m.contains("Trace ID: 1") && !m.contains('\n'));
        let oidc = "401 response from the OIDC endpoint. Check service connection ID";
        assert!(explain("pipelines", &config, oidc).contains("SYSTEM_ACCESSTOKEN"));
        let imds = "The requested identity has not been assigned to this resource";
        assert!(explain("managed", &config, imds).contains("managed_client_id"));
        let with_id = Config {
            managed_client_id: Some("id".into()),
            ..Config::default()
        };
        assert_eq!(
            explain("managed", &with_id, imds),
            format!("managed: {imds}")
        );
    }

    #[tokio::test]
    async fn reloads_serialised_before_keyring_read() {
        let token = LocalToken("a".repeat(64));
        let state = Arc::new(AppState::new(Config::default(), token, None));
        let other = state.reload.lock().await; // a reload in progress
        let env = env_of(&[]);
        let mut reload = std::pin::pin!(start(&state, &env));
        let wait = Duration::from_millis(50);
        assert!(tokio::time::timeout(wait, reload.as_mut()).await.is_err());
        drop(other);
        reload.await;
    }

    #[tokio::test]
    async fn successful_renewal_lifts_a_write_refusal() {
        for (fake, expected) in [(Fake::Works, "unknown"), (Fake::Fails, "denied")] {
            let token = LocalToken("a".repeat(64));
            let state = Arc::new(AppState::new(Config::default(), token, None));
            state.write.store(
                crate::cache::WRITE_DENIED,
                std::sync::atomic::Ordering::Relaxed,
            );
            let task = tokio::spawn(keep_fresh(state.clone(), "cli", Arc::new(fake)));
            state.identity.write().unwrap().task = Some(task.abort_handle());
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert_eq!(crate::cache::write_label(&state), expected);
            task.abort();
        }
    }

    /// Fake credential: fails, succeeds, or never answers (Azure unreachable).
    #[derive(Debug)]
    enum Fake {
        Fails,
        Works,
        Hangs,
    }

    #[async_trait::async_trait]
    impl TokenCredential for Fake {
        async fn get_token(
            &self,
            _: &[&str],
            _: Option<TokenRequestOptions<'_>>,
        ) -> azure_core::Result<AccessToken> {
            match self {
                Fake::Fails => Err(azure_core::Error::with_message(
                    azure_core::error::ErrorKind::Credential,
                    "Please run 'az login' to set up account",
                )),
                Fake::Works => Ok(AccessToken::new(
                    Secret::new("t"),
                    OffsetDateTime::now_utc() + Duration::from_secs(3600),
                )),
                Fake::Hangs => std::future::pending().await,
            }
        }
    }

    async fn status_after(fake: Fake) -> (Status, serde_json::Value, Duration) {
        let config = Config {
            account: Some("mystorageaccount".into()),
            ..Config::default()
        };
        let state = Arc::new(AppState::new(config, LocalToken("a".repeat(64)), None));
        state.identity.write().unwrap().status = Status::down(Some("cli"), "acquiring token");
        let task = tokio::spawn(keep_fresh(state.clone(), "cli", Arc::new(fake)));
        state.identity.write().unwrap().task = Some(task.abort_handle());
        tokio::time::sleep(Duration::from_millis(50)).await;
        let start = std::time::Instant::now();
        let health = crate::health::handler(axum::extract::State(state.clone())).await;
        let elapsed = start.elapsed();
        let status = state.identity.read().unwrap().status.clone();
        (status, health.0, elapsed)
    }

    #[tokio::test]
    async fn failure_then_success_then_azure_unreachable() {
        let (status, health, _) = status_after(Fake::Fails).await;
        assert!(!status.ready);
        assert_eq!(
            status.reason,
            "cli: Please run 'az login' to set up account"
        );
        assert_eq!(health["identity"]["reason"], status.reason.as_str());

        let (status, health, _) = status_after(Fake::Works).await;
        assert!(status.ready);
        assert_eq!(health["identity"]["ready"], true);
        assert_eq!(health["identity"]["kind"], "cli");

        let (status, health, elapsed) = status_after(Fake::Hangs).await;
        assert!(elapsed < Duration::from_millis(50), "{elapsed:?}");
        assert!(!status.ready);
        assert_eq!(health["identity"]["reason"], "acquiring token");
    }

    #[tokio::test]
    async fn identity_change_stops_old_task_and_resets_write_to_unknown() {
        let state = Arc::new(AppState::new(
            Config::default(),
            LocalToken("a".repeat(64)),
            None,
        ));
        let old = tokio::spawn(std::future::pending::<()>());
        state.identity.write().unwrap().task = Some(old.abort_handle());
        state.write.store(
            crate::cache::WRITE_DENIED,
            std::sync::atomic::Ordering::Relaxed,
        );
        start(&state, &env_of(&[])).await;
        assert!(old.await.unwrap_err().is_cancelled());
        assert_eq!(crate::cache::write_label(&state), "unknown");
        let identity = state.identity.read().unwrap();
        assert!(identity.task.is_none() && identity.store.is_none());
        assert_eq!(identity.status.reason, "account missing from the config");
    }
}
