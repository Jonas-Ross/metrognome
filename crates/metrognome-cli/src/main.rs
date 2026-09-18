//! `metrognome` CLI.
//!
//! Contract with selecta (and any other machine consumer): **stdout carries
//! nothing but JSON**. Logs, progress, and diagnostics go to stderr. Breaking
//! that is breaking the interface.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use metrognome::{AnalysisOptions, Analyzer, AnalyzerConfig, KeyProfile, Query};
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
        let analyzer = Analyzer::new(&self.config()?)?;
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

/// One input line's fate: either a query to run, or a parse failure to report.
enum Line {
    Query(Query),
    Malformed(String),
}

async fn batch(concurrency: usize, opts: &CommonOpts) -> Result<()> {
    let analyzer = Arc::new(opts.analyzer()?);
    let permits = Arc::new(tokio::sync::Semaphore::new(concurrency));

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut pending: Vec<tokio::task::JoinHandle<serde_json::Value>> = Vec::new();

    while let Some(raw) = lines.next_line().await.context("reading stdin")? {
        if raw.trim().is_empty() {
            continue;
        }
        let parsed = match serde_json::from_str::<Query>(&raw) {
            Ok(q) => Line::Query(q),
            Err(e) => Line::Malformed(e.to_string()),
        };
        let analyzer = Arc::clone(&analyzer);
        let permits = Arc::clone(&permits);
        pending.push(tokio::spawn(async move {
            match parsed {
                Line::Query(q) => {
                    let _permit = permits.acquire().await.expect("semaphore closed");
                    serde_json::to_value(analyzer.analyze(q).await).unwrap_or_else(error_value)
                }
                // A line that is not valid JSON still gets a result object, so
                // the output has exactly one line per input line and a consumer
                // reading them positionally never loses alignment.
                Line::Malformed(message) => serde_json::json!({
                    "schema_version": metrognome::SCHEMA_VERSION,
                    "status": "error",
                    "error": { "kind": "invalid_input", "message": message },
                }),
            }
        }));
    }

    // Awaited in order, so output order matches input order regardless of which
    // track finished first.
    let mut failures = 0usize;
    for handle in pending {
        let value = match handle.await {
            Ok(v) => v,
            Err(e) => error_value(e),
        };
        if value.get("status").and_then(|s| s.as_str()) != Some("ok") {
            failures += 1;
        }
        println!("{value}");
    }
    if failures > 0 {
        tracing::warn!(failures, "some tracks could not be analyzed");
    }
    Ok(())
}

fn error_value(e: impl std::fmt::Display) -> serde_json::Value {
    serde_json::json!({
        "schema_version": metrognome::SCHEMA_VERSION,
        "status": "error",
        "error": { "kind": "internal", "message": e.to_string() },
    })
}
