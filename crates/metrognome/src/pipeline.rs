//! Wiring: resolve, fetch, decode, analyze.
//!
//! [`analyze_pcm`](crate::analyze_pcm) is pure DSP over samples and a sample
//! rate, with no idea a preview URL exists; [`Analyzer`] is the preview-specific
//! orchestration on top. That split is what lets a live-capture path reuse the
//! DSP unchanged.

use std::path::PathBuf;
use std::sync::Mutex;

use crate::analyze::{analyze_pcm_with, AnalysisOptions};
use crate::cache::{Cache, CachedAnalysis};
use crate::decode::decode_bytes;
use crate::error::{Error, Result};
use crate::fetch;
use crate::ratelimit::{RateLimiter, DEFAULT_BURST, DEFAULT_PER_MINUTE};
use crate::resolve::{Country, Resolver};
use crate::types::{Analysis, AudioInfo, Features, Query, TrackMatch};

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
    /// iTunes storefront to resolve in. `None` is Apple's default, the US.
    pub country: Option<Country>,
}

impl Default for AnalyzerConfig {
    fn default() -> Self {
        AnalyzerConfig {
            requests_per_minute: DEFAULT_PER_MINUTE,
            burst: DEFAULT_BURST,
            analysis: AnalysisOptions::default(),
            cache_path: crate::cache::default_path().ok(),
            country: None,
        }
    }
}

/// End-to-end preview analysis.
pub struct Analyzer {
    client: reqwest::Client,
    resolver: Resolver,
    country: Option<Country>,
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
        )
        .with_country(config.country.clone());
        let cache = match &config.cache_path {
            Some(path) => Some(Mutex::new(Cache::open(path)?)),
            None => None,
        };
        Ok(Analyzer {
            client,
            resolver,
            country: config.country.clone(),
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

    async fn resolve(&self, query: &Query) -> Result<TrackMatch> {
        let key = resolution_key(query, self.country.as_ref());
        if let Some(hit) = key.as_deref().and_then(|k| self.cached_resolution(k)) {
            return Ok(hit);
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

        if let Some(key) = &key {
            self.store_resolution(key, &resolved);
        }
        Ok(resolved)
    }

    fn cached_resolution(&self, key: &str) -> Option<TrackMatch> {
        let guard = self.cache.as_ref()?.lock().ok()?;
        guard.get_resolution(key).ok().flatten()
    }

    fn store_resolution(&self, key: &str, value: &TrackMatch) {
        let Some(cache) = &self.cache else { return };
        let Ok(guard) = cache.lock() else { return };
        // A cache write failure is not worth failing an analysis over; the cost
        // is one repeated request next time.
        if let Err(e) = guard.put_resolution(key, value) {
            tracing::warn!(error = %e, "could not cache resolution");
        }
    }

    fn cached(&self, track_id: i64, options_key: &str) -> Option<CachedAnalysis> {
        let guard = self.cache.as_ref()?.lock().ok()?;
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
        // A panicked task is a metrognome bug, not bad audio. Reporting it as
        // `decode` would have a consumer blacklist the track for our fault.
        .map_err(|e| Error::Internal(format!("analysis task failed: {e}")))?
    }
}

/// Key under which a query's *resolution* is cached.
///
/// Normalized so that casing and spacing differences between a library's
/// metadata and a previous run do not miss.
fn resolution_key(query: &Query, country: Option<&Country>) -> Option<String> {
    // The same query can resolve differently, or only, in another store. The
    // default store carries no suffix so caches from before `--country` stay warm.
    let store = match country {
        Some(c) if !c.is_default() => format!("@{}", c.code()),
        _ => String::new(),
    };
    // A track ID identifies rather than describes, so matching never comes
    // into it and the key needs no matcher version.
    if let Some(id) = query.track_id {
        return Some(format!("id{store}:{id}"));
    }
    let artist = query.artist.as_deref()?.trim().to_lowercase();
    let title = query.title.as_deref()?.trim().to_lowercase();
    if title.is_empty() {
        return None;
    }
    // Unit separator: cannot appear in metadata, so "a b"+"c" and "a"+"b c"
    // cannot collide.
    let v = crate::resolve::MATCHER_VERSION;
    Some(format!("q{v}{store}:{artist}\u{1}{title}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> AnalyzerConfig {
        AnalyzerConfig {
            cache_path: None,
            ..Default::default()
        }
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
        assert_eq!(resolution_key(&a, None), resolution_key(&b, None));
        // An ID is its own key and never collides with a text query.
        let c = Query {
            track_id: Some(5),
            ..Default::default()
        };
        assert_eq!(resolution_key(&c, None).unwrap(), "id:5");
        assert!(resolution_key(&Query::default(), None).is_none());
    }

    #[test]
    fn resolution_keys_separate_stores_but_not_the_default_one() {
        let q = Query {
            artist: Some("Kaizers Orchestra".into()),
            title: Some("Ompa til du dør".into()),
            ..Default::default()
        };
        let id = Query {
            track_id: Some(5),
            ..Default::default()
        };
        let us = Country::parse("us");
        let no = Country::parse("no");
        for query in [&q, &id] {
            assert_eq!(
                resolution_key(query, us.as_ref()),
                resolution_key(query, None)
            );
            assert_ne!(
                resolution_key(query, no.as_ref()),
                resolution_key(query, None)
            );
        }
        assert_eq!(resolution_key(&id, no.as_ref()).unwrap(), "id@no:5");
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
