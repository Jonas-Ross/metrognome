//! `batch` behaviour, driven through the real binary.
//!
//! The guarantees being checked are the ones selecta depends on: exactly one
//! output line per input line, in input order, with a bad row reported rather
//! than dropped, and nothing but JSON on stdout.

use std::io::Write;
use std::net::SocketAddr;
use std::process::{Command, Stdio};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Serves a fixture search/lookup response and a WAV preview for any track.
async fn serve(preview: Vec<u8>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let preview = preview.clone();
            let base = format!("http://{addr}");
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let Ok(n) = sock.read(&mut buf).await else {
                    return;
                };
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();

                let (ctype, body): (&str, Vec<u8>) = if path.starts_with("/preview") {
                    ("audio/wav", preview)
                } else if path.contains("nothing") {
                    (
                        "application/json",
                        br#"{"resultCount":0,"results":[]}"#.to_vec(),
                    )
                } else {
                    let id = if path.starts_with("/lookup") {
                        4242
                    } else {
                        999
                    };
                    (
                        "application/json",
                        format!(
                            r#"{{"resultCount":1,"results":[{{"trackId":{id},
                            "artistName":"Test Act","trackName":"Test Track",
                            "previewUrl":"{base}/preview.wav"}}]}}"#
                        )
                        .into_bytes(),
                    )
                };
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
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

#[tokio::test(flavor = "multi_thread")]
async fn batch_emits_one_line_per_input_in_order_and_never_drops_a_row() {
    let sr = 44_100;
    let preview = metrognome::testsig::wav_bytes(
        &metrognome::testsig::groove(130.0, 20.0, sr, metrognome::testsig::Groove::FourOnFloor),
        sr,
        1,
    );
    let addr = serve(preview).await;
    let cache = std::env::temp_dir().join(format!("mg-batch-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&cache);

    let input = concat!(
        r#"{"artist":"Test Act","title":"Test Track","client_ref":"first"}"#,
        "\n",
        "this is not json\n",
        r#"{"artist":"Test Act","title":"nothing at all","client_ref":"third"}"#,
        "\n",
        r#"{"track_id":4242,"client_ref":"fourth"}"#,
        "\n",
    );

    let mut child = Command::new(env!("CARGO_BIN_EXE_metrognome"))
        .args([
            "batch",
            "--concurrency",
            "4",
            "--api-base-url",
            &format!("http://{addr}"),
            "--cache-path",
            cache.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn metrognome");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(input.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait");

    let stdout = String::from_utf8(out.stdout).expect("utf8");
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 4, "expected one line per input row:\n{stdout}");

    let parsed: Vec<serde_json::Value> = lines
        .iter()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("line not JSON: {l} ({e})")))
        .collect();

    // Every line, failures included, is a full `Analysis`. A consumer that
    // deserializes the stream into one type must not choke on precisely the
    // bad row batch mode exists to report.
    for l in &lines {
        serde_json::from_str::<metrognome::Analysis>(l)
            .unwrap_or_else(|e| panic!("line is not an Analysis: {l} ({e})"));
    }

    // Order is input order, not completion order.
    assert_eq!(parsed[0]["query"]["client_ref"], "first");
    assert_eq!(parsed[0]["status"], "ok");
    let bpm = parsed[0]["features"]["tempo"]["bpm"].as_f64().unwrap();
    assert!((bpm - 130.0).abs() < 1.0, "bpm {bpm}");

    // A malformed line is reported in place, keeping positional alignment.
    assert_eq!(parsed[1]["status"], "error");
    assert_eq!(parsed[1]["error"]["kind"], "invalid_input");

    // A track the store has nothing for is an error result, not a lost row.
    assert_eq!(parsed[2]["status"], "error");
    assert_eq!(parsed[2]["query"]["client_ref"], "third");
    assert_eq!(parsed[2]["error"]["kind"], "no_match");

    assert_eq!(parsed[3]["query"]["client_ref"], "fourth");
    assert_eq!(parsed[3]["status"], "ok");

    let _ = std::fs::remove_file(&cache);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_run_is_served_from_the_cache() {
    let sr = 44_100;
    let preview = metrognome::testsig::wav_bytes(
        &metrognome::testsig::groove(122.0, 20.0, sr, metrognome::testsig::Groove::FourOnFloor),
        sr,
        1,
    );
    let addr = serve(preview).await;
    let cache = std::env::temp_dir().join(format!("mg-cache-run-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&cache);

    let run = || {
        let out = Command::new(env!("CARGO_BIN_EXE_metrognome"))
            .args([
                "analyze",
                "--artist",
                "Test Act",
                "--title",
                "Test Track",
                "--api-base-url",
                &format!("http://{addr}"),
                "--cache-path",
                cache.to_str().unwrap(),
            ])
            .output()
            .expect("run metrognome");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&out.stdout).expect("json on stdout")
    };

    let first = run();
    assert_eq!(first["cached"], false);
    let second = run();
    assert_eq!(second["cached"], true);
    assert_eq!(first["features"], second["features"]);

    let _ = std::fs::remove_file(&cache);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cache_hit_reports_this_query_s_match_not_the_one_that_filled_it() {
    let sr = 44_100;
    let preview = metrognome::testsig::wav_bytes(
        &metrognome::testsig::groove(126.0, 20.0, sr, metrognome::testsig::Groove::FourOnFloor),
        sr,
        1,
    );
    let addr = serve(preview).await;
    let cache = std::env::temp_dir().join(format!("mg-match-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&cache);

    let run = |title: &str| {
        let out = Command::new(env!("CARGO_BIN_EXE_metrognome"))
            .args([
                "analyze",
                "--artist",
                "Test Act",
                "--title",
                title,
                "--api-base-url",
                &format!("http://{addr}"),
                "--cache-path",
                cache.to_str().unwrap(),
            ])
            .output()
            .expect("run metrognome");
        serde_json::from_slice::<metrognome::Analysis>(&out.stdout).expect("analysis on stdout")
    };

    // The store answers every search with the same track, so a good query and
    // a bad one land on one cache row. The analysis is shared; the match is not.
    let exact = run("Test Track");
    assert!(!exact.cached);
    let exact_score = exact.track.as_ref().expect("track").match_score;
    assert!(exact_score > 0.9, "score {exact_score}");

    let sloppy = run("Something Else Entirely");
    assert!(sloppy.cached, "second query should reuse the analysis");
    let t = sloppy.track.as_ref().expect("track");
    assert!(
        t.match_score < exact_score,
        "a cache hit reported the earlier query's match score: {} vs {exact_score}",
        t.match_score
    );
    assert!(
        t.uncertain,
        "a weak match must stay flagged through the cache"
    );
    assert_eq!(exact.features, sloppy.features, "features are shared");

    let _ = std::fs::remove_file(&cache);
}
