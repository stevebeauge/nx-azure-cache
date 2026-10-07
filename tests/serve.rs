//! End-to-end tests: each test runs the binary on a free port, with a temporary config
//! directory, and talks raw HTTP to it.

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const BIN: &str = env!("CARGO_BIN_EXE_nx-azure-cache");

struct Gateway {
    child: Child,
    port: u16,
    dir: PathBuf,
}

impl Drop for Gateway {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Gateway {
    fn token(&self) -> String {
        std::fs::read_to_string(self.dir.join("nx-azure-cache/local-token")).unwrap()
    }
}

fn temp_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "nx-azure-cache-test-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// `serve` command isolated in `dir` (config and logs), port from the env if given.
fn serve_cmd(dir: &PathBuf, port: Option<u16>) -> Command {
    cmd(dir, port, "serve")
}

fn cmd(dir: &PathBuf, port: Option<u16>, command: &str) -> Command {
    let mut cmd = Command::new(BIN);
    cmd.arg(command)
        .env("APPDATA", dir)
        .env("XDG_CONFIG_HOME", dir)
        .env("LOCALAPPDATA", dir)
        .env("XDG_DATA_HOME", dir)
        .env_remove("NX_AZURE_CACHE_PORT")
        .env_remove("NX_AZURE_CACHE_ACCOUNT")
        .env_remove("NX_AZURE_CACHE_CREDENTIAL")
        // Away from the real keyring: after a `login` on the machine, `auto` would pick `user`.
        .env("NX_AZURE_CACHE_TOKEN_STORE", "file")
        // On a CI agent, `auto` would pick the agent's Identity.
        .env_remove("SYSTEM_OIDCREQUESTURI")
        .env_remove("AZURE_FEDERATED_TOKEN_FILE");
    if let Some(port) = port {
        cmd.env("NX_AZURE_CACHE_PORT", port.to_string());
    }
    cmd
}

/// Starts a Gateway and waits until it answers on `/health`.
fn start_in(dir: PathBuf, env_port: Option<u16>, expected_port: u16) -> Gateway {
    start_cmd(serve_cmd(&dir, env_port), expected_port)
}

fn start_cmd(mut cmd: Command, port: u16) -> Gateway {
    let child = cmd.stdout(Stdio::null()).spawn().unwrap();
    let dir = PathBuf::from(
        cmd.get_envs()
            .find(|(k, _)| *k == "APPDATA")
            .unwrap()
            .1
            .unwrap(),
    );
    let gw = Gateway { child, port, dir };
    wait_health(gw.port);
    gw
}

fn wait_health(port: u16) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while http(port, "GET", "/health", None, b"").0 != 200 {
        assert!(Instant::now() < deadline, "the Gateway does not answer");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn start() -> Gateway {
    let port = free_port();
    start_in(temp_dir(), Some(port), port)
}

/// Minimal HTTP/1.1 request; returns (status, body). Status 0 if the connection fails.
fn http(port: u16, method: &str, path: &str, token: Option<&str>, body: &[u8]) -> (u16, String) {
    let Ok(mut s) = TcpStream::connect(("127.0.0.1", port)) else {
        return (0, String::new());
    };
    let auth = token
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n{auth}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    s.write_all(head.as_bytes()).unwrap();
    s.write_all(body).unwrap();
    read_response(&mut s)
}

fn read_response(s: &mut TcpStream) -> (u16, String) {
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8_lossy(&raw).into_owned();
    let status = text.get(9..12).and_then(|c| c.parse().ok()).unwrap_or(0);
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_owned())
        .unwrap_or_default();
    (status, body)
}

#[test]
fn health_without_token_in_spec_format() {
    let gw = start();
    let (status, body) = http(gw.port, "GET", "/health", None, b"");
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["service"], "nx-azure-cache");
    assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(v["identity"]["ready"], false);
    assert_eq!(v["identity"]["reason"], "account missing from the config");
    assert_eq!(v["write"], "unknown");
}

