//! Nx cache routes: `GET` and `PUT /{workspace}/v1/cache/{hash}`.
//!
//! Golden rule: only `200`, `404`, `403` and `409` ever reach Nx. This handler is the
//! router fallback, so any unknown path follows the same rule (`404` on read, `403` on
//! write) instead of a status that would be fatal.

use crate::access_log::CacheLog;
use crate::server::AppState;
use crate::store::{Download, Store, StoreError};
use axum::{
    body::{Body, Bytes},
    extract::{Request, State},
    http::{Method, StatusCode, header::CONTENT_LENGTH},
    response::{IntoResponse, Response},
};
use futures_util::{StreamExt, stream::BoxStream};
use std::sync::{Arc, atomic::Ordering};
use std::time::Duration;
use tokio::sync::OwnedSemaphorePermit;
use tokio::task::JoinSet;
use tokio::time::timeout;

/// Delay before the first byte of a GET.
const FIRST_BYTE: Duration = Duration::from_secs(10);
/// Idle time tolerated mid-stream, and maximum duration of a write call.
const IDLE: Duration = Duration::from_secs(30);
/// Resumes of a GET cut mid-stream.
const RESUMES: u32 = 3;
/// Block size (Put Block): bounds the memory of a PUT, whatever the Entry.
pub const BLOCK: usize = 4 * 1024 * 1024;
/// Put Block calls in flight per PUT; also the cap on blocks in memory, including the block
/// being filled.
const IN_FLIGHT: usize = 4;

/// Write state, read by `/health.write`.
pub const WRITE_UNKNOWN: u8 = 0;
pub const WRITE_ALLOWED: u8 = 1;
pub const WRITE_DENIED: u8 = 2;

pub fn write_label(state: &AppState) -> &'static str {
    match state.write.load(Ordering::Relaxed) {
        WRITE_ALLOWED => "allowed",
        WRITE_DENIED => "denied",
        _ => "unknown",
    }
}

pub async fn handler(State(state): State<Arc<AppState>>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let (workspace, hash) = split_path(parts.uri.path());
    let token_ok = state.token.matches_header(&parts.headers);
    let refused = refusal(token_ok, workspace, hash);
    let blob = format!("{workspace}/{hash}");

    if parts.method == Method::PUT {
        let (status, outcome, detail, bytes) = put(&state, refused, &blob, body).await;
        return reply(status, workspace, hash, outcome, detail, bytes);
    }

    // Read (and any other method): anything that is not a hit is a miss.
    if let Some(detail) = refused {
        return reply(
            StatusCode::NOT_FOUND,
            workspace,
            hash,
            "denied",
            Some(detail.into()),
            0,
        );
    }
    let Some(store) = state.store() else {
        let detail = Some("no storage".into());
        return reply(StatusCode::NOT_FOUND, workspace, hash, "miss", detail, 0);
    };
    // Waiting for the permit counts toward the first-byte delay. The permit is held for the
    // whole relay, resumes included.
    let first = timeout(FIRST_BYTE, async {
        let permit = state.azure.clone().acquire_owned().await.unwrap();
        store.get(&blob, 0, None).await.map(|dl| (dl, permit))
    })
    .await
    .unwrap_or_else(|_| Err(StoreError::Other("timeout".into())));
    match first {
        Ok((dl, permit)) => {
            let len = dl.len;
            let label = format!(
                "workspace={workspace:?} hash={:?}",
                &hash[..hash.len().min(12)]
            );
            let body = Body::from_stream(relay(store, blob, label, dl, permit));
            let mut res = (StatusCode::OK, [(CONTENT_LENGTH, len)], body).into_response();
            res.extensions_mut()
                .insert(log(workspace, hash, "hit", None, len));
            res
        }
        Err(StoreError::NotFound) => reply(StatusCode::NOT_FOUND, workspace, hash, "miss", None, 0),
        Err(e) => reply(
            StatusCode::NOT_FOUND,
            workspace,
            hash,
            "error",
            Some(e.code()),
            0,
        ),
    }
}

