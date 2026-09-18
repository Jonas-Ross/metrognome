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

/// Redirect hops allowed.
///
/// A preview redirects to a CDN once in practice. reqwest's default is ten, to
/// anywhere, which turns a URL taken from a JSON response into a request to
/// wherever that response points.
const MAX_REDIRECTS: usize = 2;

/// Whether a URL may be requested at all.
///
/// Plain HTTP is allowed only to loopback, so the fixture servers in the tests
/// keep working while a redirect cannot downgrade a real fetch off TLS.
fn allowed(url: &reqwest::Url) -> bool {
    match url.scheme() {
        "https" => true,
        "http" => matches!(
            url.host_str(),
            Some("127.0.0.1") | Some("localhost") | Some("[::1]") | Some("::1")
        ),
        _ => false,
    }
}

/// Build the shared HTTP client.
///
/// One client per process: it owns the connection pool, and the iTunes endpoints
/// all live on the same host, so reusing connections is most of the latency win.
pub fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("metrognome/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                attempt.error(format!("more than {MAX_REDIRECTS} redirects"))
            } else if allowed(attempt.url()) {
                attempt.follow()
            } else {
                let to = attempt.url().clone();
                attempt.error(format!("refused redirect to {to}"))
            }
        }))
        .build()
        .map_err(|e| Error::Http(format!("client build: {e}")))
}

/// Content types a preview body may claim.
///
/// An allowlist, but a generous one: Apple serves `audio/x-m4a` and CDNs vary,
/// and this path cannot be exercised against the real API from CI, so a type
/// nobody anticipated must not break a working fetch. It still rejects the case
/// that matters — HTML or JSON from a redirect being decoded as audio.
fn plausible_audio(ctype: Option<&str>) -> bool {
    let Some(ctype) = ctype else {
        // Absent is not a claim of anything; the decoder is the real check.
        return true;
    };
    let ctype = ctype
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    ctype.is_empty()
        || ctype.starts_with("audio/")
        || ctype == "video/mp4"
        || ctype == "video/quicktime"
        || ctype == "application/octet-stream"
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
    let parsed =
        reqwest::Url::parse(url).map_err(|e| Error::Http(format!("bad URL {url}: {e}")))?;
    if !allowed(&parsed) {
        return Err(Error::Http(format!("refused URL {url}")));
    }

    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| Error::Http(format!("GET {url}: {e}")))?;

    let status = resp.status();
    if !status.is_success() {
        return Err(Error::Http(format!("GET {url}: status {status}")));
    }

    let ctype = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    if !plausible_audio(ctype.as_deref()) {
        return Err(Error::Http(format!(
            "GET {url}: not audio, served {}",
            ctype.unwrap_or_default()
        )));
    }

    read_bounded(resp, MAX_PREVIEW_BYTES, "preview").await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> reqwest::Url {
        reqwest::Url::parse(s).expect("url")
    }

    #[test]
    fn only_tls_or_loopback_is_requestable() {
        assert!(allowed(&url("https://itunes.apple.com/x.m4a")));
        assert!(allowed(&url("http://127.0.0.1:8080/x.wav")));
        assert!(!allowed(&url("http://example.com/x.m4a")));
        assert!(!allowed(&url("file:///etc/passwd")));
    }

    #[test]
    fn html_is_not_a_preview() {
        assert!(plausible_audio(Some("audio/x-m4a")));
        assert!(plausible_audio(Some("audio/wav; charset=binary")));
        assert!(plausible_audio(Some("video/mp4")));
        assert!(plausible_audio(None));
        assert!(!plausible_audio(Some("text/html")));
        assert!(!plausible_audio(Some("application/json")));
    }
}
