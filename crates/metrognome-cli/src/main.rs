//! `metrognome` CLI.
//!
//! Contract with selecta (and any other machine consumer): **stdout carries
//! nothing but JSON**. Logs, progress, and diagnostics go to stderr. Breaking
//! that is breaking the interface.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use metrognome::{Analysis, AnalysisOptions, Analyzer, AnalyzerConfig, Error, KeyProfile, Query};
use tokio::io::{AsyncBufReadExt, BufReader};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "metrognome",
    version,
    about = "BPM and key estimation from preview clips"
)]
struct Cli {
    /// Increase log verbosity (repeatable). Logs always go to stderr.
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Estimate features for one track. Prints one JSON object on stdout.
    Analyze {
        /// Artist name. Required unless --track-id is given.
        #[arg(long)]
        artist: Option<String>,
        /// Track title. Required unless --track-id is given.
        #[arg(long)]
        title: Option<String>,
        /// iTunes store track ID. Not a Music.app persistent ID.
        #[arg(long)]
        track_id: Option<i64>,
        /// Opaque string echoed back in the result.
        #[arg(long)]
        client_ref: Option<String>,
        #[command(flatten)]
        opts: CommonOpts,
    },
    /// Read JSON lines on stdin, write one result line per input line on
    /// stdout, in input order. A bad line is a result with `status: "error"`,
    /// never a crash.
    Batch {
        /// How many tracks to have in flight at once.
        ///
        /// Resolution is rate limited regardless; this mostly overlaps preview
        /// downloads, which are not.
        #[arg(long, default_value_t = 4)]
        concurrency: usize,
        #[command(flatten)]
        opts: CommonOpts,
    },
    /// Analyze a set of well-known electronic tracks with documented tempos
    /// and report expected versus estimated.
    ///
    /// Needs the network. The table goes to stderr so stdout stays parseable.
    Validate {
        #[command(flatten)]
        opts: CommonOpts,
    },
    /// Run the same accuracy check against synthesized audio. No network.
    ///
    /// Same tempo range as `validate`, carrying the octave and metric traps
    /// each idiom actually has.
    Selftest {
        /// Sample rate to synthesize at.
        #[arg(long, default_value_t = 44_100)]
        sample_rate: u32,
        /// Key profile set: `edm` (default) or `krumhansl`.
        #[arg(long, default_value = "edm", value_parser = parse_key_profile)]
        key_profile: KeyProfile,
    },
    /// Download a preview clip and print decoded PCM statistics as JSON.
    Probe {
        /// Direct URL to an audio clip.
        #[arg(long)]
        url: String,
    },
}

#[derive(clap::Args, Debug, Clone)]
struct CommonOpts {
    /// iTunes API requests per minute. The documented ceiling is around 20;
    /// raising this risks the address being throttled.
    #[arg(long, default_value_t = metrognome::ratelimit::DEFAULT_PER_MINUTE)]
    requests_per_minute: f64,
    /// How many requests may be issued back to back from idle.
    #[arg(long, default_value_t = metrognome::ratelimit::DEFAULT_BURST)]
    burst: f64,
    /// Key profile set: `edm` (default) or `krumhansl`.
    #[arg(long, default_value = "edm", value_parser = parse_key_profile)]
    key_profile: KeyProfile,
    /// Where to keep the result cache. Defaults to the platform cache
    /// directory.
    #[arg(long)]
    cache_path: Option<PathBuf>,
    /// Do not read or write the cache.
    #[arg(long)]
    no_cache: bool,
    /// Attach the per-factor breakdown behind each key confidence.
    ///
    /// For working out why a key scored the way it did over an arbitrary set
    /// of tracks, which the fixed reference set of `validate` cannot cover.
    #[arg(long)]
    explain_key: bool,
    /// Origin for the iTunes API. Hidden: it exists so the test suite can point
    /// the binary at a local stand-in, and so a network problem can be
    /// reproduced against a proxy.
    #[arg(long, hide = true)]
    api_base_url: Option<String>,
}

fn parse_key_profile(s: &str) -> Result<KeyProfile, String> {
    KeyProfile::parse(s).ok_or_else(|| format!("unknown key profile: {s}"))
}