/// Streaming relay of a GET: on a cut or idle stream, resumes with `Range` from the byte
/// reached, with `If-Match` on the ETag. Resumes exhausted: the stream fails and the
/// connection is dropped, the only case where Nx sees an error (known limit).
fn relay(
    store: Arc<dyn Store>,
    blob: String,
    label: String,
    first: Download,
    permit: OwnedSemaphorePermit,
) -> BoxStream<'static, std::io::Result<Bytes>> {
    struct Relay {
        store: Arc<dyn Store>,
        blob: String,
        label: String,
        etag: Option<String>,
        total: u64,
        sent: u64,
        resumes: u32,
        body: Option<BoxStream<'static, Result<Bytes, StoreError>>>,
        _permit: OwnedSemaphorePermit,
    }
    let state = Relay {
        store,
        blob,
        label,
        etag: first.etag,
        total: first.len,
        sent: 0,
        resumes: 0,
        body: Some(first.body),
        _permit: permit,
    };
    futures_util::stream::unfold(state, |mut s| async move {
        let mut body = s.body.take()?; // `None`: stream finished or abandoned.
        loop {
            let mut cause = match timeout(IDLE, body.next()).await {
                Ok(Some(Ok(chunk))) => {
                    s.sent += chunk.len() as u64;
                    s.body = Some(body);
                    return Some((Ok(chunk), s));
                }
                Ok(None) if s.sent >= s.total => return None,
                Ok(None) => "truncated body".to_owned(),
                Ok(Some(Err(e))) => e.code(),
                Err(_) => "idle".to_owned(),
            };
            let mut resumed = None;
            while resumed.is_none() && s.resumes < RESUMES {
                s.resumes += 1;
                crate::journal::line(&format!(
                    "GET {} resume {}/{RESUMES} at {}B: {cause}",
                    s.label, s.resumes, s.sent
                ));
                let call = s.store.get(&s.blob, s.sent, s.etag.clone());
                match timeout(FIRST_BYTE, call).await {
                    Ok(Ok(dl)) => resumed = Some(dl.body),
                    // Blob rewritten since the GET started: retrying would be pointless.
                    Ok(Err(e @ StoreError::Changed)) => {
                        cause = e.code();
                        break;
                    }
                    Ok(Err(e)) => cause = e.code(),
                    Err(_) => cause = "timeout".into(),
                }
            }
            match resumed {
                Some(next) => body = next,
                None => {
                    crate::journal::line(&format!(
                        "GET {} cut at {}B of {}B: {cause}",
                        s.label, s.sent, s.total
                    ));
                    return Some((Err(std::io::Error::other("resumes exhausted")), s));
                }
            }
        }
    })
    .boxed()
}

