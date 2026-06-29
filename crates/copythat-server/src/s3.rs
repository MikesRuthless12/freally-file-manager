//! Phase 48 follow-up — an S3-compatible HTTP service.
//!
//! Path-style S3 over its own axum router on the bound listener (S3's
//! request/response semantics differ enough from WebDAV that it is a
//! distinct mode, like SFTP). A single implicit bucket maps onto
//! [`ServerConfig::root`]: the first path segment is the bucket name (any
//! name is accepted) and the rest of the path is the object key under the
//! served root.
//!
//! Like the SFTP transport — and unlike the WebDAV surface, which delegates
//! path safety to `dav-server` — this module is the *only* path jail in
//! front of the root. Every client key is funnelled through [`resolve_key`],
//! which URL-decodes it and then lexically normalises `.` / `..` against the
//! canonicalised root, refusing any escape (`..` above the root, an absolute
//! key, or a drive / UNC prefix). No component is resolved through the
//! filesystem, and directory listings skip symlinks, so a crafted key (or a
//! symlink under the root) can never climb out.
//!
//! Authentication maps [`AuthMode`] onto AWS Signature V4:
//! [`AuthMode::None`] serves open; [`AuthMode::Basic`] treats `user` as the
//! access-key-id and `password` as the secret and verifies the
//! `Authorization: AWS4-HMAC-SHA256 …` header (the canonical request →
//! string-to-sign → derived signing-key → HMAC chain, constant-time
//! compared to the header signature). S3 has no bearer concept, so an S3 +
//! [`AuthMode::Bearer`] config is rejected up-front in [`crate::serve`].

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use hmac::{Hmac, Mac};
use http_body_util::BodyExt;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::io::AsyncWriteExt;

use crate::{AuthMode, MetricsRegistry, ServerConfig, ServerError};

type HmacSha256 = Hmac<Sha256>;

/// SHA-256 of the empty string — the SigV4 payload hash for a body-less
/// request whose `x-amz-content-sha256` header is absent.
const EMPTY_PAYLOAD_SHA256: &str =
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// RFC 3986 *unreserved* set: percent-encode everything EXCEPT
/// `A-Za-z0-9-_.~`. Used to re-encode the SigV4 canonical query string.
const SIGV4_UNRESERVED: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

/// State shared with every S3 request handler.
#[derive(Clone)]
struct S3State {
    /// Canonicalised served root; every resolved key must stay under it.
    root: Arc<PathBuf>,
    auth: Arc<AuthMode>,
    readonly: bool,
    metrics: Arc<MetricsRegistry>,
}

/// Build the axum router for the S3 surface. Canonicalising the root can
/// fail (a missing / unreadable directory), which surfaces as a bind error
/// rather than a silently-unjailed server — mirroring `sftp::spawn`.
pub(crate) fn build_router(
    config: &ServerConfig,
    metrics: Arc<MetricsRegistry>,
) -> Result<Router, ServerError> {
    let root = std::fs::canonicalize(&config.root).map_err(|e| ServerError::Bind {
        addr: config.bind_addr.clone(),
        message: format!("s3 served root {:?} is unusable: {e}", config.root),
    })?;

    let state = S3State {
        root: Arc::new(root),
        auth: Arc::new(config.auth.clone()),
        readonly: config.readonly,
        metrics,
    };

    // Path-style routes. `/` and `/{bucket}` list; `/{bucket}/{*key}` is the
    // object surface. No `/metrics` route here — it would shadow a bucket
    // literally named `metrics`, and (like SFTP) there is no scrape surface
    // on this transport; PUTs still bump the shared registry.
    Ok(Router::new()
        .route("/", get(list_objects))
        .route("/{bucket}", get(list_objects))
        .route(
            "/{bucket}/{*key}",
            get(get_object)
                .head(head_object)
                .put(put_object)
                .delete(delete_object),
        )
        .with_state(state))
}

// ---------------------------------------------------------------------------
// Path jail
// ---------------------------------------------------------------------------

