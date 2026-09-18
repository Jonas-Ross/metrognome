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
    /// Download a preview clip and print decoded PCM statistics as JSON.
    Probe {
        /// Direct URL to an audio clip.
        #[arg(long)]
        url: String,
    },
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