impl CommonOpts {
    fn analyzer(&self) -> Result<Analyzer> {
        self.analyzer_with(self.config()?)
    }

    /// An analyzer that never reads or writes cached analyses.
    ///
    /// `validate` measures the algorithm, so a cached row measures nothing and
    /// looks identical in the output. An accuracy check should not be able to
    /// pass on stale data because someone forgot an `ALGORITHM_VERSION` bump.
    fn fresh_analyzer(&self) -> Result<Analyzer> {
        self.analyzer_with(self.fresh_config()?)
    }

    /// [`Self::config`] with the analysis cache switched off and the key
    /// scoring breakdown switched on.
    ///
    /// Only the diagnostic commands take this path, which is exactly where the
    /// breakdown is wanted and where a cache would hide it.
    fn fresh_config(&self) -> Result<AnalyzerConfig> {
        let config = self.config()?;
        Ok(AnalyzerConfig {
            cache_path: None,
            analysis: AnalysisOptions {
                explain_key_scoring: true,
                ..config.analysis
            },
            ..config
        })
    }

    fn analyzer_with(&self, config: AnalyzerConfig) -> Result<Analyzer> {
        let analyzer = Analyzer::new(&config)?;
        Ok(match &self.api_base_url {
            Some(url) => analyzer.with_base_url(url.clone()),
            None => analyzer,
        })
    }

    fn config(&self) -> Result<AnalyzerConfig> {
        let cache_path = if self.no_cache {
            None
        } else {
            match self.cache_path.clone() {
                Some(p) => Some(p),
                None => Some(metrognome::cache::default_path().context("locating cache")?),
            }
        };
        Ok(AnalyzerConfig {
            requests_per_minute: self.requests_per_minute,
            burst: self.burst,
            analysis: AnalysisOptions {
                key_profile: self.key_profile,
                explain_key_scoring: self.explain_key,
            },
            cache_path,
        })
    }
}

fn init_logging(verbose: u8) {
    let default = match verbose {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("METROGNOME_LOG").unwrap_or_else(|_| EnvFilter::new(default)),
        )
        // stdout is the JSON channel; everything human-readable goes to stderr.
        .with_writer(std::io::stderr)
        .init();
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    init_logging(cli.verbose);

    match cli.command {
        Command::Analyze {
            artist,
            title,
            track_id,
            client_ref,
            opts,
        } => {
            let analyzer = opts.analyzer()?;
            let result = analyzer
                .analyze(Query {
                    artist,
                    title,
                    track_id,
                    client_ref,
                })
                .await;
            println!("{}", serde_json::to_string(&result)?);
            // A failed lookup is a reportable result, not a crashed process:
            // stdout still carries a well-formed object. The exit code is what
            // a shell caller checks.
            if result.status != "ok" {
                std::process::exit(1);
            }
        }

        Command::Batch { concurrency, opts } => {
            batch(concurrency.max(1), &opts).await?;
        }

        Command::Validate { opts } => {
            let analyzer = opts.fresh_analyzer()?;
            let mut rows = Vec::new();
            for t in metrognome::validate::REFERENCE_TRACKS {
                tracing::info!(artist = t.artist, title = t.title, "validating");
                let result = analyzer
                    .analyze(Query {
                        artist: Some(t.artist.to_string()),
                        title: Some(t.title.to_string()),
                        ..Default::default()
                    })
                    .await;
                let mut row = metrognome::validate::row(
                    format!("{} — {}", t.artist, t.title),
                    t.genre,
                    t.expected_bpm,
                    t.expected_key,
                    &result.features,
                );
                row.error = result.error.map(|e| format!("{}: {}", e.kind, e.message));
                // Carry what the query actually resolved to. Without it a bad
                // match and a bad estimate are indistinguishable in the table.
                if let (Some(track), Some(audio)) = (&result.track, &result.audio) {
                    row.matched = Some(metrognome::validate::MatchedTrack::of(
                        track,
                        audio,
                        &result.features,
                    ));
                }
                rows.push(row);
            }
            report(&rows)?;
        }

        Command::Selftest {
            sample_rate,
            key_profile,
        } => {
            let rows = metrognome::validate::selftest(
                sample_rate,
                &AnalysisOptions {
                    key_profile,
                    explain_key_scoring: true,
                },
            );
            report(&rows)?;
        }

        Command::Probe { url } => {
            let client = metrognome::fetch::client()?;
            let bytes = metrognome::fetch::fetch_bytes(&client, &url)
                .await
                .with_context(|| format!("fetching {url}"))?;
            let byte_len = bytes.len();
            let pcm =
                tokio::task::spawn_blocking(move || metrognome::decode_bytes(bytes, Some("m4a")))
                    .await
                    .context("decode task panicked")?
                    .context("decoding preview")?;

            let out = serde_json::json!({
                "url": url,
                "bytes": byte_len,
                "pcm": pcm.stats(),
            });
            println!("{}", serde_json::to_string(&out)?);
        }
    }
    Ok(())
}