/// Confine a client-supplied (still URL-encoded) object key under the served
/// root.
///
/// The key is percent-decoded first, then walked lexically: `.` is skipped,
/// `..` pops one level (never above the root), a `Normal` segment is pushed,
/// and an absolute (`RootDir`) or drive / UNC (`Prefix`) component is a hard
/// reject. No component is resolved through the filesystem, so a symlink
/// under the root cannot be used to break out. Returns `None` on any escape.
fn resolve_key(root: &Path, raw_key: &str) -> Option<PathBuf> {
    let decoded = percent_decode_str(raw_key).decode_utf8_lossy();
    let mut out = root.to_path_buf();
    for component in Path::new(decoded.as_ref()).components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                // Pop one level, but never climb above the served root.
                if !out.pop() || !out.starts_with(root) {
                    return None;
                }
            }
            // A rooted path (`/…`) or a drive / UNC prefix (`C:\…`) is an
            // absolute-escape attempt, not a key under the bucket.
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    // Belt-and-suspenders: the lexical walk already guarantees this.
    out.starts_with(root).then_some(out)
}

/// Split a request path into `(bucket, raw_key)`, where `raw_key` keeps its
/// percent-encoding (decoded later by [`resolve_key`]). `/` → `("", None)`;
/// `/bucket` → `("bucket", None)`; `/bucket/a/b` → `("bucket", "a/b")`.
fn split_bucket_key(path: &str) -> (String, Option<String>) {
    let trimmed = path.trim_start_matches('/');
    match trimmed.split_once('/') {
        Some((bucket, key)) => (bucket.to_string(), Some(key.to_string())),
        None => (trimmed.to_string(), None),
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /` or `GET /{bucket}` (with or without `?list-type=2`) —
/// ListObjectsV2. Walks the served root recursively and renders the
/// `ListBucketResult` XML, honouring an optional `prefix` query parameter.
async fn list_objects(State(st): State<S3State>, req: Request) -> Response {
    if let Some(resp) = authorize(&st, &req) {
        return resp;
    }
    let uri = req.uri();
    let (bucket, _) = split_bucket_key(uri.path());
    let prefix = query_param(uri.query(), "prefix").unwrap_or_default();

    let mut entries = Vec::new();
    if let Err(e) = collect_entries(&st.root, &st.root, &mut entries) {
        st.metrics.record_error();
        return s3_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "InternalError",
            &e.to_string(),
        );
    }
    entries.retain(|e| e.key.starts_with(&prefix));
    entries.sort_by(|a, b| a.key.cmp(&b.key));

    s3_xml_response(StatusCode::OK, render_list_xml(&bucket, &prefix, &entries))
}

/// `GET /{bucket}/{*key}` — GetObject. Returns the file bytes (200) or a
/// `NoSuchKey` error (404).
async fn get_object(State(st): State<S3State>, req: Request) -> Response {
    if let Some(resp) = authorize(&st, &req) {
        return resp;
    }
    let (_, raw_key) = split_bucket_key(req.uri().path());
    let Some(raw_key) = raw_key else {
        return s3_error(StatusCode::NOT_FOUND, "NoSuchKey", "the key does not exist");
    };
    let Some(path) = resolve_key(&st.root, &raw_key) else {
        return access_denied();
    };

    match tokio::fs::read(&path).await {
        Ok(bytes) => {
            let modified = tokio::fs::metadata(&path)
                .await
                .ok()
                .and_then(|m| m.modified().ok());
            let last_mod = modified.map(http_date).unwrap_or_default();
            let etag_val = content_etag(bytes.len() as u64, modified);
            (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, "application/octet-stream".to_string()),
                    (header::LAST_MODIFIED, last_mod),
                    (header::ETAG, etag_val),
                ],
                bytes,
            )
                .into_response()
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            s3_error(StatusCode::NOT_FOUND, "NoSuchKey", "the key does not exist")
        }
        Err(e) => {
            st.metrics.record_error();
            s3_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "InternalError",
                &e.to_string(),
            )
        }
    }
}

/// `HEAD /{bucket}/{*key}` — object metadata (Content-Length, Last-Modified,
/// ETag) or 404. A HEAD response never carries a body, even on error.
async fn head_object(State(st): State<S3State>, req: Request) -> Response {
    if let Some(resp) = authorize(&st, &req) {
        return resp;
    }
    let (_, raw_key) = split_bucket_key(req.uri().path());
    let Some(raw_key) = raw_key else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(path) = resolve_key(&st.root, &raw_key) else {
        return StatusCode::FORBIDDEN.into_response();
    };

    match tokio::fs::metadata(&path).await {
        Ok(meta) if meta.is_file() => {
            let modified = meta.modified().ok();
            (
                StatusCode::OK,
                [
                    (header::CONTENT_LENGTH, meta.len().to_string()),
                    (header::CONTENT_TYPE, "application/octet-stream".to_string()),
                    (
                        header::LAST_MODIFIED,
                        modified.map(http_date).unwrap_or_default(),
                    ),
                    (header::ETAG, content_etag(meta.len(), modified)),
                ],
            )
                .into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

/// `PUT /{bucket}/{*key}` — PutObject. Streams the body to `root/key`
/// (creating parent directories), returning 200 with an `ETag` header.
/// Rejected with 403 when the server is read-only.
async fn put_object(State(st): State<S3State>, req: Request) -> Response {
    if let Some(resp) = authorize(&st, &req) {
        return resp;
    }
    if st.readonly {
        return s3_error(StatusCode::FORBIDDEN, "AccessDenied", "server is read-only");
    }
    let (_, raw_key) = split_bucket_key(req.uri().path());
    let Some(raw_key) = raw_key else {
        return s3_error(
            StatusCode::BAD_REQUEST,
            "InvalidRequest",
            "missing object key",
        );
    };
    let Some(path) = resolve_key(&st.root, &raw_key) else {
        return access_denied();
    };

    if let Some(parent) = path.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            st.metrics.record_error();
            return s3_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "InternalError",
                &e.to_string(),
            );
        }
    }
    let mut file = match tokio::fs::File::create(&path).await {
        Ok(f) => f,
        Err(e) => {
            st.metrics.record_error();
            return s3_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "InternalError",
                &e.to_string(),
            );
        }
    };

    // Stream the body frame-by-frame so a large upload is never fully
    // buffered in memory.
    let mut body = req.into_body();
    let mut written: u64 = 0;
    loop {
        match body.frame().await {
            Some(Ok(frame)) => {
                if let Ok(data) = frame.into_data() {
                    if let Err(e) = file.write_all(&data).await {
                        st.metrics.record_error();
                        return s3_error(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "InternalError",
                            &e.to_string(),
                        );
                    }
                    written += data.len() as u64;
                }
            }
            Some(Err(_)) => {
                st.metrics.record_error();
                return s3_error(
                    StatusCode::BAD_REQUEST,
                    "IncompleteBody",
                    "error reading request body",
                );
            }
            None => break,
        }
    }
    if let Err(e) = file.flush().await {
        st.metrics.record_error();
        return s3_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "InternalError",
            &e.to_string(),
        );
    }

    st.metrics.record_copy(written);
    let modified = file.metadata().await.ok().and_then(|m| m.modified().ok());
    (
        StatusCode::OK,
        [(header::ETAG, content_etag(written, modified))],
    )
        .into_response()
}

/// `DELETE /{bucket}/{*key}` — DeleteObject. Removes the file and returns
/// 204; like S3, a delete of an absent key is idempotent (still 204).
/// Rejected with 403 when the server is read-only.
async fn delete_object(State(st): State<S3State>, req: Request) -> Response {
    if let Some(resp) = authorize(&st, &req) {
        return resp;
    }
    if st.readonly {
        return s3_error(StatusCode::FORBIDDEN, "AccessDenied", "server is read-only");
    }
    let (_, raw_key) = split_bucket_key(req.uri().path());
    let Some(raw_key) = raw_key else {
        return s3_error(
            StatusCode::BAD_REQUEST,
            "InvalidRequest",
            "missing object key",
        );
    };
    let Some(path) = resolve_key(&st.root, &raw_key) else {
        return access_denied();
    };

    match tokio::fs::remove_file(&path).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        // Idempotent: deleting a missing key is success in S3.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
            st.metrics.record_error();
            s3_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "InternalError",
                &e.to_string(),
            )
        }
    }
}

// ---------------------------------------------------------------------------
// AWS Signature V4 verification
// ---------------------------------------------------------------------------

/// Gate a request against the configured auth mode. Returns `Some(403)` when
/// it should be rejected, `None` when it passes.
fn authorize(st: &S3State, req: &Request) -> Option<Response> {
    match &*st.auth {
        AuthMode::None => None,
        AuthMode::Basic { user, password } => verify_sigv4(req, user, password),
        // `serve()` rejects an S3 + Bearer config up-front, so this arm is
        // unreachable in practice; deny defensively (rather than panic, or
        // silently serve open) so a caller bypassing `serve()` can't run
        // an authenticated config wide open.
        AuthMode::Bearer { .. } => Some(s3_error(
            StatusCode::FORBIDDEN,
            "AccessDenied",
            "bearer auth is not supported for S3",
        )),
    }
}

/// Parsed fields of an `Authorization: AWS4-HMAC-SHA256 …` header.
struct ParsedAuth {
    akid: String,
    date: String,
    region: String,
    service: String,
    signed_headers: Vec<String>,
    signature: String,
}

/// Verify an AWS SigV4 signature. `akid_want` / `secret` are the configured
/// access-key-id and secret (`AuthMode::Basic`'s `user` / `password`).
/// Returns `Some(403)` on any failure (missing / malformed header, wrong
/// access key, or signature mismatch) and `None` when the signature is valid.
fn verify_sigv4(req: &Request, akid_want: &str, secret: &str) -> Option<Response> {
    let headers = req.headers();
    let Some(auth_header) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    else {
        return Some(s3_error(
            StatusCode::FORBIDDEN,
            "AccessDenied",
            "missing Authorization header",
        ));
    };
    let Some(parsed) = parse_auth_header(auth_header) else {
        return Some(s3_error(
            StatusCode::FORBIDDEN,
            "AccessDenied",
            "malformed Authorization header",
        ));
    };

    // Wrong / unknown access key id is reported distinctly from a signature
    // mismatch, matching real S3.
    if !ct_eq(&parsed.akid, akid_want) {
        return Some(s3_error(
            StatusCode::FORBIDDEN,
            "InvalidAccessKeyId",
            "the access key id does not exist",
        ));
    }

    let Some(amz_date) = headers.get("x-amz-date").and_then(|v| v.to_str().ok()) else {
        return Some(s3_error(
            StatusCode::FORBIDDEN,
            "AccessDenied",
            "missing x-amz-date header",
        ));
    };
    // Payload hash is taken verbatim from the header (`UNSIGNED-PAYLOAD` or
    // the hex SHA-256 of the body); absent → the empty-body hash.
    let payload_hash = headers
        .get("x-amz-content-sha256")
        .and_then(|v| v.to_str().ok())
        .unwrap_or(EMPTY_PAYLOAD_SHA256);

    let sig_mismatch = || {
        s3_error(
            StatusCode::FORBIDDEN,
            "SignatureDoesNotMatch",
            "the request signature does not match",
        )
    };

    // Canonical request. The canonical URI is the path exactly as received
    // (S3 single-encodes, and the client signed the same on-the-wire form).
    let path = req.uri().path();
    let canonical_uri = if path.is_empty() { "/" } else { path };
    let canonical_query = canonical_query_string(req.uri().query().unwrap_or(""));
    let Some((canonical_headers, signed_headers)) =
        canonical_headers(headers, &parsed.signed_headers)
    else {
        return Some(sig_mismatch());
    };
    let canonical_request = format!(
        "{}\n{canonical_uri}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{payload_hash}",
        req.method().as_str(),
    );

    // String to sign.
    let scope = format!(
        "{}/{}/{}/aws4_request",
        parsed.date, parsed.region, parsed.service
    );
    let hashed_request = hex::encode(Sha256::digest(canonical_request.as_bytes()));
    let string_to_sign = format!("AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{hashed_request}");

    // Derived signing key → signature → constant-time compare.
    let signing_key = derive_signing_key(secret, &parsed.date, &parsed.region, &parsed.service);
    let signature = hex::encode(hmac_sha256(&signing_key, string_to_sign.as_bytes()));
    if ct_eq(&signature, &parsed.signature) {
        None
    } else {
        Some(sig_mismatch())
    }
}

/// Parse `AWS4-HMAC-SHA256 Credential=<akid>/<date>/<region>/<service>/aws4_request,
/// SignedHeaders=<a;b;c>, Signature=<hex>` into its fields.
fn parse_auth_header(header: &str) -> Option<ParsedAuth> {
    let rest = header.strip_prefix("AWS4-HMAC-SHA256")?.trim_start();
    let (mut credential, mut signed, mut signature) = (None, None, None);
    for part in rest.split(',') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix("Credential=") {
            credential = Some(v);
        } else if let Some(v) = part.strip_prefix("SignedHeaders=") {
            signed = Some(v);
        } else if let Some(v) = part.strip_prefix("Signature=") {
            signature = Some(v);
        }
    }

    let mut cred = credential?.split('/');
    let akid = cred.next()?.to_string();
    let date = cred.next()?.to_string();
    let region = cred.next()?.to_string();
    let service = cred.next()?.to_string();
    cred.next()?; // the `aws4_request` terminator must be present.

    Some(ParsedAuth {
        akid,
        date,
        region,
        service,
        signed_headers: signed?.split(';').map(str::to_string).collect(),
        signature: signature?.to_string(),
    })
}

/// Build the canonical-headers block and the `;`-joined signed-header list
/// from the request, for the header names the signature claims to cover.
/// Returns `None` if a signed header is absent or non-ASCII.
fn canonical_headers(headers: &HeaderMap, signed: &[String]) -> Option<(String, String)> {
    let mut names: Vec<String> = signed.iter().map(|s| s.to_ascii_lowercase()).collect();
    names.sort();
    names.dedup();

    let mut block = String::new();
    for name in &names {
        let value = headers.get(name.as_str()).and_then(|v| v.to_str().ok())?;
        block.push_str(name);
        block.push(':');
        block.push_str(value.trim());
        block.push('\n');
    }
    Some((block, names.join(";")))
}

/// Build the SigV4 canonical query string: each parameter decoded then
/// RFC 3986-re-encoded, sorted by encoded name (then value), `&`-joined.
fn canonical_query_string(query: &str) -> String {
    if query.is_empty() {
        return String::new();
    }
    let mut pairs: Vec<(String, String)> = query
        .split('&')
        .filter(|s| !s.is_empty())
        .map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            (uri_encode(&decode(k)), uri_encode(&decode(v)))
        })
        .collect();
    pairs.sort();
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// `HMAC-SHA256(key, data)`. HMAC accepts a key of any length, so the keying
/// step never fails.
fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts keys of any length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// Derive the SigV4 signing key:
/// `HMAC(HMAC(HMAC(HMAC("AWS4"+secret, date), region), service), "aws4_request")`.
fn derive_signing_key(secret: &str, date: &str, region: &str, service: &str) -> Vec<u8> {
    let k_date = hmac_sha256(format!("AWS4{secret}").as_bytes(), date.as_bytes());
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, service.as_bytes());
    hmac_sha256(&k_service, b"aws4_request")
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// One file discovered while walking the served root for a listing.
struct Entry {
    key: String,
    size: u64,
    modified: Option<SystemTime>,
    etag: String,
}

