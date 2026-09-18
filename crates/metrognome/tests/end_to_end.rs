//! End-to-end `analyze` against a local stand-in for the iTunes API.
//!
//! This is the only test that exercises resolve -> fetch -> decode -> DSP as one
//! path. It serves a fixture search response and a WAV rendered from a known
//! groove, so the assertion at the end is that the whole pipeline recovers the
//! tempo that was synthesized at the start. No network, no Apple.

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

#[tokio::test]
async fn analyze_resolves_fetches_decodes_and_estimates_tempo() {
    let addr = serve().await;
    let analyzer = Analyzer::new(&AnalyzerConfig::default())
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
    let analyzer = Analyzer::new(&AnalyzerConfig::default())
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
    let analyzer = Analyzer::new(&AnalyzerConfig::default())
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
    assert_eq!(out.error.expect("error").kind, "no_preview");
    // The query survives the failure so the caller can tell which row broke.
    assert_eq!(out.query.client_ref.as_deref(), Some("keep-me"));
}
