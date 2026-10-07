//! `user` Identity of developer machines: browser sign-in (PKCE, loopback) or device code,
//! then tokens obtained silently by refresh, with refresh token rotation. Commands `login`,
//! `logout`, `whoami`.

use crate::{config::Config, token_store::TokenStore};
use azure_core::{
    credentials::{AccessToken, Secret, TokenCredential, TokenRequestOptions},
    error::ErrorKind,
    http::Url,
    time::OffsetDateTime,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
};

/// Storage, plus `profile`: without it, the Entra v2 id token has no `preferred_username`.
const SCOPES: &str = "https://storage.azure.com/.default openid profile offline_access";
const EXPIRED: &str = "sign-in expired: run `nx-azure-cache login`";
const ABSENT: &str = "not signed in: run `nx-azure-cache login`";

/// Signed-in account, read from the id token.
#[derive(Debug, PartialEq)]
pub struct Identity {
    pub upn: String,
    pub tenant: String,
}

/// Tokens obtained silently.
pub struct Session {
    access_token: String,
    expires_in: u64,
    pub identity: Identity,
}

#[derive(Debug)]
pub struct Auth {
    http: reqwest::Client,
    /// `https://login.microsoftonline.com/{tenant}/oauth2/v2.0`.
    authority: String,
    client_id: String,
    store: TokenStore,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    id_token: Option<String>,
    expires_in: Option<u64>,
    error: Option<String>,
    error_description: Option<String>,
}

impl TokenResponse {
    /// Message of an OAuth error: Entra code and description, never a token.
    fn failure(self) -> String {
        format!(
            "{}: {}",
            self.error.unwrap_or_default(),
            self.error_description.unwrap_or_default()
        )
    }
}

impl Auth {
    /// Developer flows of the configured tenant and client, storage per `token_store`.
    pub fn new(config: &Config) -> Result<Auth, String> {
        let set = |v: &Option<String>| v.clone().filter(|v| !v.is_empty());
        let keys = [
            ("tenant_id", &config.tenant_id),
            ("client_id", &config.client_id),
        ];
        let (Some(tenant_id), Some(client_id)) = (set(keys[0].1), set(keys[1].1)) else {
            let missing: Vec<String> = keys
                .iter()
                .filter(|(_, v)| set(v).is_none())
                .map(|(k, _)| format!("`{k}` (or NX_AZURE_CACHE_{})", k.to_uppercase()))
                .collect();
            return Err(format!(
                "sign-in is not configured: set {} in config.toml, from the Entra tenant and \
                 the app registration used for developer sign-in (see docs/runbook-azure.md)",
                missing.join(" and ")
            ));
        };
        let store = TokenStore::from_config(&config.token_store, &crate::config::config_dir()?)?;
        // Tests only: a fake local OAuth endpoint instead of Entra.
        let authority = std::env::var("NX_AZURE_CACHE_AUTHORITY").unwrap_or_else(|_| {
            format!("https://login.microsoftonline.com/{tenant_id}/oauth2/v2.0")
        });
        Ok(Auth::with(authority, client_id, store))
    }

    fn with(authority: String, client_id: String, store: TokenStore) -> Auth {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .expect("HTTP client");
        Auth {
            http,
            authority,
            client_id,
            store,
        }
    }

    /// Auth code + PKCE through the browser, with account selection.
    pub async fn login_browser(&self) -> Result<Identity, String> {
        self.check_store().await?;
        let verifier = random_b64::<32>();
        let challenge = B64.encode(Sha256::digest(verifier.as_bytes()));
        let state = random_b64::<16>();
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| format!("no local port available: {e}"))?;
        // The address actually listened on: `localhost` may resolve to `::1` in the browser.
        let redirect = format!(
            "http://{}",
            listener.local_addr().map_err(|e| e.to_string())?
        );
        let url = Url::parse_with_params(
            &self.endpoint("authorize"),
            [
                ("client_id", self.client_id.as_str()),
                ("response_type", "code"),
                ("redirect_uri", &redirect),
                ("scope", SCOPES),
                ("code_challenge", &challenge),
                ("code_challenge_method", "S256"),
                ("state", &state),
                // Forces account selection: a guest or admin account without RBAC gets nothing.
                ("prompt", "select_account"),
            ],
        )
        .map_err(|e| e.to_string())?;
        if webbrowser::open(url.as_str()).is_ok() {
            println!(
                "Signing in through the browser… (no browser: `nx-azure-cache login --device`)"
            );
        } else {
            println!("Open this address in a browser:\n{url}");
        }