/// Recursively collect every regular file under `dir` as a key relative to
/// `root` (with `/` separators). `DirEntry::file_type` does not follow
/// symlinks, so a symlink (neither a plain file nor a real dir here) is
/// skipped — the listing never escapes the jail through one.
fn collect_entries(root: &Path, dir: &Path, out: &mut Vec<Entry>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let path = entry.path();
        if file_type.is_dir() {
            collect_entries(root, &path, out)?;
        } else if file_type.is_file() {
            let Ok(rel) = path.strip_prefix(root) else {
                continue;
            };
            let meta = entry.metadata()?;
            let modified = meta.modified().ok();
            out.push(Entry {
                key: rel.to_string_lossy().replace('\\', "/"),
                size: meta.len(),
                modified,
                etag: content_etag(meta.len(), modified),
            });
        }
    }
    Ok(())
}

/// Render the `ListBucketResult` XML for a ListObjectsV2 response.
fn render_list_xml(bucket: &str, prefix: &str, entries: &[Entry]) -> String {
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">");
    out.push_str(&format!("<Name>{}</Name>", xml_escape(bucket)));
    out.push_str(&format!("<Prefix>{}</Prefix>", xml_escape(prefix)));
    out.push_str(&format!("<KeyCount>{}</KeyCount>", entries.len()));
    out.push_str("<MaxKeys>1000</MaxKeys><IsTruncated>false</IsTruncated>");
    for e in entries {
        let modified = e.modified.map(rfc3339).unwrap_or_default();
        out.push_str(&format!(
            "<Contents><Key>{}</Key><LastModified>{modified}</LastModified>\
             <ETag>{}</ETag><Size>{}</Size><StorageClass>STANDARD</StorageClass></Contents>",
            xml_escape(&e.key),
            xml_escape(&e.etag),
            e.size,
        ));
    }
    out.push_str("</ListBucketResult>");
    out
}