#[test]
fn cache_routes_only_return_404_on_get_and_403_on_put() {
    let gw = start();
    let good = gw.token();
    let wrong = "0".repeat(64);
    let long_hash = "a".repeat(129);
    let paths = [
        "/acme-web/v1/cache/abc123".to_owned(),
        "/Workspace/v1/cache/abc".to_owned(),
        "/-workspace/v1/cache/abc".to_owned(),
        format!("/{}/v1/cache/abc", "a".repeat(64)),
        "/ws/v1/cache/a-b".to_owned(),
        format!("/ws/v1/cache/{long_hash}"),
        "/ws/v1/cache/a%2Fb".to_owned(),
        "/%FF/v1/cache/abc".to_owned(),
        "//v1/cache/abc".to_owned(),
        "/v1/cache/abc".to_owned(),
        "/ws/v1/cache/".to_owned(),
        "/ws/v1/cache/abc/extra".to_owned(),
        "/".to_owned(),
    ];
    for path in &paths {
        for token in [None, Some(wrong.as_str()), Some(good.as_str()), Some("")] {
            assert_eq!(
                http(gw.port, "GET", path, token, b"").0,
                404,
                "GET {path} {token:?}"
            );
            assert_eq!(
                http(gw.port, "PUT", path, token, b"tarball").0,
                403,
                "PUT {path} {token:?}"
            );
        }
    }
    // Token without the Bearer prefix.
    let mut s = TcpStream::connect(("127.0.0.1", gw.port)).unwrap();
    let req = format!(
        "GET /ws/v1/cache/abc HTTP/1.1\r\nHost: x\r\nAuthorization: {good}\r\nConnection: close\r\n\r\n"
    );
    s.write_all(req.as_bytes()).unwrap();
    assert_eq!(read_response(&mut s).0, 404);
}

#[test]
fn put_of_50_mb_answers_after_reading_the_whole_body() {
    let gw = start();
    let size = 50 * 1024 * 1024;
    let body = vec![7u8; size];
    for token in ["0".repeat(64), gw.token()] {
        let mut s = TcpStream::connect(("127.0.0.1", gw.port)).unwrap();
        let head = format!(
            "PUT /ws/v1/cache/abc HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {token}\r\nContent-Length: {size}\r\nConnection: close\r\n\r\n"
        );
        s.write_all(head.as_bytes()).unwrap();
        s.write_all(&body[..size - 1]).unwrap();
        // Until the last byte is sent, no response may arrive.
        s.set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        let err = s.read(&mut [0u8; 1]).unwrap_err();
        assert!(
            matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut),
            "{err}"
        );
        s.write_all(&body[size - 1..]).unwrap();
        assert_eq!(read_response(&mut s).0, 403);
    }
}

#[test]
fn local_token_created_then_kept() {
    let dir = temp_dir();
    let port = free_port();
    let first = start_in(dir.clone(), Some(port), port);
    let token = first.token();
    assert_eq!(token.len(), 64);
    assert!(token.bytes().all(|b| b.is_ascii_hexdigit()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(dir.join("nx-azure-cache/local-token")).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }
    drop(first);
    let second = start_in(dir, Some(port), port);
    assert_eq!(second.token(), token);
}

#[test]
fn second_instance_exits_0_and_foreign_port_exits_1() {
    let gw = start();
    let out = serve_cmd(&gw.dir, Some(gw.port)).output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).contains("already running"));

    let other = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = other.local_addr().unwrap().port();
    // Some "other program" that accepts and closes without speaking HTTP.
    std::thread::spawn(move || {
        for s in other.incoming() {
            drop(s);
        }
    });
    let out = serve_cmd(&temp_dir(), Some(port)).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn config_toml_then_env_override() {
    let dir = temp_dir();
    let (file_port, env_port) = (free_port(), free_port());
    std::fs::create_dir_all(dir.join("nx-azure-cache")).unwrap();
    std::fs::write(
        dir.join("nx-azure-cache/config.toml"),
        format!("port = {file_port}\naccount = \"mystorageaccount\"\n"),
    )
    .unwrap();

    let gw = start_in(dir.clone(), None, file_port);
    let v: serde_json::Value =
        serde_json::from_str(&http(file_port, "GET", "/health", None, b"").1).unwrap();
    let reason = v["identity"]["reason"].as_str().unwrap();
    assert!(reason.starts_with("no Identity available"), "{reason}");
    drop(gw);

    let _gw = start_in(dir, Some(env_port), env_port);
    assert_eq!(http(file_port, "GET", "/health", None, b"").0, 0);
}

