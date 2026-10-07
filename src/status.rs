//! `status [--json]` command: reads `/health`, then `/stats` with the local token read from
//! disk. Gateway unreachable: "Gateway stopped" and exit code 1.

use crate::config::{Config, config_dir};
use serde_json::{Value, json};
use std::time::Duration;

pub async fn run(as_json: bool) -> i32 {
    let (dir, config) = match config_dir().and_then(|d| Config::load(&d).map(|c| (d, c))) {
        Ok(found) => found,
        Err(e) => {
            eprintln!("nx-azure-cache: {e}");
            return 1;
        }
    };
    let port = config.port;
    let health = match get(port, "/health", None).await {
        Some((200, h)) if h["service"] == "nx-azure-cache" => h,
        _ => {
            println!("Gateway stopped (nothing answers on 127.0.0.1:{port})");
            return 1;
        }
    };
    let stats = match std::fs::read_to_string(dir.join("local-token")) {
        Ok(token) => match get(port, "/stats", Some(token.trim())).await {
            Some((200, s)) => Ok(s),
            Some((401, _)) => Err("local token rejected".to_owned()),
            other => Err(format!("unexpected response: {:?}", other.map(|o| o.0))),
        },
        Err(e) => Err(format!("unreadable local token: {e}")),
    };

    if as_json {
        let stats = stats.unwrap_or(Value::Null);
        println!("{}", json!({ "health": health, "stats": stats }));
        return 0;
    }
    let identity = &health["identity"];
    println!(
        "nx-azure-cache Gateway {} on 127.0.0.1:{port}",
        text(&health["version"])
    );
    if identity["ready"] == true {
        println!("Health:   ready");
    } else {
        println!("Health:   unusable ({})", text(&identity["reason"]));
    }
    let name = stats.as_ref().ok().map(|s| &s["identity"]);
    let name = name.filter(|n| n.is_string()).unwrap_or(&identity["kind"]);
    println!(
        "Identity: {}",
        if name.is_string() {
            text(name)
        } else {
            "none".into()
        }
    );
    println!("Write:    {}", text(&health["write"]));
    match stats {
        Ok(s) => print_table(&s["workspaces"]),
        Err(e) => println!("\nStatistics unavailable: {e}"),
    }
    0
}

fn print_table(workspaces: &Value) {
    let Some(workspaces) = workspaces.as_object().filter(|w| !w.is_empty()) else {
        println!("\nNo cache request since startup.");
        return;
    };
    let width = workspaces.keys().map(|k| k.len()).max().unwrap_or(0).max(9);
    println!(
        "\n{:width$}  {:>6}  {:>6}  {:>6}  {:>5}  {:>5}  {:>12}  {:>10}  {:>10}",
        "Workspace", "hits", "misses", "writes", "409", "403", "Azure errors", "read", "written"
    );
    for (name, c) in workspaces {
        let n = |k: &str| c[k].as_u64().unwrap_or(0);
        println!(
            "{name:width$}  {:>6}  {:>6}  {:>6}  {:>5}  {:>5}  {:>12}  {:>10}  {:>10}",
            n("hits"),
            n("misses"),
            n("writes"),
            n("conflicts"),
            n("forbidden"),
            n("azure_errors"),
            bytes(n("bytes_read")),
            bytes(n("bytes_written")),
        );
    }
}

fn text(v: &Value) -> String {
    v.as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| v.to_string())
}

fn bytes(n: u64) -> String {
    let mut size = n as f64;
    for unit in ["B", "KiB", "MiB", "GiB"] {
        if size < 1024.0 {
            return if unit == "B" {
                format!("{n} B")
            } else {
                format!("{size:.1} {unit}")
            };
        }
        size /= 1024.0;
    }
    format!("{size:.1} TiB")
}

/// `GET` on `127.0.0.1:{port}`: status and JSON body (`null` if it is not JSON); `None` if
/// nothing answers in HTTP within 2 s.
pub async fn get(port: u16, path: &str, token: Option<&str>) -> Option<(u16, Value)> {
    let mut req = local_client()?.get(format!("http://127.0.0.1:{port}{path}"));
    if let Some(token) = token {
        req = req.bearer_auth(token);
    }
    let res = req.send().await.ok()?;
    let status = res.status().as_u16();
    let body = res.bytes().await.ok()?;
    Some((status, serde_json::from_slice(&body).unwrap_or(Value::Null)))
}

/// HTTP client to the local Gateway, 2 s timeout. Plain HTTP on loopback: never through an
/// environment proxy (corporate `HTTP_PROXY`), and no system certificate store (missing
/// from a bare image, it would make `build` fail).
pub fn local_client() -> Option<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .no_proxy()
        .tls_certs_only([])
        .build()
        .ok()
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn chunked_response_read_like_any_other() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let _ = s.read(&mut [0u8; 1024]).await;
            let body = r#"{"service":"nx-azure-cache"}"#;
            let head = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close";
            let res = format!("{head}\r\n\r\n{:x}\r\n{body}\r\n0\r\n\r\n", body.len());
            s.write_all(res.as_bytes()).await.unwrap();
        });
        let (status, body) = super::get(port, "/health", None).await.unwrap();
        assert_eq!(status, 200);
        assert_eq!(body["service"], "nx-azure-cache");
    }

    #[test]
    fn readable_sizes() {
        assert_eq!(super::bytes(0), "0 B");
        assert_eq!(super::bytes(1023), "1023 B");
        assert_eq!(super::bytes(1536), "1.5 KiB");
        assert_eq!(super::bytes(5 * 1024 * 1024 * 1024), "5.0 GiB");
    }
}