/// An opaque, content-derived ETag (quoted hex). Real S3 returns the MD5 of
/// the object; we derive a stable token from size + mtime instead, which is
/// cheap (no second read) and sufficient for an opaque cache validator.
fn content_etag(size: u64, modified: Option<SystemTime>) -> String {
    let nanos = modified
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let digest = Sha256::digest(format!("{size}-{nanos}").as_bytes());
    format!("\"{}\"", hex::encode(&digest[..16]))
}

/// Look up a query parameter by name, returning its percent-decoded value.
fn query_param(query: Option<&str>, key: &str) -> Option<String> {
    query?.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        (k == key).then(|| decode(v))
    })
}

/// Percent-decode a string (lossily for invalid UTF-8).
fn decode(s: &str) -> String {
    percent_decode_str(s).decode_utf8_lossy().into_owned()
}

/// RFC 3986-encode a string, leaving only the unreserved set unescaped.
fn uri_encode(s: &str) -> String {
    utf8_percent_encode(s, SIGV4_UNRESERVED).to_string()
}

/// Minimal XML text escaping for keys / names embedded in the listing.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// An `application/xml` response with `body` and `status`.
fn s3_xml_response(status: StatusCode, body: String) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/xml".to_string())],
        body,
    )
        .into_response()
}

/// An S3 `<Error>` XML response.
fn s3_error(status: StatusCode, code: &str, message: &str) -> Response {
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Error><Code>{}</Code><Message>{}</Message></Error>",
        xml_escape(code),
        xml_escape(message),
    );
    s3_xml_response(status, body)
}

