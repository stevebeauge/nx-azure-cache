//! Seam between the HTTP translation (`cache.rs`) and the storage: the `Store` trait and
//! its Azure Blob implementation. Tests plug in a fake in-memory storage.

use async_trait::async_trait;
use axum::body::Bytes;
use azure_core::{
    credentials::TokenCredential,
    error::ErrorKind,
    http::{ClientOptions, Etag, RetryOptions, StatusCode, Url},
};
use azure_storage_blob::{
    BlobContainerClient, BlobContainerClientOptions,
    models::{
        BlobClientDownloadOptions, BlockBlobClientCommitBlockListOptions, BlockLookupList,
        HttpRange, StorageErrorCode,
    },
};
use futures_util::{StreamExt, stream::BoxStream};
use std::{num::NonZero, sync::Arc};

/// Outcome of a storage call, already classified for the translation to Nx.
#[derive(Debug, Clone, PartialEq)]
pub enum StoreError {
    /// Blob not found.
    NotFound,
    /// The blob already exists (conditional commit).
    Exists,
    /// The blob changed since the GET started (`If-Match` of a resume failed).
    Changed,
    /// Authorization refusal: the Identity cannot write. Carries the Azure code.
    Denied(String),
    /// Any other error (network, token, timeout…), with a short code free of secrets.
    Other(String),
}

impl StoreError {
    /// Short code for the log line: never a URL or a token.
    pub fn code(&self) -> String {
        match self {
            StoreError::NotFound => "BlobNotFound".into(),
            StoreError::Exists => "BlobAlreadyExists".into(),
            StoreError::Changed => "ETag changed".into(),
            StoreError::Denied(c) | StoreError::Other(c) => c.clone(),
        }
    }
}

/// Response of a Get Blob: `len` is the length of **this** response.
pub struct Download {
    pub etag: Option<String>,
    pub len: u64,
    pub body: BoxStream<'static, Result<Bytes, StoreError>>,
}

#[async_trait]
pub trait Store: Send + Sync {
    /// Reads `blob` from byte `from`, conditioned on `etag` if given.
    async fn get(
        &self,
        blob: &str,
        from: u64,
        etag: Option<String>,
    ) -> Result<Download, StoreError>;
    /// Uploads an uncommitted block (Put Block).
    async fn put_block(&self, blob: &str, id: Vec<u8>, data: Bytes) -> Result<(), StoreError>;
    /// Commits the block list if the blob does not exist yet (`If-None-Match: *`).
    async fn commit(&self, blob: &str, ids: Vec<Vec<u8>>) -> Result<(), StoreError>;
}

/// Azure Blob storage of the configured container, on behalf of `credential`.
///
/// The client caches the token in its pipeline: an Identity change (login, logout)
/// requires calling this function again, which is cheap (see `identity::start`).
pub fn azure(
    account: &str,
    container: &str,
    credential: Arc<dyn TokenCredential>,
) -> Result<Arc<dyn Store>, String> {
    let url = Url::parse(&format!(
        "https://{account}.blob.core.windows.net/{container}"
    ))
    .map_err(|e| format!("invalid account or container: {e}"))?;
    let options = BlobContainerClientOptions {
        client_options: ClientOptions {
            // No retry in the SDK: it would eat the Gateway timeouts (10 s before the first
            // byte), and the Gateway resumes a GET itself.
            retry: RetryOptions::none(),
            ..Default::default()
        },
        ..Default::default()
    };
    let container = BlobContainerClient::new(url, Some(credential), Some(options))
        .map_err(|e| format!("Blob client: {e}"))?;
    Ok(Arc::new(AzureStore(container)))
}

/// A single container client: its `BlobClient`s share the pipeline and the token cache.
struct AzureStore(BlobContainerClient);