        let params = wait_for_redirect(listener, &state).await?;
        let get = |k: &str| params.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        if let Some(err) = get("error") {
            return Err(format!("{err}: {}", get("error_description").unwrap_or("")));
        }
        let code = get("code").ok_or("sign-in response without a code")?;
        let tok = self
            .post::<TokenResponse>(
                "token",
                &[
                    ("grant_type", "authorization_code"),
                    ("code", code),
                    ("redirect_uri", &redirect),
                    ("code_verifier", &verifier),
                ],
            )
            .await?;
        match tok.error {
            None => Ok(self.store_session(tok, None).await?.identity),
            Some(_) => Err(tok.failure()),
        }
    }

    /// Device code, for machines without a browser (SSH, container).
    pub async fn login_device(&self) -> Result<Identity, String> {
        self.check_store().await?;
        #[derive(Deserialize)]
        struct DeviceCode {
            device_code: String,
            message: String,
            interval: Option<u64>,
        }
        let dc: DeviceCode = self.post("devicecode", &[]).await?;
        println!("{}", dc.message);

        let mut interval = dc.interval.unwrap_or(5);
        loop {
            tokio::time::sleep(Duration::from_secs(interval)).await;
            let tok: TokenResponse = self
                .post(
                    "token",
                    &[
                        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                        ("device_code", &dc.device_code),
                    ],
                )
                .await?;
            match tok.error.as_deref() {
                None => return Ok(self.store_session(tok, None).await?.identity),
                Some("authorization_pending") => {}
                Some("slow_down") => interval += 5,
                Some(_) => return Err(tok.failure()),
            }
        }
    }

    /// Tokens obtained silently from the stored refresh token, re-read on every call: a
    /// `login` run alongside is picked up on the next refresh.
    pub async fn refresh(&self) -> Result<Session, String> {
        let store = self.store.clone();
        let rt = blocking(move || store.load()).await?.ok_or(ABSENT)?;
        let tok: TokenResponse = self
            .post(
                "token",
                &[("grant_type", "refresh_token"), ("refresh_token", &rt)],
            )
            .await?;
        match tok.error.as_deref() {
            None => self.store_session(tok, Some(rt)).await,
            // Refresh token expired, revoked, or issued for another tenant.
            Some("invalid_grant" | "interaction_required") => Err(EXPIRED.into()),
            Some(_) => Err(tok.failure()),
        }
    }

    /// Whether a refresh token is stored.
    pub async fn stored(&self) -> Result<bool, String> {
        let store = self.store.clone();
        Ok(blocking(move || store.load()).await?.is_some())
    }

    async fn check_store(&self) -> Result<(), String> {
        let store = self.store.clone();
        blocking(move || store.check()).await
    }

    /// Persists the refresh token (rotation; the previous one stays valid if Entra returns
    /// none) and extracts the session.
    async fn store_session(
        &self,
        tok: TokenResponse,
        previous_rt: Option<String>,
    ) -> Result<Session, String> {
        let (Some(access_token), Some(refresh_token), Some(id_token)) = (
            tok.access_token,
            tok.refresh_token.or(previous_rt),
            tok.id_token,
        ) else {
            return Err("incomplete token response (access, refresh or id token missing)".into());
        };
        let identity = identity(&id_token)?;
        let store = self.store.clone();
        // ponytail: a Gateway refresh in flight during a `login` may write the old refresh
        // token after the new one; a window of a few ms per hour, run `login` again.
        blocking(move || store.store(&refresh_token)).await?;
        Ok(Session {
            access_token,
            expires_in: tok.expires_in.unwrap_or(0),
            identity,
        })
    }

    /// Form POST to an Entra endpoint, `client_id` and `scope` included. An OAuth error
    /// (`error`) is returned as is to a caller expecting a `TokenResponse`.
    async fn post<T: serde::de::DeserializeOwned>(
        &self,
        endpoint: &str,
        form: &[(&str, &str)],
    ) -> Result<T, String> {
        let common = [("client_id", self.client_id.as_str()), ("scope", SCOPES)];
        let pairs: Vec<(&str, &str)> = common.iter().chain(form).copied().collect();
        let net = |e: reqwest::Error| format!("cannot reach Entra: {e}");
        let resp = self
            .http
            .post(self.endpoint(endpoint))
            .form(&pairs)
            .send()
            .await
            .map_err(net)?;
        let status = resp.status().as_u16();
        let body = resp.text().await.map_err(net)?;
        serde_json::from_str(&body).map_err(|_| {
            match serde_json::from_str::<TokenResponse>(&body) {
                Ok(err) if err.error.is_some() => err.failure(),
                _ if status >= 500 => format!("Entra unavailable ({endpoint}, HTTP {status})"),
                _ => format!("unreadable Entra response ({endpoint}, HTTP {status})"),
            }
        })
    }

    fn endpoint(&self, ep: &str) -> String {
        format!("{}/{ep}", self.authority)
    }
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| e.to_string())?
}