/// Write: the body is always read to the end before replying, even to refuse, because an
/// early response breaks the upload on the Nx side (6 attempts, then fatal).
async fn put(
    state: &Arc<AppState>,
    refused: Option<&'static str>,
    blob: &str,
    body: Body,
) -> (StatusCode, &'static str, Option<String>, u64) {
    let mut stream = body.into_data_stream();
    let store = match (state.store(), refused) {
        (_, Some(detail)) => Err(detail),
        (None, None) => Err("no storage"),
        // Reader Identity already detected: refused without an Azure call.
        _ if state.write.load(Ordering::Relaxed) == WRITE_DENIED => Err("write denied"),
        (Some(store), None) => Ok(store),
    };
    let store = &match store {
        Ok(store) => store,
        Err(detail) => {
            let bytes = drain(stream).await;
            return (StatusCode::FORBIDDEN, "denied", Some(detail.into()), bytes);
        }
    };

    // ponytail: no blob existence check before uploading: it would cost a round trip per
    // PUT for a rare case (a single parallel CI job); the 409 at commit is enough.

    // Random prefix per request: two concurrent PUTs of the same hash do not mix their
    // blocks. All ids of a blob have the same length.
    let mut prefix = [0u8; 16];
    let _ = getrandom::fill(&mut prefix);
    let mut ids: Vec<Vec<u8>> = Vec::new();
    let mut in_flight = JoinSet::new();
    let mut buf: Vec<u8> = Vec::with_capacity(BLOCK);
    let mut total = 0u64;
    let mut failure: Option<StoreError> = None;

    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else {
            failure.get_or_insert(StoreError::Other("body interrupted".into()));
            break;
        };
        total += chunk.len() as u64;
        if failure.is_some() {
            continue; // Read the rest without uploading anything more.
        }
        let mut rest = &chunk[..];
        while !rest.is_empty() && failure.is_none() {
            let n = (BLOCK - buf.len()).min(rest.len());
            buf.extend_from_slice(&rest[..n]);
            rest = &rest[n..];
            if buf.len() == BLOCK {
                let block = std::mem::take(&mut buf);
                let staged = stage(state, store, blob, &prefix, &mut ids, &mut in_flight, block);
                failure = staged.await.err();
                // Allocated once a slot is free: at most `IN_FLIGHT` blocks in memory.
                buf.reserve_exact(BLOCK);
            }
        }
    }
    let result = match failure {
        Some(e) => Err(e),
        None if !buf.is_empty() => {
            stage(state, store, blob, &prefix, &mut ids, &mut in_flight, buf).await
        }
        None => Ok(()),
    };
    // All blocks uploaded before the commit. On failure, dropping `in_flight` cancels the
    // remaining uploads.
    let result = match result {
        Ok(()) => settle(&mut in_flight, 0).await,
        Err(e) => Err(e),
    };
    let result = match result {
        Ok(()) => azure_call(state, store.commit(blob, ids)).await,
        Err(e) => Err(e),
    };

    match result {
        Ok(()) => {
            state.write.store(WRITE_ALLOWED, Ordering::Relaxed);
            (StatusCode::OK, "stored", None, total)
        }
        Err(StoreError::Exists) => (StatusCode::CONFLICT, "exists", None, total),
        Err(StoreError::Denied(code)) => {
            state.write.store(WRITE_DENIED, Ordering::Relaxed);
            (StatusCode::FORBIDDEN, "denied", Some(code), total)
        }
        // Network error, token, timeout…: write abandoned, write state unchanged.
        Err(e) => (StatusCode::FORBIDDEN, "error", Some(e.code()), total),
    }
}

type InFlight = JoinSet<Result<(), StoreError>>;

/// Starts uploading `block` in the background, then waits until fewer than `IN_FLIGHT`
/// uploads are in progress. Returns the first error of a finished upload.
async fn stage(
    state: &Arc<AppState>,
    store: &Arc<dyn Store>,
    blob: &str,
    prefix: &[u8],
    ids: &mut Vec<Vec<u8>>,
    in_flight: &mut InFlight,
    block: Vec<u8>,
) -> Result<(), StoreError> {
    let mut id = prefix.to_vec();
    id.extend_from_slice(&(ids.len() as u32).to_be_bytes());
    ids.push(id.clone());
    let (state, store, blob) = (state.clone(), store.clone(), blob.to_owned());
    in_flight.spawn(async move {
        azure_call(&state, store.put_block(&blob, id, Bytes::from(block))).await
    });
    settle(in_flight, IN_FLIGHT - 1).await
}

/// Waits until only `left` uploads are in progress.
async fn settle(in_flight: &mut InFlight, left: usize) -> Result<(), StoreError> {
    while in_flight.len() > left {
        let Some(done) = in_flight.join_next().await else {
            break;
        };
        done.unwrap_or_else(|_| Err(StoreError::Other("upload interrupted".into())))?;
    }
    Ok(())
}