#[async_trait]
impl Store for AzureStore {
    async fn get(
        &self,
        blob: &str,
        from: u64,
        etag: Option<String>,
    ) -> Result<Download, StoreError> {
        let options = BlobClientDownloadOptions {
            range: (from > 0).then(|| HttpRange::from_offset(from)),
            if_match: etag.map(Etag::from),
            // A single streamed GET: by default, the SDK runs ranged GETs in parallel.
            parallel: NonZero::new(1),
            partition_size: NonZero::new(usize::MAX),
            ..Default::default()
        };
        let res = self
            .0
            .blob_client(blob)
            .download(Some(options))
            .await
            .map_err(classify)?;
        let len = res
            .properties
            .content_length
            .ok_or_else(|| StoreError::Other("ContentLengthAbsent".into()))?;
        Ok(Download {
            etag: res.properties.etag.map(|e| e.as_ref().to_owned()),
            len,
            body: res.body.map(|r| r.map_err(classify)).boxed(),
        })
    }

    async fn put_block(&self, blob: &str, id: Vec<u8>, data: Bytes) -> Result<(), StoreError> {
        self.0
            .blob_client(blob)
            .block_blob_client()
            .stage_block(&id, data.len() as u64, data.into(), None)
            .await
            .map(drop)
            .map_err(classify)
    }

    async fn commit(&self, blob: &str, ids: Vec<Vec<u8>>) -> Result<(), StoreError> {
        let list = BlockLookupList {
            latest: Some(ids),
            ..Default::default()
        };
        let options = BlockBlobClientCommitBlockListOptions {
            if_none_match: Some(Etag::from("*")),
            ..Default::default()
        };
        self.0
            .blob_client(blob)
            .block_blob_client()
            .commit_block_list(list.try_into().map_err(classify)?, Some(options))
            .await
            .map(drop)
            .map_err(commit_error)
    }
}

/// Conditional commit (`If-None-Match: *`): its 412 means the Entry already exists.
fn commit_error(e: azure_core::Error) -> StoreError {
    match classify(e) {
        StoreError::Changed => StoreError::Exists,
        e => e,
    }
}

/// Classifies an SDK error without ever reusing its message (which may quote the URL).
fn classify(e: azure_core::Error) -> StoreError {
    match e.kind() {
        ErrorKind::HttpResponse {
            status, error_code, ..
        } => {
            let code = error_code.as_deref().unwrap_or_default();
            let is = |c: StorageErrorCode| code == c.as_ref();
            match *status {
                StatusCode::NotFound => StoreError::NotFound,
                // `If-Match` of a GET resume. The 412 of a commit on an existing blob
                // (documented; 409 observed with the SDK) is remapped by `commit_error`.
                StatusCode::PreconditionFailed => StoreError::Changed,
                StatusCode::Conflict if is(StorageErrorCode::BlobAlreadyExists) => {
                    StoreError::Exists
                }
                // `AuthenticationFailed` (invalid token, clock skew) is also a 403: not a refusal.
                StatusCode::Forbidden
                    if is(StorageErrorCode::AuthorizationPermissionMismatch)
                        || is(StorageErrorCode::AuthorizationFailure) =>
                {
                    StoreError::Denied(code.to_owned())
                }
                s => StoreError::Other(format!("{} {code}", u16::from(s)).trim().to_owned()),
            }
        }
        kind => StoreError::Other(format!("{kind:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http(status: StatusCode, code: &str) -> azure_core::Error {
        let kind = ErrorKind::HttpResponse {
            status,
            error_code: Some(code.into()),
            raw_response: None,
        };
        azure_core::Error::with_message(kind, "https://account/blob?sig=secret")
    }

    #[test]
    fn a_412_is_a_changed_etag_not_an_existing_entry() {
        let changed = classify(http(StatusCode::PreconditionFailed, "ConditionNotMet"));
        assert_eq!(changed, StoreError::Changed);
        assert_eq!(changed.code(), "ETag changed");
        let exists = classify(http(StatusCode::Conflict, "BlobAlreadyExists"));
        assert_eq!(exists, StoreError::Exists);
        // Conditional commit: a 412 still means an Entry already present (409 to Nx).
        assert_eq!(
            commit_error(http(StatusCode::PreconditionFailed, "ConditionNotMet")),
            StoreError::Exists
        );
    }
}
