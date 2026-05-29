use axum::body::Body;
use axum::extract::{Query, State as AxumState};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, State};
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::io::ReaderStream;

// ----- localhost media server -----
//
// Chromium plays HTTP files much more reliably than custom protocols, so we
// stand up a tiny in-process HTTP server on a random localhost port and point
// the <video> element at it. The server:
//   * requires a per-session token (so other origins in the webview can't
//     guess the URL),
//   * only serves files in an allowlist (paths the user has explicitly
//     registered through register_file_url), and
//   * supports byte-range requests, which is what kills the asset:// hitches.

#[derive(Clone)]
pub struct ServerState {
    pub token: String,
    pub port: u16,
    pub allowlist: Arc<AsyncMutex<HashSet<PathBuf>>>,
}

pub struct ServerInfo {
    pub port: u16,
    pub state: ServerState,
}

#[derive(Deserialize)]
pub(crate) struct ServeQuery {
    token: String,
    p: String,
}

pub(crate) fn generate_session_token() -> String {
    // 32 bytes from the OS CSPRNG → 64-char hex. Predictable inputs (time, pid)
    // would let any local process guess the token and reach the media server.
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).expect("OS RNG unavailable");
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

pub(crate) async fn serve_file(
    AxumState(state): AxumState<ServerState>,
    Query(q): Query<ServeQuery>,
    headers: HeaderMap,
) -> Response {
    // Defeat DNS rebinding: only accept Host headers that target our exact
    // loopback port. A remote site that learns the port can still send the
    // request, but the browser-supplied Host will be the attacker's domain.
    let expected_host_v4 = format!("127.0.0.1:{}", state.port);
    let expected_host_localhost = format!("localhost:{}", state.port);
    let host_ok = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(|h| h == expected_host_v4 || h == expected_host_localhost)
        .unwrap_or(false);
    if !host_ok {
        return (StatusCode::FORBIDDEN, "bad host").into_response();
    }
    if q.token != state.token {
        return (StatusCode::FORBIDDEN, "bad token").into_response();
    }
    let path = PathBuf::from(&q.p);
    {
        let allow = state.allowlist.lock().await;
        if !allow.contains(&path) {
            return (StatusCode::FORBIDDEN, "not allowed").into_response();
        }
    }
    let mut file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(_) => return (StatusCode::NOT_FOUND, "open failed").into_response(),
    };
    let total = match file.metadata().await {
        Ok(m) => m.len(),
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "metadata").into_response(),
    };
    let mime = mime_guess::from_path(&path).first_or_octet_stream();
    let mime_str = mime.essence_str().to_string();

    let range_header = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    if let Some(range) = range_header {
        if let Some(rest) = range.strip_prefix("bytes=") {
            let parts: Vec<&str> = rest.split('-').collect();
            if parts.len() == 2 {
                let start: u64 = parts[0].parse().unwrap_or(0);
                let end: u64 = if parts[1].is_empty() {
                    total.saturating_sub(1)
                } else {
                    parts[1]
                        .parse::<u64>()
                        .unwrap_or(total.saturating_sub(1))
                        .min(total.saturating_sub(1))
                };
                if total == 0 || start > end {
                    return Response::builder()
                        .status(StatusCode::RANGE_NOT_SATISFIABLE)
                        .header(header::CONTENT_RANGE, format!("bytes */{}", total))
                        .body(Body::empty())
                        .unwrap();
                }
                let chunk_len = end - start + 1;
                if file.seek(SeekFrom::Start(start)).await.is_err() {
                    return (StatusCode::INTERNAL_SERVER_ERROR, "seek").into_response();
                }
                let limited = file.take(chunk_len);
                let stream = ReaderStream::new(limited);
                return Response::builder()
                    .status(StatusCode::PARTIAL_CONTENT)
                    .header(header::CONTENT_TYPE, mime_str)
                    .header(header::CONTENT_LENGTH, chunk_len.to_string())
                    .header(
                        header::CONTENT_RANGE,
                        format!("bytes {}-{}/{}", start, end, total),
                    )
                    .header(header::ACCEPT_RANGES, "bytes")
                    // CORS so MediaElementSource can read samples for WebAudio.
                    // Token + Host already gate access; CORS is the browser's.
                    .header("access-control-allow-origin", "*")
                    .header(
                        "access-control-expose-headers",
                        "Content-Range, Content-Length, Accept-Ranges",
                    )
                    .body(Body::from_stream(stream))
                    .unwrap();
            }
        }
    }

    let stream = ReaderStream::new(file);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime_str)
        .header(header::CONTENT_LENGTH, total.to_string())
        .header(header::ACCEPT_RANGES, "bytes")
        .header("access-control-allow-origin", "*")
        .header(
            "access-control-expose-headers",
            "Content-Range, Content-Length, Accept-Ranges",
        )
        .body(Body::from_stream(stream))
        .unwrap()
}

/// CORS preflight handler. Browsers send OPTIONS with `Access-Control-Request-*`
/// headers before media element fetches that have a non-simple Origin/cred
/// configuration. Respond with permissive headers (token + Host already gate
/// real access).
pub(crate) async fn serve_options() -> Response {
    Response::builder()
        .status(StatusCode::NO_CONTENT)
        .header("access-control-allow-origin", "*")
        .header("access-control-allow-methods", "GET, OPTIONS")
        .header("access-control-allow-headers", "Range, Content-Type")
        .header("access-control-max-age", "86400")
        .body(Body::empty())
        .unwrap()
}

#[tauri::command]
pub async fn register_file_url(
    app: AppHandle,
    state: State<'_, ServerInfo>,
    path: String,
) -> Result<String, String> {
    // Scoped allowlist: a renderer-side bug (or a future XSS) must not be able
    // to coerce the backend into serving arbitrary local files like
    // NTUSER.DAT or SSH keys. Two cases are legitimate:
    //   1. Files we generated under the proxy cache dir (remux / encode /
    //      extracted track outputs).
    //   2. Files generate_proxy already trusted on this session — for the
    //      Direct strategy, that's the original source MP4.
    // Anything else is rejected outright.
    let canonical =
        std::fs::canonicalize(&path).map_err(|e| format!("canonicalize failed: {}", e))?;
    let proxies = std::fs::canonicalize(crate::proxy::proxy_dir(&app)?)
        .map_err(|e| format!("proxy dir canonicalize failed: {}", e))?;
    let in_proxies = canonical.starts_with(&proxies);
    let already_trusted = state.state.allowlist.lock().await.contains(&canonical);
    if !in_proxies && !already_trusted {
        return Err("path not allowed".into());
    }
    state.state.allowlist.lock().await.insert(canonical.clone());
    let encoded = urlencoding::encode(&canonical.to_string_lossy()).into_owned();
    Ok(format!(
        "http://127.0.0.1:{}/vid?token={}&p={}",
        state.port, state.state.token, encoded
    ))
}

/// Insert a path into the media-server allowlist directly. Used by backend
/// commands (generate_proxy's Direct strategy) to pre-trust a source path
/// that register_file_url would otherwise reject for being outside proxy_dir.
pub(crate) async fn allowlist_trust(
    state: &ServerState,
    path: &std::path::Path,
) -> Result<(), String> {
    let canonical = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
    state.allowlist.lock().await.insert(canonical);
    Ok(())
}
