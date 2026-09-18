//! Wiring: resolve, fetch, decode, analyze.
//!
//! [`analyze_pcm`] is pure DSP over samples and a sample rate, with no idea a
//! preview URL exists; [`Analyzer`] is the preview-specific orchestration on
//! top. That split is what lets a live-capture path reuse the DSP unchanged.

use std::path::PathBuf;
use std::sync::Mutex;

use crate::cache::{Cache, CachedAnalysis};
use crate::decode::decode_bytes;
use crate::dsp::{onset_envelope, Stft};
use crate::error::{Error, Result};
use crate::fetch;
use crate::key::{chromagram, estimate_key, KeyProfile};
use crate::ratelimit::{RateLimiter, DEFAULT_BURST, DEFAULT_PER_MINUTE};
use crate::resolve::Resolver;
use crate::tempo::estimate_tempo;
use crate::types::{Analysis, AudioInfo, Features, Query, TrackMatch};

/// Knobs for the DSP itself, as opposed to the network around it.
#[derive(Debug, Clone, Copy, Default)]
pub struct AnalysisOptions {
    /// Which key profile set to correlate against.
    pub key_profile: KeyProfile,
}

impl AnalysisOptions {
    /// A stable string identifying these options, used as part of the cache
    /// key. Anything that changes the output must appear here, or a cache hit
    /// will serve an answer produced under different settings.
    pub fn cache_key(&self) -> String {
        format!("key_profile={:?}", self.key_profile).to_ascii_lowercase()
    }
}

/// Estimate every supported feature from raw mono PCM, with default options.
///
/// Anything that needs to know where audio came from belongs above this line.
pub fn analyze_pcm(samples: &[f32], sample_rate: u32) -> Features {
    analyze_pcm_with(samples, sample_rate, &AnalysisOptions::default())
}

/// As [`analyze_pcm`], with explicit options.
pub fn analyze_pcm_with(samples: &[f32], sample_rate: u32, options: &AnalysisOptions) -> Features {
    // One non-finite sample poisons every downstream sum, and the result is a
    // NaN confidence that serializes as `null` — which this crate's own types
    // then refuse to parse. One pass is nothing next to two STFTs, and this is
    // the gate a live-capture tap passes through as well as a decoded preview.
    let sanitized: Option<Vec<f32>> = if samples.iter().all(|s| s.is_finite()) {
        None
    } else {
        Some(
            samples
                .iter()
                .map(|s| if s.is_finite() { *s } else { 0.0 })
                .collect(),
        )
    };
    let samples = sanitized.as_deref().unwrap_or(samples);

    // Tempo and key share nothing but the samples, and each is dominated by its
    // own STFT, so they are worth running side by side.
    let (tempo, key) = rayon::join(
        || {
            let stft = Stft::for_onsets(sample_rate);
            let env = onset_envelope(&stft.magnitudes(samples, sample_rate));
            estimate_tempo(&env)
        },
        || {
            let stft = Stft::for_chroma(sample_rate);
            let chroma = chromagram(&stft.magnitudes(samples, sample_rate));
            estimate_key(&chroma, options.key_profile)
        },
    );
    Features { tempo, key }
}

/// How the analyzer talks to the outside world.
#[derive(Debug, Clone)]
pub struct AnalyzerConfig {
    /// iTunes API requests per minute.
    pub requests_per_minute: f64,
    /// How many requests may be issued back to back from idle.
    pub burst: f64,
    /// DSP options passed down to [`analyze_pcm_with`].
    pub analysis: AnalysisOptions,
    /// Where to keep the result cache. `None` disables caching entirely.
    pub cache_path: Option<PathBuf>,
}

impl Default for AnalyzerConfig {
    fn default() -> Self {
        AnalyzerConfig {
            requests_per_minute: DEFAULT_PER_MINUTE,
            burst: DEFAULT_BURST,
            analysis: AnalysisOptions::default(),
            cache_path: crate::cache::default_path().ok(),
        }
    }
}

/// End-to-end preview analysis.
pub struct Analyzer {
    client: reqwest::Client,
    resolver: Resolver,
    analysis: AnalysisOptions,
    // SQLite calls here are microseconds and never span an await, so a plain
    // mutex is the right tool; an async one would only add ceremony.
    cache: Option<Mutex<Cache>>,
}

impl Analyzer {
    /// Build an analyzer with its own HTTP client and rate limiter.
    pub fn new(config: &AnalyzerConfig) -> Result<Self> {
        let client = fetch::client()?;
        let resolver = Resolver::new(
            client.clone(),
            RateLimiter::new(config.requests_per_minute, config.burst),
        );
        let cache = match &config.cache_path {
            Some(path) => Some(Mutex::new(Cache::open(path)?)),
            None => None,
        };
        Ok(Analyzer {
            client,
            resolver,
            analysis: config.analysis,
            cache,
        })
    }