/// Gateway with `credential = pipelines`, whose OIDC endpoint is a fake local server.
fn start_pipelines(oidc: &TcpListener) -> Gateway {
    let oidc_port = oidc.local_addr().unwrap().port();
    let port = free_port();
    let mut cmd = serve_cmd(&temp_dir(), Some(port));
    cmd.env("NX_AZURE_CACHE_ACCOUNT", "mystorageaccount")
        .env("NX_AZURE_CACHE_CREDENTIAL", "pipelines")
        .env(
            "SYSTEM_OIDCREQUESTURI",
            format!("http://127.0.0.1:{oidc_port}/oidc"),
        )
        .env("SYSTEM_ACCESSTOKEN", "build-token")
        .env(
            "AZURESUBSCRIPTION_TENANT_ID",
            "00000000-0000-0000-0000-000000000001",
        )
        .env(
            "AZURESUBSCRIPTION_CLIENT_ID",
            "00000000-0000-0000-0000-000000000002",
        )
        .env("AZURESUBSCRIPTION_SERVICE_CONNECTION_ID", "sc")
        .stderr(Stdio::null());
    start_cmd(cmd, port)
}

fn identity(port: u16) -> serde_json::Value {
    let body = http(port, "GET", "/health", None, b"").1;
    serde_json::from_str::<serde_json::Value>(&body).unwrap()["identity"].clone()
}

#[test]
fn health_under_50_ms_when_azure_does_not_answer() {
    // OIDC endpoint that accepts connections and never answers.
    let oidc = TcpListener::bind("127.0.0.1:0").unwrap();
    let gw = start_pipelines(&oidc);
    std::thread::spawn(move || {
        let held: Vec<_> = oidc.incoming().collect();
        drop(held);
    });
    // Blocking on the OIDC would take more than 60 s. The best time proves the answer has no
    // network call; a single call may be delayed by the load of other tests run in parallel.
    let mut best = Duration::MAX;
    for _ in 0..5 {
        let start = Instant::now();
        let id = identity(gw.port);
        let elapsed = start.elapsed();
        assert!(elapsed < Duration::from_secs(1), "{elapsed:?}");
        best = best.min(elapsed);
        assert_eq!(id["ready"], false);
        assert_eq!(id["kind"], "pipelines");
        assert_eq!(id["reason"], "acquiring token");
    }
    assert!(best < Duration::from_millis(50), "{best:?}");
    let stats = http(gw.port, "GET", "/stats", Some(&gw.token()), b"").1;
    let stats: serde_json::Value = serde_json::from_str(&stats).unwrap();
    assert_eq!(stats["identity"], "pipelines");
}