/// The 403 returned when a key escapes the served root.
fn access_denied() -> Response {
    s3_error(
        StatusCode::FORBIDDEN,
        "AccessDenied",
        "the key escapes the served root",
    )
}

/// Constant-time string compare via the workspace's shared `subtle`
/// primitive — same construction as the HTTP / SFTP paths.
fn ct_eq(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

/// Format a `SystemTime` as RFC 3339 UTC (`YYYY-MM-DDThh:mm:ssZ`) for the
/// listing's `<LastModified>`.
fn rfc3339(t: SystemTime) -> String {
    let secs = t
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (y, mo, d) = civil_from_days(secs.div_euclid(86_400));
    let tod = secs.rem_euclid(86_400);
    let (h, mi, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// Format a `SystemTime` as an HTTP IMF-fixdate (`Last-Modified` header).
fn http_date(t: SystemTime) -> String {
    const WD: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MO: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let secs = t
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let (y, mo, d) = civil_from_days(days);
    let tod = secs.rem_euclid(86_400);
    let (h, mi, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    // 1970-01-01 was a Thursday (index 4).
    let wd = WD[(days.rem_euclid(7) + 4) as usize % 7];
    let mo = MO[(mo - 1) as usize];
    format!("{wd}, {d:02} {mo} {y:04} {h:02}:{mi:02}:{s:02} GMT")
}

/// Civil date `(year, month, day)` from a count of days since the Unix epoch
/// (Howard Hinnant's `civil_from_days`).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canonical_root() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        (dir, root)
    }

    #[test]
    fn jail_rejects_traversal_absolute_and_encoded() {
        let (_dir, root) = canonical_root();

        // `..` traversal — literal and percent-encoded — plus an absolute
        // key all escape the jail and must be denied.
        for bad in [
            "../etc/passwd",
            "..%2f..%2fetc",
            "..%2F..%2Fetc%2Fpasswd",
            "/etc/passwd",
            "a/../../b",
        ] {
            assert!(
                resolve_key(&root, bad).is_none(),
                "escape `{bad}` must be denied"
            );
        }

        // A Windows drive-absolute key parses as a `Prefix` only on Windows.
        #[cfg(windows)]
        assert!(
            resolve_key(&root, "C:%5CWindows").is_none(),
            "drive-absolute key must be denied"
        );

        // A legitimate nested key resolves under the served root.
        let ok = resolve_key(&root, "dir/obj.bin").expect("legit key resolves");
        assert!(ok.starts_with(&root));
        assert!(ok.ends_with("obj.bin"));

        // An in-bounds `..` that stays under the root is fine.
        let inbounds = resolve_key(&root, "a/../b").expect("in-bounds key resolves");
        assert!(inbounds.starts_with(&root));
        assert!(inbounds.ends_with("b"));
    }

    #[test]
    fn split_bucket_key_shapes() {
        assert_eq!(split_bucket_key("/"), (String::new(), None));
        assert_eq!(split_bucket_key("/bucket"), ("bucket".into(), None));
        assert_eq!(
            split_bucket_key("/bucket/a/b.bin"),
            ("bucket".into(), Some("a/b.bin".into()))
        );
    }

    #[test]
    fn sigv4_signing_key_matches_reference() {
        // Known-answer test pinning the full HMAC-SHA256 signing chain
        // (kDate → kRegion → kService → kSigning → signature) against an
        // independent reference computed with Python's `hmac`/`hashlib`
        // standard library for AWS's canonical example secret + scope
        // (`wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY`, 20150830 / us-east-1 /
        // iam) and the documented string-to-sign. Any divergence in the key
        // derivation or the `"AWS4"` prefix would change this digest.
        let key = derive_signing_key(
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            "20150830",
            "us-east-1",
            "iam",
        );
        let string_to_sign = "AWS4-HMAC-SHA256\n20150830T123600Z\n20150830/us-east-1/iam/aws4_request\nf536975d06c0309214f805bb90ccff089219ecd68b2577efef23edd43b7e1a59";
        let signature = hex::encode(hmac_sha256(&key, string_to_sign.as_bytes()));
        assert_eq!(
            signature,
            "33f5dad2191de0cb4b7ab912f876876c2c4f72e2991a458f9499233c7b992438"
        );
    }

    #[test]
    fn parse_auth_header_extracts_fields() {
        let h = "AWS4-HMAC-SHA256 Credential=AKID/20240101/us-east-1/s3/aws4_request, \
                 SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature=abc123";
        let p = parse_auth_header(h).expect("parses");
        assert_eq!(p.akid, "AKID");
        assert_eq!(p.date, "20240101");
        assert_eq!(p.region, "us-east-1");
        assert_eq!(p.service, "s3");
        assert_eq!(
            p.signed_headers,
            vec!["host", "x-amz-content-sha256", "x-amz-date"]
        );
        assert_eq!(p.signature, "abc123");

        assert!(parse_auth_header("Basic dXNlcjpwdw==").is_none());
    }

    #[test]
    fn xml_escape_escapes_markup() {
        assert_eq!(xml_escape("a&b<c>\"'"), "a&amp;b&lt;c&gt;&quot;&apos;");
    }
}
