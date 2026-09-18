//! HTTP fetching of preview clips.
//!
//! Previews are well under a megabyte, so they are read fully into memory.
//! There is deliberately no disk path.

use std::time::Duration;

use crate::error::{Error, Result};

/// Refuse anything larger than this. A preview is ~500 KB; an order of
/// magnitude of headroom still catches a redirect to something unexpected
/// before it costs memory.
const MAX_PREVIEW_BYTES: usize = 8 * 1024 * 1024;

/// Build the shared HTTP client.
///
/// One client per process: it owns the connection pool, and the iTunes endpoints
/// all live on the same host, so reusing connections is most of the latency win.
pub fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("metrognome/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| Error::Http(format!("client build: {e}")))
}

/// Read a response body into memory with a hard ceiling.
///
/// Chunk by chunk rather than with `bytes()`/`text()`, which buffer the whole
/// body before any limit can be applied: a chunked response declares no length,
/// so the ceiling would only be checked once it was already in memory. Both
/// URLs reaching this crate come from outside it — `probe --url` from the
/// caller, the preview URL from a JSON response.
pub(crate) async fn read_bounded(
    mut resp: reqwest::Response,
    limit: usize,
    what: &str,
) -> Result<Vec<u8>> {
    // A declared length lets us refuse before spending any bandwidth.
    if let Some(len) = resp.content_length() {
        if len as usize > limit {
            return Err(Error::Http(format!(
                "{what} too large: {len} bytes (limit {limit})"
            )));
        }
    }
    let mut out: Vec<u8> = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| Error::Http(format!("read {what}: {e}")))?
    {
        if out.len() + chunk.len() > limit {
            return Err(Error::Http(format!("{what} too large: over {limit} bytes")));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

/// Download a preview clip into memory.
pub async fn fetch_bytes(client: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| Error::Http(format!("GET {url}: {e}")))?;

    let status = resp.status();
    if !status.is_success() {
        return Err(Error::Http(format!("GET {url}: status {status}")));
    }

    read_bounded(resp, MAX_PREVIEW_BYTES, "preview").await
}