/// Print a validation report: JSON on stdout, the table on stderr.
///
/// The split keeps the "stdout is JSON only" rule intact even for a command
/// whose whole point is to be read by a person.
fn report(rows: &[metrognome::validate::ValidationRow]) -> Result<()> {
    eprintln!("\n{}", metrognome::validate::render_table(rows));
    // Tempo and key are reported separately because they are validated to
    // different standards. Only tempo gates the run.
    let summary = metrognome::validate::Summary::of(rows);
    let failures = summary.failures();
    eprintln!(
        "tempo: {} of {} within {} BPM",
        summary.tempo_ok,
        summary.total,
        metrognome::validate::BPM_TOLERANCE
    );
    if summary.key_checked > 0 {
        eprintln!(
            "key:   {} of {} agreed (provisional, does not gate)",
            summary.key_agreed, summary.key_checked
        );
    }
    // Every row that produced a key, failure or not: a low confidence on a
    // track carrying no expected key is not a failure but is still the thing
    // worth reading.
    let scoring = metrognome::validate::render_key_scoring(rows);
    if !scoring.is_empty() {
        eprintln!("\nkey scoring\n{scoring}");
    }
    // The table says which rows are wrong; this says what they were wrong
    // about. Failures only — a passing row needs no explaining.
    let diagnostics = metrognome::validate::render_diagnostics(rows);
    if !diagnostics.is_empty() {
        eprintln!("\n{diagnostics}");
    }
    eprintln!();
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "schema_version": metrognome::SCHEMA_VERSION,
            "algorithm_version": metrognome::ALGORITHM_VERSION,
            "rows": rows,
            // Tempo failures only, matching the exit status.
            "failures": failures,
            "tempo_ok": summary.tempo_ok,
            "key_checked": summary.key_checked,
            "key_agreed": summary.key_agreed,
        }))?
    );
    if failures > 0 {
        std::process::exit(1);
    }
    Ok(())
}

/// One input line's fate: either a query to run, or a parse failure to report.
enum Line {
    Query(Query),
    Malformed(String),
}

