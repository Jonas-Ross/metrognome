//! `metrognome` CLI.
//!
//! Contract with selecta (and any other machine consumer): **stdout carries
//! nothing but JSON**. Logs, progress, and diagnostics go to stderr. Breaking
//! that is breaking the interface.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
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
        net: NetOpts,
    },
    /// Download a preview clip and print decoded PCM statistics as JSON.
    Probe {
        /// Direct URL to an audio clip.
        #[arg(long)]
        url: String,
    },
}

#[derive(clap::Args, Debug, Clone)]
struct NetOpts {
    /// iTunes API requests per minute. The documented ceiling is around 20;
    /// raising this risks the address being throttled.
    #[arg(long, default_value_t = metrognome::ratelimit::DEFAULT_PER_MINUTE)]
    requests_per_minute: f64,
    /// How many requests may be issued back to back from idle.
    #[arg(long, default_value_t = metrognome::ratelimit::DEFAULT_BURST)]
    burst: f64,
}

impl From<&NetOpts> for metrognome::AnalyzerConfig {
    fn from(o: &NetOpts) -> Self {
        metrognome::AnalyzerConfig {
            requests_per_minute: o.requests_per_minute,
            burst: o.burst,
        }
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
            net,
        } => {
            let analyzer = metrognome::Analyzer::new(&(&net).into())?;
            let result = analyzer
                .analyze(metrognome::Query {
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
