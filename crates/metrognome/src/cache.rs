//! On-disk result cache, keyed by resolved store track ID.
//!
//! Caches the result, never the audio, so re-scanning a library of tens of
//! thousands of tracks is free the second time. A row hits only when the
//! algorithm version and analysis options both match, so a DSP change can
//! never silently serve estimates from code that no longer exists.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension};

use crate::error::{Error, Result};
use crate::types::{AudioInfo, Features, TrackMatch};
use crate::ALGORITHM_VERSION;

/// What a cache row holds. The query is deliberately not part of it: two
/// different queries that resolve to the same track share one analysis.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct CachedAnalysis {
    /// The resolved track.
    pub track: TrackMatch,
    /// The features that were measured.
    pub features: Features,
    /// Properties of the audio they came from.
    pub audio: AudioInfo,
}

/// SQLite-backed result cache.
pub struct Cache {
    conn: Connection,
}

impl Cache {
    /// Open (and migrate) the cache at `path`, creating parent directories.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Cache(format!("creating {}: {e}", parent.display())))?;
        }
        let conn = Connection::open(path)
            .map_err(|e| Error::Cache(format!("opening {}: {e}", path.display())))?;
        Self::init(conn)
    }

    /// An in-memory cache. Used by tests and by `--no-cache` callers that still
    /// want the code path exercised.
    pub fn in_memory() -> Result<Self> {
        let conn =
            Connection::open_in_memory().map_err(|e| Error::Cache(format!("in-memory: {e}")))?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS results (
                 track_id          INTEGER NOT NULL,
                 algorithm_version INTEGER NOT NULL,
                 options           TEXT    NOT NULL,
                 payload           TEXT    NOT NULL,
                 created_at        TEXT    NOT NULL DEFAULT (datetime('now')),
                 PRIMARY KEY (track_id, algorithm_version, options)
             );
             CREATE TABLE IF NOT EXISTS resolutions (
                 query_key  TEXT PRIMARY KEY,
                 matched    TEXT NOT NULL,
                 created_at TEXT NOT NULL DEFAULT (datetime('now'))
             );",
        )
        .map_err(|e| Error::Cache(format!("migrating: {e}")))?;
        Ok(Cache { conn })
    }

    /// Fetch a cached analysis, or `None` for a miss.
    pub fn get(&self, track_id: i64, options: &str) -> Result<Option<CachedAnalysis>> {
        let payload: Option<String> = self
            .conn
            .query_row(
                "SELECT payload FROM results
                 WHERE track_id = ?1 AND algorithm_version = ?2 AND options = ?3",
                (track_id, ALGORITHM_VERSION, options),
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| Error::Cache(format!("reading {track_id}: {e}")))?;

        match payload {
            None => Ok(None),
            // A row written by an older serialization that no longer parses is
            // a miss, not a failure: re-analyzing costs a request, while
            // erroring out would wedge the consumer until it cleared the file.
            Some(p) => Ok(serde_json::from_str(&p).ok()),
        }
    }

    /// Store an analysis, replacing any previous row for the same key.
    pub fn put(&self, track_id: i64, options: &str, value: &CachedAnalysis) -> Result<()> {
        let payload = serde_json::to_string(value)
            .map_err(|e| Error::Cache(format!("serializing {track_id}: {e}")))?;
        self.conn
            .execute(
                "INSERT INTO results (track_id, algorithm_version, options, payload)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (track_id, algorithm_version, options)
                 DO UPDATE SET payload = excluded.payload, created_at = datetime('now')",
                (track_id, ALGORITHM_VERSION, options, payload),
            )
            .map_err(|e| Error::Cache(format!("writing {track_id}: {e}")))?;
        Ok(())
    }

    /// Fetch a cached resolution for a query, or `None`.
    ///
    /// Cached separately from analysis because it is the rate-limited step:
    /// ten thousand tracks against a warm analysis cache would still be nine
    /// hours of waiting on the search API.
    pub fn get_resolution(&self, query_key: &str) -> Result<Option<TrackMatch>> {
        let matched: Option<String> = self
            .conn
            .query_row(
                "SELECT matched FROM resolutions WHERE query_key = ?1",
                (query_key,),
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| Error::Cache(format!("reading resolution {query_key}: {e}")))?;
        Ok(matched.and_then(|m| serde_json::from_str(&m).ok()))
    }

    /// Store a resolution.
    pub fn put_resolution(&self, query_key: &str, matched: &TrackMatch) -> Result<()> {
        let payload = serde_json::to_string(matched)
            .map_err(|e| Error::Cache(format!("serializing resolution: {e}")))?;
        self.conn
            .execute(
                "INSERT INTO resolutions (query_key, matched) VALUES (?1, ?2)
                 ON CONFLICT (query_key) DO UPDATE
                 SET matched = excluded.matched, created_at = datetime('now')",
                (query_key, payload),
            )
            .map_err(|e| Error::Cache(format!("writing resolution {query_key}: {e}")))?;
        Ok(())
    }

    /// Number of rows, for diagnostics.
    pub fn len(&self) -> Result<i64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM results", [], |r| r.get(0))
            .map_err(|e| Error::Cache(format!("counting: {e}")))
    }

    /// Whether the cache holds nothing.
    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }
}

