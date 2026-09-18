//! HTTP fetching of preview clips.
//!
//! Previews are ~30 s of AAC, well under a megabyte, so they are read fully
//! into memory. There is deliberately no disk path: the bytes exist only for
//! the life of the analysis.

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

/// Download a preview clip into memory.
pub async fn fetch_bytes(client: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    let mut resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| Error::Http(format!("GET {url}: {e}")))?;

    let status = resp.status();
    if !status.is_success() {
        return Err(Error::Http(format!("GET {url}: status {status}")));
    }

    // A declared length lets us refuse before spending any bandwidth.
    if let Some(len) = resp.content_length() {
        if len as usize > MAX_PREVIEW_BYTES {
            return Err(Error::Http(format!(
                "preview too large: {len} bytes (limit {MAX_PREVIEW_BYTES})"
            )));
        }
    }

    // Read chunk by chunk rather than with `bytes()`, which buffers the whole
    // body first. A chunked response declares no length, so without this the
    // limit would only be checked after an arbitrarily large body was already
    // in memory — and `probe --url` takes a URL straight from the caller.
    let mut out: Vec<u8> = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| Error::Http(format!("read body {url}: {e}")))?
    {
        if out.len() + chunk.len() > MAX_PREVIEW_BYTES {
            return Err(Error::Http(format!(
                "preview too large: over {MAX_PREVIEW_BYTES} bytes"
            )));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}