async fn batch(concurrency: usize, opts: &CommonOpts) -> Result<()> {
    let analyzer = Arc::new(opts.analyzer()?);
    let permits = Arc::new(tokio::sync::Semaphore::new(concurrency));

    // Reading runs in its own task feeding a bounded channel, so the main loop
    // can wait on "next line" and "oldest result finished" at once. Selecting
    // on `Lines::next_line` directly is not cancel-safe and would drop input.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Line>(concurrency.max(1));
    tokio::spawn(async move {
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(raw)) => {
                    if raw.trim().is_empty() {
                        continue;
                    }
                    let parsed = match serde_json::from_str::<Query>(&raw) {
                        Ok(q) => Line::Query(q),
                        Err(e) => Line::Malformed(e.to_string()),
                    };
                    if tx.send(parsed).await.is_err() {
                        return;
                    }
                }
                Ok(None) => return,
                Err(e) => {
                    tracing::error!(error = %e, "reading stdin");
                    return;
                }
            }
        }
    });

    let mut pending: VecDeque<tokio::task::JoinHandle<serde_json::Value>> = VecDeque::new();
    let mut failures = 0usize;
    // One more in flight than there are permits, so a worker can start the
    // moment one finishes while its result is still being written.
    let window = concurrency.saturating_add(1).max(2);

    let spawn_one = |parsed: Line| {
        let analyzer = Arc::clone(&analyzer);
        let permits = Arc::clone(&permits);
        tokio::spawn(async move {
            match parsed {
                Line::Query(q) => {
                    let _permit = permits.acquire().await.expect("semaphore closed");
                    serde_json::to_value(analyzer.analyze(q).await).unwrap_or_else(error_value)
                }
                // One output line per input line, so positional readers stay
                // aligned. A full `Analysis` rather than an ad-hoc object, so
                // deserializing every line as `Analysis` cannot fail here.
                Line::Malformed(message) => serde_json::to_value(Analysis::failed(
                    Query::default(),
                    None,
                    &Error::InvalidInput(message),
                ))
                .unwrap_or_else(error_value),
            }
        })
    };

    let mut input_done = false;
    loop {
        // Always emit the oldest first, so output order matches input order
        // however the tasks finish.
        if pending.len() >= window || (input_done && !pending.is_empty()) {
            let handle = pending.pop_front().expect("non-empty");
            emit(handle.await, &mut failures);
            continue;
        }
        if input_done {
            break;
        }
        let Some(mut front) = pending.pop_front() else {
            match rx.recv().await {
                Some(parsed) => pending.push_back(spawn_one(parsed)),
                None => input_done = true,
            }
            continue;
        };
        tokio::select! {
            received = rx.recv() => {
                pending.push_front(front);
                match received {
                    Some(parsed) => pending.push_back(spawn_one(parsed)),
                    None => input_done = true,
                }
            }
            finished = &mut front => emit(finished, &mut failures),
        }
    }

    if failures > 0 {
        tracing::warn!(failures, "some tracks could not be analyzed");
    }
    Ok(())
}

/// Print one result line, counting it if it was not a success.
fn emit(finished: Result<serde_json::Value, tokio::task::JoinError>, failures: &mut usize) {
    let value = finished.unwrap_or_else(error_value);
    if value.get("status").and_then(|s| s.as_str()) != Some("ok") {
        *failures += 1;
    }
    println!("{value}");
}

/// Last-resort result object for a failure with no query to attribute it to:
/// a panicked analysis task, or a result that would not serialize.
fn error_value(e: impl std::fmt::Display) -> serde_json::Value {
    let analysis = Analysis::failed(Query::default(), None, &Error::Internal(e.to_string()));
    serde_json::to_value(&analysis).unwrap_or_else(|_| {
        serde_json::json!({
            "schema_version": metrognome::SCHEMA_VERSION,
            "algorithm_version": metrognome::ALGORITHM_VERSION,
            "status": "error",
            "query": {},
            "features": {},
            "cached": false,
            "error": { "kind": "internal", "message": e.to_string() },
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn opts(args: &[&str]) -> CommonOpts {
        #[derive(Parser)]
        struct Wrapper {
            #[command(flatten)]
            opts: CommonOpts,
        }
        let mut argv = vec!["metrognome"];
        argv.extend_from_slice(args);
        Wrapper::parse_from(argv).opts
    }

    #[test]
    fn explain_key_is_off_unless_asked_and_always_on_for_diagnostics() {
        assert!(!opts(&[]).config().unwrap().analysis.explain_key_scoring);
        assert!(
            opts(&["--explain-key"])
                .config()
                .unwrap()
                .analysis
                .explain_key_scoring
        );
        // `validate` and `selftest` want it regardless of the flag.
        assert!(
            opts(&[])
                .fresh_config()
                .unwrap()
                .analysis
                .explain_key_scoring
        );
    }

    #[test]
    fn validate_never_reads_a_cached_analysis() {
        // The guarantee lives here rather than in anyone's memory of the
        // ALGORITHM_VERSION rule, which has been forgotten once already.
        let o = opts(&[]);
        assert!(
            o.config().unwrap().cache_path.is_some(),
            "analyze and batch should still cache"
        );
        assert!(
            o.fresh_config().unwrap().cache_path.is_none(),
            "validate must not read cached analyses"
        );

        // An explicit --cache-path does not re-enable it for validate either.
        let o = opts(&["--cache-path", "/tmp/should-be-ignored.db"]);
        assert!(o.config().unwrap().cache_path.is_some());
        assert!(o.fresh_config().unwrap().cache_path.is_none());
    }
}