/// Where the cache lives when the caller does not say.
///
/// Follows the platform convention rather than inventing one, so a user can
/// find and delete it without being told where to look.
pub fn default_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| Error::Cache("HOME is not set; pass an explicit cache path".into()))?;
    let dir = if cfg!(target_os = "macos") {
        home.join("Library/Caches/metrognome")
    } else if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME") {
        PathBuf::from(xdg).join("metrognome")
    } else {
        home.join(".cache/metrognome")
    };
    Ok(dir.join("cache.db"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TempoEstimate;

    fn sample() -> CachedAnalysis {
        CachedAnalysis {
            track: TrackMatch {
                track_id: 7,
                artist: "Act".into(),
                title: "Track".into(),
                album: None,
                release_date: None,
                genre: None,
                preview_url: Some("https://example/p.m4a".into()),
                duration_ms: Some(300_000),
                match_score: 1.0,
                uncertain: false,
            },
            features: Features {
                tempo: Some(TempoEstimate {
                    bpm: 174.0,
                    confidence: 0.9,
                    uncertain: false,
                    maturity: crate::types::Maturity::Validated,
                    source: "test".into(),
                    beat_offset_secs: 0.1,
                    canonical_window_bpm: [90.0, 180.0],
                    alternates: vec![],
                }),
                key: None,
            },
            audio: AudioInfo {
                duration_secs: 30.0,
                sample_rate: 44_100,
                source_channels: 2,
                silent_fraction: 0.0,
                truncated: false,
            },
        }
    }

    #[test]
    fn round_trips_a_result() {
        let c = Cache::in_memory().unwrap();
        assert!(c.is_empty().unwrap());
        assert!(c.get(7, "default").unwrap().is_none());

        c.put(7, "default", &sample()).unwrap();
        assert_eq!(c.get(7, "default").unwrap().unwrap(), sample());
        assert_eq!(c.len().unwrap(), 1);
    }

    #[test]
    fn different_options_are_different_rows() {
        let c = Cache::in_memory().unwrap();
        c.put(7, "key_profile=edm", &sample()).unwrap();
        // Key depends on the profile set, so a different profile must not be
        // served the previous answer.
        assert!(c.get(7, "key_profile=krumhansl").unwrap().is_none());
        assert!(c.get(7, "key_profile=edm").unwrap().is_some());
    }

    #[test]
    fn writing_the_same_key_twice_replaces_rather_than_duplicates() {
        let c = Cache::in_memory().unwrap();
        c.put(7, "default", &sample()).unwrap();
        let mut updated = sample();
        updated.features.tempo.as_mut().unwrap().bpm = 128.0;
        c.put(7, "default", &updated).unwrap();
        assert_eq!(c.len().unwrap(), 1);
        assert_eq!(
            c.get(7, "default").unwrap().unwrap().features,
            updated.features
        );
    }

    #[test]
    fn an_unparseable_payload_is_a_miss_not_an_error() {
        let c = Cache::in_memory().unwrap();
        c.conn
            .execute(
                "INSERT INTO results (track_id, algorithm_version, options, payload)
                 VALUES (?1, ?2, 'default', 'not json')",
                (7, ALGORITHM_VERSION),
            )
            .unwrap();
        assert!(c.get(7, "default").unwrap().is_none());
    }

    #[test]
    fn a_stale_algorithm_version_does_not_hit() {
        let c = Cache::in_memory().unwrap();
        c.conn
            .execute(
                "INSERT INTO results (track_id, algorithm_version, options, payload)
                 VALUES (7, ?1, 'default', ?2)",
                (
                    ALGORITHM_VERSION + 1,
                    serde_json::to_string(&sample()).unwrap(),
                ),
            )
            .unwrap();
        assert!(c.get(7, "default").unwrap().is_none());
    }

    #[test]
    fn resolutions_round_trip_independently_of_results() {
        let c = Cache::in_memory().unwrap();
        assert!(c.get_resolution("act\u{1}track").unwrap().is_none());
        c.put_resolution("act\u{1}track", &sample().track).unwrap();
        assert_eq!(
            c.get_resolution("act\u{1}track").unwrap().unwrap(),
            sample().track
        );
        // Caching a resolution does not imply a cached analysis.
        assert!(c.get(7, "default").unwrap().is_none());
    }

    #[test]
    fn opening_creates_the_parent_directory() {
        let dir = std::env::temp_dir().join(format!("mg-cache-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested/cache.db");
        let c = Cache::open(&path).unwrap();
        c.put(1, "default", &sample()).unwrap();
        assert!(path.exists());
        drop(c);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
