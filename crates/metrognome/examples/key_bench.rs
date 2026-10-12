//! Measure the key scorers against labelled audio, and dump the band scorer's
//! features for `tools/keyfit/fit.py`.
//!
//! Reads `dataset<TAB>id<TAB>url<TAB>tonic pitch class<TAB>major|minor` lines on
//! stdin, prints one JSON object per track on stdout. Each clip is fetched with
//! `curl` into memory, cut to the 30 seconds in its middle to stand in for a
//! store preview, analyzed, and dropped. Not part of the test suite: it needs
//! the network. `tools/keyfit/README.md` has the dataset and the commands.

use metrognome::dsp::Stft;
use metrognome::key::{band_chroma, estimate_key_bands, key_from_pcm};
use metrognome::KeyProfile;
use rayon::prelude::*;
use std::io::{BufRead, Write};
use std::process::Command;
use std::sync::Mutex;

const CLIP_SECS: f64 = 30.0;

fn main() {
    let rows: Vec<Vec<String>> = std::io::stdin()
        .lock()
        .lines()
        .map_while(Result::ok)
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.split('\t').map(String::from).collect())
        .collect();
    let out = Mutex::new(std::io::stdout());
    let done = std::sync::atomic::AtomicUsize::new(0);
    let total = rows.len();
    rows.par_iter().for_each(|r| {
        let line = bench_one(r).unwrap_or_else(|e| {
            serde_json::json!({"dataset": r[0], "id": r[1], "error": e}).to_string()
        });
        writeln!(out.lock().expect("stdout"), "{line}").expect("stdout");
        let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        if n.is_multiple_of(50) || n == total {
            eprintln!("{n}/{total}");
        }
    });
}

fn bench_one(r: &[String]) -> Result<String, String> {
    let [dataset, id, url, pc, mode] = r else {
        return Err(format!("expected 5 columns, got {}", r.len()));
    };
    let fetched = Command::new("curl")
        .args(["-sS", "--fail", "--max-time", "60", url])
        .output()
        .map_err(|e| e.to_string())?;
    if !fetched.status.success() {
        return Err(String::from_utf8_lossy(&fetched.stderr).trim().to_string());
    }
    let pcm = metrognome::decode_bytes(fetched.stdout, url.rsplit('.').next())
        .map_err(|e| e.to_string())?;
    let sr = pcm.sample_rate;
    let skip = ((pcm.duration_secs() - CLIP_SECS).max(0.0) / 2.0 * f64::from(sr)) as usize;
    let end = (skip + (CLIP_SECS * f64::from(sr)) as usize).min(pcm.samples.len());
    let clip = &pcm.samples[skip..end];

    let chroma = band_chroma(&Stft::for_key_bands(sr).magnitudes(clip, sr));
    let mut estimates = serde_json::Map::new();
    let bands = estimate_key_bands(&chroma);
    for profile in [KeyProfile::Edm, KeyProfile::Krumhansl] {
        estimates.insert(
            format!("{profile:?}"),
            summary(key_from_pcm(clip, sr, profile)),
        );
    }
    estimates.insert("Bands".into(), summary(bands));
    Ok(serde_json::json!({
        "dataset": dataset, "id": id, "tonic": pc.parse::<u8>().map_err(|e| e.to_string())?,
        "mode": mode, "bands": chroma.bands, "estimates": estimates,
    })
    .to_string())
}

fn summary(
    est: Option<(metrognome::KeyEstimate, metrognome::types::KeyScoring)>,
) -> serde_json::Value {
    est.map_or(
        serde_json::Value::Null,
        |(e, s)| serde_json::json!({"key": e.key, "confidence": e.confidence, "scoring": s}),
    )
}