#[test]
fn rejected_system_accesstoken_gives_a_readable_reason() {
    // OIDC endpoint answering 401, as when SYSTEM_ACCESSTOKEN is not the right one.
    let oidc = TcpListener::bind("127.0.0.1:0").unwrap();
    let gw = start_pipelines(&oidc);
    std::thread::spawn(move || {
        for mut s in oidc.incoming().flatten() {
            let _ = s.read(&mut [0u8; 4096]);
            let _ = s.write_all(
                b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let id = loop {
        let id = identity(gw.port);
        if id["reason"] != "acquiring token" {
            break id;
        }
        assert!(Instant::now() < deadline, "{id}");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(id["ready"], false);
    let reason = id["reason"].as_str().unwrap();
    assert!(
        reason.starts_with("pipelines: SYSTEM_ACCESSTOKEN rejected"),
        "{reason}"
    );
    assert!(!reason.contains("build-token"), "{reason}");
}

#[test]
fn stats_requires_the_token_and_counts_per_workspace() {
    let gw = start();
    let token = gw.token();
    assert_eq!(http(gw.port, "GET", "/stats", None, b"").0, 401);
    assert_eq!(
        http(gw.port, "GET", "/stats", Some(&"0".repeat(64)), b"").0,
        401
    );

    // Without storage: the GET is a miss, the PUT a 403.
    assert_eq!(
        http(gw.port, "GET", "/acme-web/v1/cache/abc", Some(&token), b"").0,
        404
    );
    assert_eq!(
        http(
            gw.port,
            "PUT",
            "/acme-web/v1/cache/abc",
            Some(&token),
            b"tar"
        )
        .0,
        403
    );
    let (status, body) = http(gw.port, "GET", "/stats", Some(&token), b"");
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["identity"], serde_json::Value::Null);
    let workspace = &v["workspaces"]["acme-web"];
    assert_eq!(
        (
            workspace["misses"].as_u64(),
            workspace["forbidden"].as_u64()
        ),
        (Some(1), Some(1))
    );
    assert_eq!(workspace["hits"], 0);
}

#[test]
fn status_without_a_running_gateway() {
    let out = cmd(&temp_dir(), Some(free_port()), "status")
        .output()
        .unwrap();
    assert_ne!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).contains("Gateway stopped"));
}

#[test]
fn status_reads_health_then_stats() {
    let gw = start();
    http(
        gw.port,
        "GET",
        "/acme-web/v1/cache/abc",
        Some(&gw.token()),
        b"",
    );

    let out = cmd(&gw.dir, Some(gw.port), "status").output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("unusable (account missing from the config)"),
        "{text}"
    );
    assert!(text.contains("acme-web"), "{text}");

    let out = cmd(&gw.dir, Some(gw.port), "status")
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["health"]["service"], "nx-azure-cache");
    assert_eq!(v["health"]["write"], "unknown");
    assert_eq!(v["stats"]["workspaces"]["acme-web"]["misses"], 1);
}

#[test]
fn log_file_created_without_secrets() {
    let dir = temp_dir();
    let port = free_port();
    let mut child = serve_cmd(&dir, Some(port))
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    wait_health(port);
    let token = std::fs::read_to_string(dir.join("nx-azure-cache/local-token")).unwrap();
    // Fake Entra token in JWT format, sent in a header and in the query string (signed URL).
    let entra = "eyJ0eXAiOiJKV1QiLCJhbGciOiJSUzI1NiJ9.eyJhdWQiOiJzdG9yYWdlIn0.c2lnbmF0dXJl";
    for secret in [token.as_str(), entra] {
        let signed = format!("/acme-web/v1/cache/abc?sig={secret}");
        http(port, "GET", &signed, Some(secret), b"");
        http(port, "PUT", "/acme-web/v1/cache/abc", Some(secret), b"tar");
        http(port, "GET", "/stats", Some(secret), b"");
        http(port, "GET", &format!("/{secret}"), Some(secret), b"");
    }
    let _ = child.kill();
    let _ = child.wait();
    let mut stdout = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap();

    let logs: Vec<PathBuf> = std::fs::read_dir(dir.join("nx-azure-cache/logs"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(logs.len(), 1, "{logs:?}");
    let file = std::fs::read_to_string(&logs[0]).unwrap();
    assert!(file.contains("PUT workspace=\"acme-web\""), "{file}");
    for (name, out) in [("stdout", &stdout), ("file", &file)] {
        assert!(!out.contains(&token), "local token in {name}");
        assert!(!out.contains(entra), "Entra token in {name}");
        assert!(!out.contains("eyJ"), "piece of JWT in {name}");
    }
}

#[test]
fn closed_stdout_gateway_keeps_serving() {
    // Dead headless conhost, closed pipe: writing to stdout or stderr fails (EPIPE).
    let port = free_port();
    let mut child = serve_cmd(&temp_dir(), Some(port))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    drop(child.stderr.take());
    wait_health(port);
    for _ in 0..3 {
        assert_eq!(http(port, "GET", "/health", None, b"").0, 200);
    }
    let _ = child.kill();
    let _ = child.wait();
}