/// A write call: under the Azure call semaphore, time-bounded, waiting for the permit
/// included.
async fn azure_call(
    state: &AppState,
    call: impl Future<Output = Result<(), StoreError>>,
) -> Result<(), StoreError> {
    let call = async {
        let _permit = state.azure.acquire().await.unwrap();
        call.await
    };
    timeout(IDLE, call)
        .await
        .unwrap_or_else(|_| Err(StoreError::Other("timeout".into())))
}

/// Reason for refusing a cache request, or `None` if it is acceptable.
fn refusal(token_ok: bool, workspace: &str, hash: &str) -> Option<&'static str> {
    if !token_ok {
        Some("token")
    } else if !valid_workspace(workspace) || !valid_hash(hash) {
        Some("format")
    } else {
        None
    }
}

fn log(
    workspace: &str,
    hash: &str,
    outcome: &'static str,
    detail: Option<String>,
    bytes: u64,
) -> CacheLog {
    CacheLog {
        workspace: workspace.to_owned(),
        hash: hash.to_owned(),
        outcome,
        detail,
        bytes,
    }
}

fn reply(
    status: StatusCode,
    workspace: &str,
    hash: &str,
    outcome: &'static str,
    detail: Option<String>,
    bytes: u64,
) -> Response {
    let mut res = status.into_response();
    res.extensions_mut()
        .insert(log(workspace, hash, outcome, detail, bytes));
    res
}

/// Consumes the body without keeping it in memory; returns the number of bytes read.
async fn drain(mut stream: axum::body::BodyDataStream) -> u64 {
    let mut total = 0u64;
    while let Some(Ok(chunk)) = stream.next().await {
        total += chunk.len() as u64;
    }
    total
}

/// Splits `/{workspace}/v1/cache/{hash}`; a path of any other shape gives an empty pair,
/// hence an invalid format.
fn split_path(path: &str) -> (&str, &str) {
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    match segments.as_slice() {
        [workspace, "v1", "cache", hash] => (workspace, hash),
        _ => ("", ""),
    }
}