/// Waits for the redirect carrying the code. Browsers sometimes open empty speculative
/// connections, and another local process may hit the port: only the request carrying our
/// `state` counts. Gives up after 5 minutes (tab closed).
async fn wait_for_redirect(
    listener: TcpListener,
    state: &str,
) -> Result<Vec<(String, String)>, String> {
    let wait = async {
        loop {
            let (mut stream, _) = listener.accept().await.map_err(|e| e.to_string())?;
            let mut line = String::new();
            let mut reader = BufReader::new(&mut stream);
            let _ = tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line)).await;
            let target = line.split_whitespace().nth(1).unwrap_or("");
            let Ok(url) = Url::parse(&format!("http://127.0.0.1{target}")) else {
                continue;
            };
            let params: Vec<(String, String)> = url.query_pairs().into_owned().collect();
            if params.iter().any(|(k, v)| k == "state" && v == state) {
                let _ = stream
                    .write_all(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\n\
                         Connection: close\r\n\r\n\
                         nx-azure-cache: signed in, you can close this tab."
                            .as_bytes(),
                    )
                    .await;
                return Ok(params);
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(300), wait)
        .await
        .map_err(|_| "no response from the browser within 5 minutes".to_string())?
}

/// Reads `preferred_username` and `tid` from the id token. No signature check: the token
/// comes straight from the token endpoint, over TLS (OIDC Core §3.1.3.7).
fn identity(id_token: &str) -> Result<Identity, String> {
    #[derive(Deserialize)]
    struct Claims {
        preferred_username: Option<String>,
        tid: Option<String>,
    }
    let invalid = || "unreadable id token".to_string();
    let payload = id_token.split('.').nth(1).ok_or_else(invalid)?;
    let bytes = B64
        .decode(payload.trim_end_matches('='))
        .map_err(|_| invalid())?;
    let c: Claims = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    Ok(Identity {
        upn: c
            .preferred_username
            .ok_or("id token without preferred_username")?,
        tenant: c.tid.unwrap_or_default(),
    })
}

pub(crate) fn random_b64<const N: usize>() -> String {
    let mut buf = [0u8; N];
    getrandom::fill(&mut buf).expect("system random generator unavailable");
    B64.encode(buf)
}

/// `TokenCredential` adapter: caches the access token and refreshes only within 5 min of
/// its expiry, under a lock: N concurrent calls make a single refresh.
#[derive(Debug)]
pub struct UserCredential {
    auth: Auth,
    cached: tokio::sync::Mutex<Cached>,
    /// UPN read from the id token of the last successful refresh.
    upn: std::sync::Mutex<Option<String>>,
}

