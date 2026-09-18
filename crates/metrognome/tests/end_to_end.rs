//! End-to-end `analyze` against a local stand-in for the iTunes API.
//!
//! The only test exercising resolve -> fetch -> decode -> DSP as one path. It
//! serves a fixture search response and a WAV rendered from a known groove, and
//! asserts the pipeline recovers the tempo that went in. No network.

use std::net::SocketAddr;

use metrognome::testsig::{self, Groove};
use metrognome::{Analyzer, AnalyzerConfig, Query};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const SR: u32 = 44_100;
const TRUE_BPM: f32 = 126.0;

async fn serve() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let preview = testsig::wav_bytes(
        &testsig::groove(TRUE_BPM, 30.0, SR, Groove::FourOnFloor),
        SR,
        1,
    );

    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let preview = preview.clone();
            let base = format!("http://{addr}");
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let Ok(n) = sock.read(&mut buf).await else {
                    return;
                };
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();

                let (content_type, body): (&str, Vec<u8>) = if path.starts_with("/preview") {
                    ("audio/wav", preview)
                } else if path.contains("id=777") {
                    // The row exists and is permanently unanalyzable, which is
                    // a different state from an ID that matches nothing.
                    (
                        "application/json",
                        br#"{"resultCount":1,"results":[{"wrapperType":"track","kind":"song",
                        "trackId":777,"artistName":"Test Act","trackName":"No Preview"}]}"#
                            .to_vec(),
                    )
                } else if path.starts_with("/search") || path.starts_with("/lookup") {
                    let json = format!(
                        r#"{{"resultCount":1,"results":[{{"wrapperType":"track","kind":"song",
                        "trackId":999,"artistName":"Test Act","trackName":"Test Track",
                        "collectionName":"Test Album","releaseDate":"2020-01-01T12:00:00Z",
                        "primaryGenreName":"Electronic","trackTimeMillis":300000,
                        "previewUrl":"{base}/preview.wav"}}]}}"#
                    );
                    ("application/json", json.into_bytes())
                } else {
                    ("text/plain", b"not found".to_vec())
                };

                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(&body).await;
                let _ = sock.flush().await;
            });
        }
    });
    addr
}

/// An analyzer config with no cache.
///
/// `AnalyzerConfig::default()` points at the real user cache: it writes to
/// whoever runs the suite, serves back resolutions whose preview URLs name a
/// long-dead random port, and lets the test pass without fetching or decoding
/// anything.
fn uncached() -> AnalyzerConfig {
    AnalyzerConfig {
        cache_path: None,
        ..Default::default()
    }
}

#[tokio::test]
async fn analyze_resolves_fetches_decodes_and_estimates_tempo() {
    let addr = serve().await;
    let analyzer = Analyzer::new(&uncached())
        .expect("analyzer")
        .with_base_url(format!("http://{addr}"));

    let out = analyzer
        .analyze(Query {
            artist: Some("Test Act".into()),
            title: Some("Test Track".into()),
            client_ref: Some("persistent-id-123".into()),
            ..Default::default()
        })
        .await;

    assert_eq!(out.status, "ok", "{out:?}");
    assert_eq!(out.schema_version, metrognome::SCHEMA_VERSION);
    // The caller's own key comes back untouched, so a batch row can be matched
    // up without relying on output order.
    assert_eq!(out.query.client_ref.as_deref(), Some("persistent-id-123"));

    let track = out.track.expect("track");
    assert_eq!(track.track_id, 999);
    assert!(!track.uncertain, "score {}", track.match_score);

    let audio = out.audio.expect("audio");
    assert_eq!(audio.sample_rate, SR);
    assert!((audio.duration_secs - 30.0).abs() < 0.5);

    let tempo = out.features.tempo.expect("tempo");
    assert!(
        (tempo.bpm - TRUE_BPM).abs() < 1.0,
        "expected {TRUE_BPM}, got {} (confidence {})",
        tempo.bpm,
        tempo.confidence
    );
    assert!(!tempo.uncertain);
}