    /// Point resolution at a different origin. For tests.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.resolver = self.resolver.with_base_url(base_url);
        self
    }

    /// Resolve, fetch and analyze one query.
    ///
    /// Never returns `Err`: a failure is an [`Analysis`] with
    /// `status: "error"`, so one bad track cannot kill a batch.
    pub async fn analyze(&self, query: Query) -> Analysis {
        let track = match self.resolve(&query).await {
            Ok(t) => t,
            Err(e) => return Analysis::failed(query, None, &e),
        };

        let options_key = self.analysis.cache_key();
        if let Some(hit) = self.cached(track.track_id, &options_key) {
            // Features and audio come from the cache; the track does not. The
            // cache is keyed by store track ID, so two queries share a row, but
            // `match_score` describes how well *this* query matched.
            let mut out = Analysis::ok(query, Some(track), hit.features, hit.audio);
            out.cached = true;
            return out;
        }

        match self.analyze_resolved(&track).await {
            Ok((features, audio)) => {
                self.store(
                    track.track_id,
                    &options_key,
                    &CachedAnalysis {
                        track: track.clone(),
                        features: features.clone(),
                        audio: audio.clone(),
                    },
                );
                Analysis::ok(query, Some(track), features, audio)
            }
            Err(e) => Analysis::failed(query, Some(track), &e),
        }
    }

    /// Key under which a query's *resolution* is cached.
    ///
    /// Normalized so that casing and spacing differences between a library's
    /// metadata and a previous run do not miss.
    fn resolution_key(query: &Query) -> Option<String> {
        // A track ID identifies rather than describes, so matching never comes
        // into it and the key needs no matcher version.
        if let Some(id) = query.track_id {
            return Some(format!("id:{id}"));
        }
        let artist = query.artist.as_deref()?.trim().to_lowercase();
        let title = query.title.as_deref()?.trim().to_lowercase();
        if title.is_empty() {
            return None;
        }
        // Unit separator: cannot appear in metadata, so "a b"+"c" and "a"+"b c"
        // cannot collide.
        let v = crate::resolve::MATCHER_VERSION;
        Some(format!("q{v}:{artist}\u{1}{title}"))
    }

    async fn resolve(&self, query: &Query) -> Result<TrackMatch> {
        let key = Self::resolution_key(query);
        if let Some(key) = &key {
            if let Some(cache) = &self.cache {
                if let Ok(guard) = cache.lock() {
                    if let Ok(Some(hit)) = guard.get_resolution(key) {
                        return Ok(hit);
                    }
                }
            }
        }

        let resolved = if let Some(id) = query.track_id {
            self.resolver.lookup(id).await
        } else {
            match (query.artist.as_deref(), query.title.as_deref()) {
                (Some(artist), Some(title)) if !title.trim().is_empty() => {
                    self.resolver.search(artist, title).await
                }
                _ => Err(Error::InvalidInput(
                    "need either track_id, or both artist and title".into(),
                )),
            }
        }?;

        if let (Some(key), Some(cache)) = (&key, &self.cache) {
            if let Ok(guard) = cache.lock() {
                // A cache write failure is not worth failing an analysis over;
                // the cost is one repeated request next time.
                if let Err(e) = guard.put_resolution(key, &resolved) {
                    tracing::warn!(error = %e, "could not cache resolution");
                }
            }
        }
        Ok(resolved)
    }

    fn cached(&self, track_id: i64, options_key: &str) -> Option<CachedAnalysis> {
        let cache = self.cache.as_ref()?;
        let guard = cache.lock().ok()?;
        guard.get(track_id, options_key).ok().flatten()
    }

    fn store(&self, track_id: i64, options_key: &str, value: &CachedAnalysis) {
        let Some(cache) = &self.cache else { return };
        let Ok(guard) = cache.lock() else { return };
        if let Err(e) = guard.put(track_id, options_key, value) {
            tracing::warn!(error = %e, "could not cache analysis");
        }
    }

    /// Download and analyze a track that has already been resolved.
    pub async fn analyze_resolved(&self, track: &TrackMatch) -> Result<(Features, AudioInfo)> {
        let url = track.preview_url.clone().ok_or(Error::NoPreview {
            track_id: track.track_id,
        })?;
        let bytes = fetch::fetch_bytes(&self.client, &url).await?;
        let options = self.analysis;

        // Decode and DSP are CPU-bound and would otherwise block the reactor
        // for the whole of a ~200 ms analysis while other downloads wait.
        tokio::task::spawn_blocking(move || {
            let pcm = decode_bytes(bytes, Some("m4a"))?;
            let stats = pcm.stats();
            let features = analyze_pcm_with(&pcm.samples, pcm.sample_rate, &options);
            Ok((
                features,
                AudioInfo {
                    duration_secs: stats.duration_secs,
                    sample_rate: stats.sample_rate,
                    source_channels: stats.source_channels,
                    silent_fraction: stats.silent_fraction,
                    truncated: pcm.truncated,
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
    fn analyze_pcm_reports_tempo_and_key_for_a_full_arrangement() {
        // Drums plus a sustained chord progression: each feature has to find
        // its own evidence with the other's all over the spectrum.
        let sr = 44_100;
        let mut sig = testsig::groove(124.0, 24.0, sr, Groove::FourOnFloor);
        let chords = testsig::chord_progression(5, testsig::Quality::Minor, 24.0, sr);
        testsig::mix_at(&mut sig, &chords, 0);

        let f = analyze_pcm(&sig, sr);
        let tempo = f.tempo.expect("tempo");
        assert!((tempo.bpm - 124.0).abs() < 1.0, "got {}", tempo.bpm);
        let key = f.key.expect("key");
        assert_eq!(key.key, "F minor", "conf {}", key.confidence);
        assert_eq!(key.camelot, "4A");
    }

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
    fn non_finite_pcm_never_produces_a_confident_feature_or_unparseable_json() {
        // Float WAV can carry these, and symphonia's `pcm` feature decodes it.
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let features = analyze_pcm(&vec![bad; 441_000], 44_100);
            let json = serde_json::to_string(&features)
                .unwrap_or_else(|e| panic!("{bad} did not serialize: {e}"));
            // The contract type has to be able to read back what we emit;
            // a NaN confidence serializes as `null` and fails right here.
            serde_json::from_str::<Features>(&json)
                .unwrap_or_else(|e| panic!("{bad} emitted unparseable JSON: {json} ({e})"));
            if let Some(k) = &features.key {
                assert!(k.confidence.is_finite() && k.uncertain, "{bad}: {k:?}");
            }
            if let Some(t) = &features.tempo {
                assert!(t.confidence.is_finite() && t.uncertain, "{bad}: {t:?}");
            }
        }
    }

    #[test]
    fn a_single_non_finite_sample_does_not_poison_a_good_clip() {
        let sr = 44_100;
        let mut samples = crate::testsig::click_track(128.0, 20.0, sr);
        samples[sr as usize] = f32::NAN;
        let tempo = analyze_pcm(&samples, sr).tempo.expect("tempo");
        assert!((tempo.bpm - 128.0).abs() < 1.0, "got {}", tempo.bpm);
    }

    #[test]
    fn analyze_pcm_on_silence_reports_no_confident_tempo() {
        let f = analyze_pcm(&vec![0.0f32; 44_100 * 5], 44_100);
        match f.tempo {
            None => {}
            Some(t) => assert!(t.uncertain, "{t:?}"),
        }
    }

    fn test_config() -> AnalyzerConfig {
        AnalyzerConfig {
            cache_path: None,
            ..Default::default()
        }
    }

    #[test]
    fn the_cache_key_changes_with_anything_that_changes_the_output() {
        let a = AnalysisOptions {
            key_profile: KeyProfile::Edm,
        };
        let b = AnalysisOptions {
            key_profile: KeyProfile::Krumhansl,
        };
        assert_ne!(a.cache_key(), b.cache_key());
    }

    #[test]
    fn resolution_keys_normalize_case_and_spacing() {
        let a = Query {
            artist: Some("Daft Punk".into()),
            title: Some("Around the World".into()),
            ..Default::default()
        };
        let b = Query {
            artist: Some("  daft punk ".into()),
            title: Some("AROUND THE WORLD  ".into()),
            ..Default::default()
        };
        assert_eq!(Analyzer::resolution_key(&a), Analyzer::resolution_key(&b));
        // An ID is its own key and never collides with a text query.
        let c = Query {
            track_id: Some(5),
            ..Default::default()
        };
        assert_eq!(Analyzer::resolution_key(&c).unwrap(), "id:5");
        assert!(Analyzer::resolution_key(&Query::default()).is_none());
    }

    #[tokio::test]
    async fn a_query_with_neither_id_nor_title_is_an_input_error() {
        let a = Analyzer::new(&test_config()).unwrap();
        let out = a.analyze(Query::default()).await;
        assert_eq!(out.status, "error");
        assert_eq!(out.error.unwrap().kind, "invalid_input");
    }

    #[tokio::test]
    async fn a_resolved_track_without_a_preview_fails_cleanly() {
        let a = Analyzer::new(&test_config()).unwrap();
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