#[derive(Debug, Default)]
struct Cached {
    token: Option<AccessToken>,
    /// Last refresh failure, returned as is for `identity::RETRY` (until the next attempt
    /// of `keep_fresh`): a failing Identity does not call Entra again on every Blob call.
    failure: Option<(std::time::Instant, String)>,
}

impl UserCredential {
    pub fn new(auth: Auth) -> UserCredential {
        UserCredential {
            auth,
            cached: Default::default(),
            upn: std::sync::Mutex::new(None),
        }
    }

    /// UPN of the signed-in account, known after the first refresh.
    pub fn upn(&self) -> Option<String> {
        self.upn.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[async_trait::async_trait]
impl TokenCredential for UserCredential {
    async fn get_token(
        &self,
        _: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        let mut cached = self.cached.lock().await;
        let now = OffsetDateTime::now_utc();
        if let Some(token) = cached
            .token
            .as_ref()
            .filter(|t| (t.expires_on - now).whole_seconds() > 300)
        {
            return Ok(token.clone());
        }
        let failed = |e: String| azure_core::Error::with_message(ErrorKind::Credential, e);
        if let Some((at, e)) = &cached.failure
            && at.elapsed() < crate::identity::RETRY
        {
            return Err(failed(e.clone()));
        }
        let session = match self.auth.refresh().await {
            Ok(session) => session,
            Err(e) => {
                cached.failure = Some((std::time::Instant::now(), e.clone()));
                return Err(failed(e));
            }
        };
        *self.upn.lock().unwrap_or_else(|e| e.into_inner()) = Some(session.identity.upn);
        let token = AccessToken::new(
            Secret::new(session.access_token),
            now + Duration::from_secs(session.expires_in),
        );
        *cached = Cached {
            token: Some(token.clone()),
            failure: None,
        };
        Ok(token)
    }
}

/// `login [--device]`, `logout`, `whoami`: process exit code.
pub async fn command(name: &str, args: &[String]) -> i32 {
    match run(name, args).await {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("nx-azure-cache: {e}");
            1
        }
    }
}

async fn run(name: &str, args: &[String]) -> Result<(), String> {
    let dir = crate::config::config_dir()?;
    let config = Config::load(&dir)?;
    match name {
        "login" => {
            let auth = Auth::new(&config)?;
            let id = match args {
                [] => auth.login_browser().await?,
                [flag] if flag == "--device" => auth.login_device().await?,
                _ => return Err("usage: nx-azure-cache login [--device]".into()),
            };
            println!("signed in: {} (tenant {})", id.upn, id.tenant);
        }
        "logout" => {
            // The configured storage first: its error matters. The other one as a precaution
            // (no keyring on Linux: nothing to clear).
            let chosen = TokenStore::from_config(&config.token_store, &dir)?;
            let all = TokenStore::all(&dir);
            blocking(move || {
                chosen.clear()?;
                all.iter().filter(|s| **s != chosen).for_each(|s| drop(s.clear()));
                Ok(())
            })
            .await?;
            println!("signed out");
        }
        _ /* whoami */ => {
            let auth = match Auth::new(&config) {
                Ok(auth) => auth,
                Err(e) => {
                    println!("not signed in ({e})");
                    return Ok(());
                }
            };
            if !auth.stored().await? {
                println!("not signed in");
                return Ok(());
            }
            let id = auth.refresh().await?.identity;
            println!("{}\ntenant {}", id.upn, id.tenant);
            return Ok(());
        }
    }
    println!("{}", notify_gateway(&config, &dir).await);
    Ok(())
}

/// Tells an already running Gateway to reload its Identity.
async fn notify_gateway(config: &Config, dir: &std::path::Path) -> String {
    let Ok(token) = std::fs::read_to_string(dir.join("local-token")) else {
        return "Gateway never started: it will pick up the Identity at startup".into();
    };
    let Some(client) = crate::status::local_client() else {
        return "Gateway not notified: local HTTP client unavailable".into();
    };
    let sent = client
        .post(format!("http://127.0.0.1:{}/reload", config.port))
        .bearer_auth(token.trim())
        .send()
        .await;
    match sent {
        Ok(r) if r.status().is_success() => "Gateway notified: Identity reloaded".into(),
        Ok(r) => format!("Gateway: reload refused (HTTP {})", r.status().as_u16()),
        Err(_) => "Gateway not running: it will pick up the Identity at startup".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Form, Json, Router, extract::State, http::StatusCode, routing::post};
    use std::{
        collections::HashMap,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    fn jwt(claims: &str) -> String {
        format!(
            "{}.{}.sig",
            B64.encode(r#"{"alg":"RS256"}"#),
            B64.encode(claims)
        )
    }

    #[test]
    fn identity_read_from_id_token() {
        let id = identity(&jwt(
            r#"{"preferred_username":"dev@example.com","tid":"t1"}"#,
        ))
        .unwrap();
        assert_eq!(
            id,
            Identity {
                upn: "dev@example.com".into(),
                tenant: "t1".into()
            }
        );
        assert!(identity(&jwt(r#"{"name":"x"}"#)).is_err());
        assert!(identity("not-a-jwt").is_err());
        assert!(identity("a.!!!.c").is_err());
    }

    type Reply = fn(usize, &HashMap<String, String>) -> serde_json::Value;

    /// Fake OAuth endpoint: `reply(n, form)` answers the n-th call (starting at 1), after
    /// 50 ms so that concurrent calls overlap.
    async fn fake(reply: Reply) -> (String, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let handler = |State((calls, reply)): State<(Arc<AtomicUsize>, Reply)>,
                       Form(form): Form<HashMap<String, String>>| async move {
            let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
            tokio::time::sleep(Duration::from_millis(50)).await;
            let body = reply(n, &form);
            let status = match body.get("error") {
                Some(_) => StatusCode::BAD_REQUEST,
                None => StatusCode::OK,
            };
            (status, Json(body))
        };
        let app = Router::new()
            .route("/{endpoint}", post(handler))
            .with_state((calls.clone(), reply));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });
        (format!("http://{addr}"), calls)
    }

    fn tokens(n: usize) -> serde_json::Value {
        serde_json::json!({
            "access_token": format!("at-{n}"),
            "refresh_token": format!("rt-{n}"),
            "id_token": jwt(r#"{"preferred_username":"dev@example.com","tid":"t1"}"#),
            "expires_in": 3600,
        })
    }

    fn auth(authority: String, name: &str) -> (Auth, std::path::PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("nx-azure-cache-test-{name}-{}", random_b64::<6>()));
        let store = TokenStore::from_config("file", &dir).unwrap();
        (Auth::with(authority, "client".into(), store), dir)
    }

    #[tokio::test]
    async fn single_refresh_for_n_calls_then_cache() {
        let (authority, calls) = fake(|n, form| {
            assert_eq!(form["grant_type"], "refresh_token");
            assert_eq!(form["refresh_token"], "rt-0");
            assert!(form["scope"].contains("offline_access"));
            tokens(n)
        })
        .await;
        let (auth, dir) = auth(authority, "adapt");
        auth.store.store("rt-0").unwrap();
        let cred = Arc::new(UserCredential::new(auth));
        let all = (0..20).map(|_| {
            let cred = cred.clone();
            tokio::spawn(async move { cred.get_token(&[], None).await.unwrap() })
        });
        for t in futures_util::future::join_all(all).await {
            assert_eq!(t.unwrap().token.secret(), "at-1");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // Served from the cache afterwards.
        let t = cred.get_token(&[], None).await.unwrap();
        assert_eq!(t.token.secret(), "at-1");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // Rotation: the new refresh token is stored.
        assert_eq!(cred.auth.store.load().unwrap().as_deref(), Some("rt-1"));
        // UPN read from the id token, exposed by `/stats.identity`.
        assert_eq!(cred.upn().as_deref(), Some("dev@example.com"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn refused_refresh_gives_ready_false_without_token_in_reason() {
        let (authority, _) = fake(|_, _| {
            serde_json::json!({"error": "invalid_grant", "error_description": "AADSTS70043: expired"})
        })
        .await;
        let (auth, dir) = auth(authority, "expired");
        auth.store.store("rt-secret").unwrap();
        let config = Config {
            account: Some("mystorageaccount".into()),
            ..Config::default()
        };
        let state = Arc::new(crate::server::AppState::new(
            config,
            crate::token::LocalToken("a".repeat(64)),
            None,
        ));
        let task = tokio::spawn(crate::identity::keep_fresh(
            state.clone(),
            "user",
            Arc::new(UserCredential::new(auth)),
        ));
        state.identity.write().unwrap().task = Some(task.abort_handle());
        tokio::time::sleep(Duration::from_millis(300)).await;
        let status = state.identity.read().unwrap().status.clone();
        assert!(!status.ready);
        assert_eq!(status.reason, format!("user: {EXPIRED}"));
        assert!(!status.reason.contains("rt-secret"));
        task.abort();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn refresh_failure_cached_until_next_attempt() {
        let (authority, calls) =
            fake(|_, _| serde_json::json!({"error": "invalid_grant", "error_description": "x"}))
                .await;
        let (auth, dir) = auth(authority, "negative");
        auth.store.store("rt-0").unwrap();
        let cred = UserCredential::new(auth);
        for _ in 0..3 {
            let e = cred.get_token(&[], None).await.unwrap_err();
            assert!(e.to_string().contains(EXPIRED), "{e}");
        }
        // A failing Identity does not send an Entra POST on every Blob call.
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // Delay elapsed (next `keep_fresh` attempt): new try.
        cred.cached.lock().await.failure.as_mut().unwrap().0 -= crate::identity::RETRY;
        assert!(cred.get_token(&[], None).await.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn device_code_waits_then_stores() {
        let (authority, calls) = fake(|n, form| match n {
            1 => serde_json::json!({"device_code": "dc", "message": "go to …", "interval": 0}),
            2 => serde_json::json!({"error": "authorization_pending"}),
            _ => {
                assert_eq!(form["device_code"], "dc");
                tokens(n)
            }
        })
        .await;
        let (auth, dir) = auth(authority, "device");
        let id = auth.login_device().await.unwrap();
        assert_eq!(id.upn, "dev@example.com");
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(auth.store.load().unwrap().as_deref(), Some("rt-3"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn redirect_filtered_by_state() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let wait = tokio::spawn(async move { wait_for_redirect(listener, "s1").await });
        let send = |req: &'static str| async move {
            let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
            s.write_all(req.as_bytes()).await.unwrap();
            s
        };
        drop(send("").await); // empty speculative connection
        drop(send("GET /?code=x&state=other HTTP/1.1\r\n\r\n").await);
        let mut ok = send("GET /?code=c1&state=s1 HTTP/1.1\r\n\r\n").await;
        let params = wait.await.unwrap().unwrap();
        assert!(params.contains(&("code".into(), "c1".into())));
        let mut page = String::new();
        tokio::io::AsyncReadExt::read_to_string(&mut ok, &mut page)
            .await
            .unwrap();
        assert!(page.contains("signed in"));
    }

    #[test]
    fn login_without_tenant_or_client_says_what_to_set() {
        let e = Auth::new(&Config::default()).unwrap_err();
        assert!(
            e.contains("`tenant_id` (or NX_AZURE_CACHE_TENANT_ID)"),
            "{e}"
        );
        assert!(
            e.contains("`client_id` (or NX_AZURE_CACHE_CLIENT_ID)"),
            "{e}"
        );
        let tenant_only = Config {
            tenant_id: Some("t".into()),
            ..Config::default()
        };
        let e = Auth::new(&tenant_only).unwrap_err();
        assert!(e.contains("client_id") && !e.contains("tenant_id"), "{e}");
    }
}