#[tokio::test]
async fn a_track_id_takes_the_lookup_path() {
    let addr = serve().await;
    let analyzer = Analyzer::new(&uncached())
        .expect("analyzer")
        .with_base_url(format!("http://{addr}"));

    let out = analyzer
        .analyze(Query {
            track_id: Some(999),
            ..Default::default()
        })
        .await;

    assert_eq!(out.status, "ok", "{out:?}");
    // An ID identifies rather than describes, so there is no fuzziness to report.
    assert_eq!(out.track.expect("track").match_score, 1.0);
}

#[tokio::test]
async fn a_lookup_that_returns_nothing_useful_is_a_reportable_failure() {
    let addr = serve().await;
    let analyzer = Analyzer::new(&uncached())
        .expect("analyzer")
        .with_base_url(format!("http://{addr}"));

    let out = analyzer
        .analyze(Query {
            track_id: Some(12345),
            client_ref: Some("keep-me".into()),
            ..Default::default()
        })
        .await;

    assert_eq!(out.status, "error");
    // Not `no_preview`: that kind says the track exists and can never be
    // analyzed, which a consumer may record and never retry. A mistyped ID
    // would then be marked terminally unanalyzable.
    assert_eq!(out.error.expect("error").kind, "not_found");
    // The query survives the failure so the caller can tell which row broke.
    assert_eq!(out.query.client_ref.as_deref(), Some("keep-me"));
}

#[tokio::test]
async fn a_track_with_no_preview_is_a_different_failure_from_a_missing_one() {
    let addr = serve().await;
    let analyzer = Analyzer::new(&uncached())
        .expect("analyzer")
        .with_base_url(format!("http://{addr}"));

    let out = analyzer
        .analyze(Query {
            track_id: Some(777),
            ..Default::default()
        })
        .await;

    assert_eq!(out.status, "error");
    assert_eq!(out.error.expect("error").kind, "no_preview");
}

/// Serves an endless chunked body with no `Content-Length`, which is how a
/// hostile or misconfigured origin defeats a size check that only runs after
/// the body has been buffered.
/// Streams an endless chunked body from any path, so both the preview and the
/// search path can be pointed at it.
async fn serve_endless_chunks() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                if sock.read(&mut buf).await.is_err() {
                    return;
                }
                let head = "HTTP/1.1 200 OK\r\nContent-Type: audio/mp4\r\n\
                            Transfer-Encoding: chunked\r\n\r\n";
                if sock.write_all(head.as_bytes()).await.is_err() {
                    return;
                }
                // 64 KiB per chunk, forever — until the client gives up, which
                // is the behaviour under test.
                let chunk = vec![b'A'; 64 * 1024];
                let header = format!("{:x}\r\n", chunk.len());
                loop {
                    if sock.write_all(header.as_bytes()).await.is_err()
                        || sock.write_all(&chunk).await.is_err()
                        || sock.write_all(b"\r\n").await.is_err()
                    {
                        return;
                    }
                }
            });
        }
    });
    addr
}

#[tokio::test]
async fn a_body_with_no_declared_length_still_hits_the_size_limit() {
    let addr = serve_endless_chunks().await;
    let client = metrognome::fetch::client().expect("client");
    let url = format!("http://{addr}/endless.m4a");

    let err = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        metrognome::fetch::fetch_bytes(&client, &url),
    )
    .await
    .expect("fetch_bytes must give up on its own, not run until the test times out")
    .expect_err("an unbounded body must be refused");

    assert!(
        err.to_string().contains("too large"),
        "expected a size refusal, got: {err}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_search_response_is_bounded_too() {
    // `--api-base-url` and a redirect both point resolution at servers Apple
    // does not run, and the body is buffered whole before it is parsed.
    let addr = serve_endless_chunks().await;
    let resolver = metrognome::resolve::Resolver::new(
        metrognome::fetch::client().expect("client"),
        metrognome::ratelimit::RateLimiter::new(600.0, 10.0),
    )
    .with_base_url(format!("http://{addr}"));

    let err = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        resolver.search("Any", "Thing"),
    )
    .await
    .expect("the search read must give up on its own")
    .expect_err("an unbounded search body must be refused");

    assert!(
        err.to_string().contains("too large"),
        "expected a size refusal, got: {err}"
    );
}
