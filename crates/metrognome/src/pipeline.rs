//! Wiring: resolve, fetch, decode, analyze.
//!
//! The split here is the point of the crate. [`analyze_pcm`] is pure DSP over
//! samples and a sample rate — it has no idea a preview URL exists, which is
//! what lets a future live-capture path reuse it unchanged. [`Analyzer`] is the
//! preview-specific orchestration layered on top.

use crate::decode::decode_bytes;
use crate::dsp::{onset_envelope, Stft};
use crate::error::{Error, Result};
use crate::fetch;
use crate::ratelimit::{RateLimiter, DEFAULT_BURST, DEFAULT_PER_MINUTE};
use crate::resolve::Resolver;
use crate::tempo::estimate_tempo;
use crate::types::{Analysis, AudioInfo, Features, Query, TrackMatch};

/// Estimate every supported feature from raw mono PCM.
///
/// Takes samples and a sample rate and nothing else. Anything that needs to
/// know where audio came from belongs above this line, not below it.
pub fn analyze_pcm(samples: &[f32], sample_rate: u32) -> Features {
    let onset_stft = Stft::for_onsets(sample_rate);
    let env = onset_envelope(&onset_stft.magnitudes(samples, sample_rate));
    Features {
        tempo: estimate_tempo(&env),
        key: None,
    }
}

/// How the analyzer talks to the outside world.
#[derive(Debug, Clone)]
pub struct AnalyzerConfig {
    /// iTunes API requests per minute.
    pub requests_per_minute: f64,
    /// How many requests may be issued back to back from idle.
    pub burst: f64,
}

impl Default for AnalyzerConfig {
    fn default() -> Self {
        AnalyzerConfig {
            requests_per_minute: DEFAULT_PER_MINUTE,
            burst: DEFAULT_BURST,
        }
    }
}

/// End-to-end preview analysis.
pub struct Analyzer {
    client: reqwest::Client,
    resolver: Resolver,
}

impl Analyzer {
    /// Build an analyzer with its own HTTP client and rate limiter.
    pub fn new(config: &AnalyzerConfig) -> Result<Self> {
        let client = fetch::client()?;
        let resolver = Resolver::new(
            client.clone(),
            RateLimiter::new(config.requests_per_minute, config.burst),
        );
        Ok(Analyzer { client, resolver })
    }

    /// Point resolution at a different origin. For tests.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.resolver = self.resolver.with_base_url(base_url);
        self
    }

    /// Resolve, fetch and analyze one query.
    ///
    /// Never returns `Err`: a failure is an [`Analysis`] with `status: "error"`,
    /// because the batch path must be able to report one bad track without
    /// losing the rest.
    pub async fn analyze(&self, query: Query) -> Analysis {
        let track = match self.resolve(&query).await {
            Ok(t) => t,
            Err(e) => return Analysis::failed(query, None, &e),
        };
        match self.analyze_resolved(&track).await {
            Ok((features, audio)) => Analysis::ok(query, Some(track), features, audio),
            Err(e) => Analysis::failed(query, Some(track), &e),
        }
    }

    async fn resolve(&self, query: &Query) -> Result<TrackMatch> {
        if let Some(id) = query.track_id {
            return self.resolver.lookup(id).await;
        }
        match (query.artist.as_deref(), query.title.as_deref()) {
            (Some(artist), Some(title)) if !title.trim().is_empty() => {
                self.resolver.search(artist, title).await
            }
            _ => Err(Error::InvalidInput(
                "need either track_id, or both artist and title".into(),
            )),
        }
    }

    /// Download and analyze a track that has already been resolved.
    pub async fn analyze_resolved(&self, track: &TrackMatch) -> Result<(Features, AudioInfo)> {
        let url = track.preview_url.clone().ok_or(Error::NoPreview {
            track_id: track.track_id,
        })?;
        let bytes = fetch::fetch_bytes(&self.client, &url).await?;

        // Decode and DSP are CPU-bound and would otherwise block the reactor
        // for the whole of a ~200 ms analysis while other downloads wait.
        tokio::task::spawn_blocking(move || {
            let pcm = decode_bytes(bytes, Some("m4a"))?;
            let stats = pcm.stats();
            let features = analyze_pcm(&pcm.samples, pcm.sample_rate);
            Ok((
                features,
                AudioInfo {
                    duration_secs: stats.duration_secs,
                    sample_rate: stats.sample_rate,
                    source_channels: stats.source_channels,
                    silent_fraction: stats.silent_fraction,
                },
            ))
        })
        .await
        .map_err(|e| Error::Decode(format!("analysis task failed: {e}")))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testsig::{self, Groove};

    #[test]
    fn analyze_pcm_reports_tempo_for_a_groove() {
        let sr = 44_100;
        let sig = testsig::groove(128.0, 30.0, sr, Groove::FourOnFloor);
        let f = analyze_pcm(&sig, sr);
        let tempo = f.tempo.expect("tempo");
        assert!((tempo.bpm - 128.0).abs() < 1.0, "got {}", tempo.bpm);
        assert!(!tempo.uncertain);
    }

    #[test]
    fn analyze_pcm_is_sample_rate_agnostic() {
        // Same groove at a different rate must land on the same tempo; nothing
        // in the DSP may be tuned to 44.1 kHz.
        let sig = testsig::groove(124.0, 30.0, 48_000, Groove::FourOnFloor);
        let f = analyze_pcm(&sig, 48_000);
        assert!((f.tempo.unwrap().bpm - 124.0).abs() < 1.0);
    }

    #[test]
    fn analyze_pcm_on_silence_reports_no_confident_tempo() {
        let f = analyze_pcm(&vec![0.0f32; 44_100 * 5], 44_100);
        match f.tempo {
            None => {}
            Some(t) => assert!(t.uncertain, "{t:?}"),
        }
    }

    #[tokio::test]
    async fn a_query_with_neither_id_nor_title_is_an_input_error() {
        let a = Analyzer::new(&AnalyzerConfig::default()).unwrap();
        let out = a.analyze(Query::default()).await;
        assert_eq!(out.status, "error");
        assert_eq!(out.error.unwrap().kind, "invalid_input");
    }

    #[tokio::test]
    async fn a_resolved_track_without_a_preview_fails_cleanly() {
        let a = Analyzer::new(&AnalyzerConfig::default()).unwrap();
        let track = TrackMatch {
            track_id: 42,
            artist: "x".into(),
            title: "y".into(),
            album: None,
            release_date: None,
            genre: None,
            preview_url: None,
            duration_ms: None,
            match_score: 1.0,
            uncertain: false,
        };
        let err = a.analyze_resolved(&track).await.unwrap_err();
        assert_eq!(err.kind(), crate::ErrorKind::NoPreview);
    }
}