/// `^[a-z0-9][a-z0-9-]{0,62}$`
pub fn valid_workspace(workspace: &str) -> bool {
    let b = workspace.as_bytes();
    (1..=63).contains(&b.len())
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// `^[A-Za-z0-9]{1,128}$`
fn valid_hash(hash: &str) -> bool {
    (1..=128).contains(&hash.len()) && hash.bytes().all(|c| c.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Config, token::LocalToken};
    use async_trait::async_trait;
    use axum::http::header::AUTHORIZATION;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, AtomicUsize};

    #[test]
    fn formats() {
        assert!(
            valid_workspace("acme-web") && valid_workspace("0") && valid_workspace(&"a".repeat(63))
        );
        assert!(
            !valid_workspace("")
                && !valid_workspace("-a")
                && !valid_workspace("Abc")
                && !valid_workspace("a_b")
        );
        assert!(!valid_workspace(&"a".repeat(64)));
        assert!(valid_hash("aZ09") && valid_hash(&"a".repeat(128)));
        assert!(
            !valid_hash("")
                && !valid_hash(&"a".repeat(129))
                && !valid_hash("a-b")
                && !valid_hash("a%2Fb")
        );
        assert_eq!(split_path("/d/v1/cache/h"), ("d", "h"));
        assert_eq!(split_path("/v1/cache/h"), ("", ""));
        assert_eq!(split_path("/d/v1/cache/h/x"), ("", ""));
    }

    /// Fake in-memory storage, with failures on demand.
    #[derive(Default)]
    struct Fake {
        blobs: Mutex<HashMap<String, Vec<u8>>>,
        blocks: Mutex<HashMap<Vec<u8>, Bytes>>,
        /// Total number of storage calls.
        calls: AtomicUsize,
        /// `(from, etag)` of each GET.
        gets: Mutex<Vec<(u64, Option<String>)>>,
        /// Error returned by every Put Block.
        put_error: Option<StoreError>,
        /// The first GET is cut after this many bytes.
        cut_at: Option<usize>,
        /// GET that never answers.
        hang: bool,
        /// Blob rewritten: a GET conditioned on the ETag fails.
        changed: bool,
        /// Does not keep the blocks (memory test).
        discard: bool,
        max_block: AtomicUsize,
        /// Put Block calls in progress, and their maximum.
        in_flight: AtomicUsize,
        max_in_flight: AtomicUsize,
    }

    #[async_trait]
    impl Store for Fake {
        async fn get(
            &self,
            blob: &str,
            from: u64,
            etag: Option<String>,
        ) -> Result<Download, StoreError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let first = {
                let mut gets = self.gets.lock().unwrap();
                gets.push((from, etag.clone()));
                gets.len() == 1
            };
            if self.changed && etag.is_some() {
                return Err(StoreError::Changed);
            }
            if self.hang {
                std::future::pending::<()>().await;
            }
            let data = self.blobs.lock().unwrap().get(blob).cloned();
            let rest = data.ok_or(StoreError::NotFound)?[from as usize..].to_vec();
            let len = rest.len() as u64;
            let chunks = match self.cut_at {
                Some(cut) if first => vec![
                    Ok(Bytes::copy_from_slice(&rest[..cut])),
                    Err(StoreError::Other("cut".into())),
                ],
                _ => vec![Ok(Bytes::from(rest))],
            };
            let body = futures_util::stream::iter(chunks).boxed();
            Ok(Download {
                etag: Some("e1".into()),
                len,
                body,
            })
        }

        async fn put_block(&self, _blob: &str, id: Vec<u8>, data: Bytes) -> Result<(), StoreError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(e) = &self.put_error {
                return Err(e.clone());
            }
            self.max_block.fetch_max(data.len(), Ordering::SeqCst);
            let n = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_in_flight.fetch_max(n, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(2)).await; // network round trip
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            if !self.discard {
                self.blocks.lock().unwrap().insert(id, data);
            }
            Ok(())
        }

        async fn commit(&self, blob: &str, ids: Vec<Vec<u8>>) -> Result<(), StoreError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut blobs = self.blobs.lock().unwrap();
            if blobs.contains_key(blob) {
                return Err(StoreError::Exists);
            }
            if !self.discard {
                let blocks = self.blocks.lock().unwrap();
                let data = ids.iter().flat_map(|id| blocks[id].to_vec()).collect();
                blobs.insert(blob.to_owned(), data);
            }
            Ok(())
        }
    }

    fn app(fake: &Arc<Fake>) -> Arc<AppState> {
        let token = LocalToken("a".repeat(64));
        Arc::new(AppState::new(Config::default(), token, Some(fake.clone())))
    }

    async fn call(state: &Arc<AppState>, method: Method, body: Body) -> (u16, Vec<u8>) {
        let put = method == Method::PUT;
        let req = Request::builder()
            .method(method)
            .uri("/ws/v1/cache/abc")
            .header(AUTHORIZATION, format!("Bearer {}", "a".repeat(64)))
            .body(body)
            .unwrap();
        let res = handler(State(state.clone()), req).await;
        let status = res.status().as_u16();
        // Same counting as the log middleware.
        let log = res.extensions().get::<CacheLog>().unwrap();
        crate::stats::record(&state.stats, put, status, log);
        let body = axum::body::to_bytes(res.into_body(), usize::MAX).await;
        (status, body.unwrap().to_vec())
    }

    /// PUT body that counts the bytes actually read by the Gateway.
    fn counted(data: &[u8]) -> (Body, Arc<AtomicU64>) {
        let read = Arc::new(AtomicU64::new(0));
        let r = read.clone();
        let chunks: Vec<Bytes> = data.chunks(1000).map(Bytes::copy_from_slice).collect();
        let stream = futures_util::stream::iter(chunks).map(move |c| {
            r.fetch_add(c.len() as u64, Ordering::SeqCst);
            Ok::<_, std::io::Error>(c)
        });
        (Body::from_stream(stream), read)
    }

    fn entry() -> Vec<u8> {
        (0..10_000u32).map(|i| i as u8).collect()
    }

    #[tokio::test]
    async fn miss_write_hit_then_duplicate() {
        let fake = Arc::new(Fake::default());
        let state = app(&fake);
        assert_eq!(call(&state, Method::GET, Body::empty()).await.0, 404);

        let (body, read) = counted(&entry());
        assert_eq!(call(&state, Method::PUT, body).await.0, 200);
        assert_eq!(read.load(Ordering::SeqCst), 10_000);
        assert_eq!(write_label(&state), "allowed");

        assert_eq!(
            call(&state, Method::GET, Body::empty()).await,
            (200, entry())
        );

        let (body, read) = counted(&entry());
        assert_eq!(call(&state, Method::PUT, body).await.0, 409);
        assert_eq!(read.load(Ordering::SeqCst), 10_000);
    }

    #[tokio::test]
    async fn counters_hit_miss_write_409_403() {
        let fake = Arc::new(Fake::default());
        let state = app(&fake);
        assert_eq!(call(&state, Method::GET, Body::empty()).await.0, 404);
        assert_eq!(call(&state, Method::PUT, Body::from(entry())).await.0, 200);
        assert_eq!(call(&state, Method::GET, Body::empty()).await.0, 200);
        assert_eq!(call(&state, Method::PUT, Body::from(entry())).await.0, 409);
        // Reader Identity already detected: 403 without an Azure call.
        state.write.store(WRITE_DENIED, Ordering::Relaxed);
        assert_eq!(call(&state, Method::PUT, Body::from(entry())).await.0, 403);

        let c = state.stats.lock().unwrap()["ws"].clone();
        let got = [
            c.hits,
            c.misses,
            c.writes,
            c.conflicts,
            c.forbidden,
            c.azure_errors,
        ];
        assert_eq!(got, [1, 1, 1, 1, 1, 0]);
        assert_eq!((c.bytes_read, c.bytes_written), (10_000, 10_000));

        // Azure error on write: one 403 and one Azure error.
        let fake = Arc::new(Fake {
            put_error: Some(StoreError::Other("Connection".into())),
            ..Default::default()
        });
        let state = app(&fake);
        assert_eq!(call(&state, Method::PUT, Body::from(entry())).await.0, 403);
        let c = state.stats.lock().unwrap()["ws"].clone();
        assert_eq!((c.forbidden, c.azure_errors, c.bytes_written), (1, 1, 0));
    }

    #[tokio::test]
    async fn authorization_refusal_then_put_without_storage_call() {
        let fake = Arc::new(Fake {
            put_error: Some(StoreError::Denied("AuthorizationPermissionMismatch".into())),
            ..Default::default()
        });
        let state = app(&fake);
        let (body, read) = counted(&entry());
        assert_eq!(call(&state, Method::PUT, body).await.0, 403);
        assert_eq!(read.load(Ordering::SeqCst), 10_000);
        assert_eq!(write_label(&state), "denied");

        let before = fake.calls.load(Ordering::SeqCst);
        let (body, read) = counted(&entry());
        assert_eq!(call(&state, Method::PUT, body).await.0, 403);
        assert_eq!(read.load(Ordering::SeqCst), 10_000);
        assert_eq!(fake.calls.load(Ordering::SeqCst), before);
    }

    #[tokio::test]
    async fn network_error_on_write_does_not_switch_to_denied() {
        let fake = Arc::new(Fake {
            put_error: Some(StoreError::Other("Connection".into())),
            ..Default::default()
        });
        let state = app(&fake);
        let (body, read) = counted(&entry());
        assert_eq!(call(&state, Method::PUT, body).await.0, 403);
        assert_eq!(read.load(Ordering::SeqCst), 10_000);
        assert_eq!(write_label(&state), "unknown");
    }

    #[tokio::test]
    async fn cut_mid_get_resumes_with_range() {
        let fake = Arc::new(Fake {
            cut_at: Some(3000),
            ..Default::default()
        });
        fake.blobs.lock().unwrap().insert("ws/abc".into(), entry());
        let state = app(&fake);
        assert_eq!(
            call(&state, Method::GET, Body::empty()).await,
            (200, entry())
        );
        let gets = fake.gets.lock().unwrap().clone();
        assert_eq!(gets, [(0, None), (3000, Some("e1".into()))]);
    }

    #[tokio::test]
    async fn etag_change_mid_get_stops_resuming() {
        let fake = Arc::new(Fake {
            cut_at: Some(3000),
            changed: true,
            ..Default::default()
        });
        fake.blobs.lock().unwrap().insert("ws/abc".into(), entry());
        let req = Request::builder()
            .uri("/ws/v1/cache/abc")
            .header(AUTHORIZATION, format!("Bearer {}", "a".repeat(64)))
            .body(Body::empty())
            .unwrap();
        let res = handler(State(app(&fake)), req).await;
        assert!(
            axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .is_err()
        );
        // A single resume: retrying on a rewritten blob is pointless.
        let gets = fake.gets.lock().unwrap().clone();
        assert_eq!(gets, [(0, None), (3000, Some("e1".into()))]);
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_before_first_byte_gives_404() {
        let fake = Arc::new(Fake {
            hang: true,
            ..Default::default()
        });
        assert_eq!(call(&app(&fake), Method::GET, Body::empty()).await.0, 404);
    }

    #[tokio::test(start_paused = true)]
    async fn saturated_azure_calls_404_on_get_403_on_put() {
        let fake = Arc::new(Fake::default());
        let state = app(&fake);
        let _all = state.azure.clone().acquire_many_owned(16).await.unwrap();
        let wait = Duration::from_secs(3600);
        let get = timeout(wait, call(&state, Method::GET, Body::empty())).await;
        assert_eq!(get.map(|r| r.0), Ok(404));
        let put = timeout(wait, call(&state, Method::PUT, Body::from(entry()))).await;
        assert_eq!(put.map(|r| r.0), Ok(403));
        assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn put_of_500_mb_in_bounded_blocks() {
        let fake = Arc::new(Fake {
            discard: true,
            ..Default::default()
        });
        let chunk = Bytes::from(vec![7u8; 64 * 1024]);
        // 8000 × 64 KiB = 500 MiB, produced on the fly (clones of a single buffer).
        let stream = futures_util::stream::iter(std::iter::repeat_n(chunk, 8000))
            .map(Ok::<_, std::io::Error>);
        let (status, _) = call(&app(&fake), Method::PUT, Body::from_stream(stream)).await;
        assert_eq!(status, 200);
        // No buffer proportional to the size: 4 MiB blocks, at most 4 in flight.
        assert_eq!(fake.max_block.load(Ordering::SeqCst), BLOCK);
        assert_eq!(fake.max_in_flight.load(Ordering::SeqCst), IN_FLIGHT);
        assert_eq!(fake.calls.load(Ordering::SeqCst), 500 / 4 + 1);
    }

    #[tokio::test]
    async fn put_in_parallel_blocks_keeps_content_in_order() {
        let fake = Arc::new(Fake::default());
        let state = app(&fake);
        let data: Vec<u8> = (0..BLOCK * 10 + 123).map(|i| (i / BLOCK) as u8).collect();
        assert_eq!(
            call(&state, Method::PUT, Body::from(data.clone())).await.0,
            200
        );
        assert_eq!(fake.max_in_flight.load(Ordering::SeqCst), IN_FLIGHT);
        assert!(call(&state, Method::GET, Body::empty()).await == (200, data));
    }
}
