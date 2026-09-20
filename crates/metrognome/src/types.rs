//! The JSON contract with selecta — interface, not implementation detail.
//!
//! Every emitted object carries [`SCHEMA_VERSION`] so a consumer can refuse
//! what it does not understand, and every feature carries its own `source`,
//! `confidence`, `uncertain` and `maturity` rather than presenting a guess as
//! a fact. New features arrive as new optional fields; existing ones keep
//! their meaning.

use serde::{Deserialize, Serialize};

/// Version of the JSON objects this binary emits.
///
/// Bumped when the guarantees change: a field a consumer may now count on
/// being present, or one that moved or changed meaning. A field that may be
/// absent is not a bump, because nothing could have depended on it.
pub const SCHEMA_VERSION: u32 = 2;

/// Confidence at or below which a feature flags itself uncertain.
///
/// Set where a 30-second preview of a beatless intro lands: the consumer should
/// treat anything under this as a hint, not a measurement.
pub const UNCERTAIN_AT_OR_BELOW: f32 = 0.5;

/// Normalize a computed confidence into the finite 0-1 range the contract
/// promises.
///
/// Non-finite becomes 0. A NaN confidence serializes as `null`, which
/// [`Features`] refuses to deserialize, and compares false against every
/// threshold — so a guess with nothing behind it would ship as `uncertain:
/// false`.
pub fn normalize_confidence(c: f32) -> f32 {
    if c.is_finite() {
        (c.clamp(0.0, 1.0) * 1000.0).round() / 1000.0
    } else {
        0.0
    }
}

/// How far a feature's accuracy has been checked against real recordings.
///
/// Distinct from confidence, which is about one clip: this is about the
/// estimator. Tempo has published references that agree; key does not, so it
/// ships measured-but-unverified. A consumer reads this rather than hardcoding
/// which feature it trusts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Maturity {
    /// Checked against published references and passing.
    Validated,
    /// Emitted but unmeasured. Read it through `confidence` and `uncertain`,
    /// and do not write it anywhere a validated figure is implied.
    Provisional,
}

/// One alternate reading of a feature, with how it relates to the chosen one.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Alternate {
    /// The alternate as a number: BPM for tempo, Camelot position for key
    /// (so wheel distance is arithmetic rather than string parsing).
    pub value: f64,
    /// Display form, when the number alone does not identify the alternate.
    /// Absent for tempo, where the number is the whole answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Relation to the chosen value, e.g. `"half"`, `"double"`, `"relative_minor"`.
    pub relation: String,
    /// Raw score, comparable only against other alternates in the same list.
    pub score: f32,
}

/// The factors behind a tempo confidence, plus the raw inputs they came from.
///
/// Diagnostic only, and outside the JSON contract. See DECISIONS.md entry 36.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TempoConfidenceFactors {
    /// How far above background the chosen grid's beats sit, saturating.
    pub clarity: f32,
    /// How alike those beats are.
    pub evenness: f32,
    /// How far the winner is ahead of the best unrelated reading.
    pub margin: f32,
    /// Whether the clip is periodic at this rate at all.
    pub periodic: f32,
    /// How much audio the estimate stands on.
    pub coverage: f32,
    /// Mean onset strength on the chosen grid, in envelope standard deviations.
    pub beat_mean: f32,
    /// Spread of onset strength across those beats, same units.
    pub beat_sd: f32,
    /// Raw score gap over the best unrelated reading, `None` when there is none.
    pub rival_gap: Option<f32>,
    /// Autocorrelation at the chosen period.
    pub periodicity: f32,
    /// Beats of audio the estimate stands on.
    pub observed_beats: f32,
}

/// Tempo in beats per minute.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TempoEstimate {
    /// Estimated tempo, folded into [`Self::canonical_window_bpm`].
    pub bpm: f32,
    /// 0-1. See [`UNCERTAIN_AT_OR_BELOW`].
    pub confidence: f32,
    /// True when the estimate should be treated as a hint.
    pub uncertain: bool,
    /// Whether tempo estimation itself has been validated. See [`Maturity`].
    pub maturity: Maturity,
    /// Which algorithm produced this, versioned.
    pub source: String,
    /// Offset of the first beat from the start of the analyzed audio.
    pub beat_offset_secs: f32,
    /// The one-octave window the reported tempo is folded into.
    pub canonical_window_bpm: [f32; 2],
    /// Other plausible tempos, best first. Always includes the half and double
    /// of the chosen tempo so a consumer can override the fold.
    pub alternates: Vec<Alternate>,
    /// What produced [`Self::confidence`]. Not serialized, so it stays out of
    /// the consumer contract.
    #[serde(default, skip_serializing)]
    pub confidence_factors: TempoConfidenceFactors,
}

/// Musical key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KeyEstimate {
    /// Standard notation, e.g. `"F minor"`.
    pub key: String,
    /// Tonic pitch class name, e.g. `"F"`.
    pub tonic: String,
    /// `"major"` or `"minor"`.
    pub mode: String,
    /// Camelot wheel position, e.g. `"4A"`.
    pub camelot: String,
    /// 0-1. See [`UNCERTAIN_AT_OR_BELOW`].
    pub confidence: f32,
    /// True when the estimate should be treated as a hint.
    pub uncertain: bool,
    /// Whether key estimation itself has been validated. See [`Maturity`].
    pub maturity: Maturity,
    /// Which algorithm and profile set produced this, versioned.
    pub source: String,
    /// Other plausible keys, best first.
    pub alternates: Vec<Alternate>,
    /// How `confidence` was arrived at, factor by factor.
    ///
    /// Absent unless diagnostics were asked for, so the payload a consumer
    /// sees does not carry the estimator's internals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scoring: Option<KeyScoring>,
}

/// The factors behind a key confidence, for diagnosing a surprising estimate.
///
/// `confidence` is their product, so a low one is explained by whichever term
/// is small: nothing fits better than drums would (`strength`), two keys tie
/// (`margin`), the chroma is flat (`structure`), or it states too few pitch
/// classes (`coverage`). Diagnostic only — nothing in the contract depends on
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct KeyScoring {
    /// Correlation of the winning profile.
    pub correlation: f32,
    /// Correlation of the runner-up.
    pub runner_up: f32,
    /// Chroma salience, before the structure floor is applied.
    pub salience: f32,
    /// Effective count of pitch classes carrying tonal energy.
    pub tonal_pitch_classes: f32,
    /// How well the winner fits, 0-1.
    pub strength: f32,
    /// How far ahead of the runner-up it is, 0-1.
    pub margin: f32,
    /// Whether the chroma has any structure to correlate against, 0-1. A floor
    /// against a flat chroma, not a measure of how tonal the material is.
    ///
    /// The alias reads back diagnostic output captured while this was called
    /// `tonality`, which it was for as long as it measured how tonal a clip is.
    #[serde(alias = "tonality")]
    pub structure: f32,
    /// Whether enough distinct pitch classes are present to choose, 0-1.
    pub coverage: f32,
}

/// The store track a query resolved to.
///
/// Returned even when the match is poor, so a bad match is visible rather than
/// inferred from an implausible tempo.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TrackMatch {
    /// iTunes store track ID. Not a Music.app persistent ID.
    pub track_id: i64,
    /// Artist as the store spells it.
    pub artist: String,
    /// Title as the store spells it.
    pub title: String,
    /// Album, when the store gave one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    /// ISO-8601 release timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_date: Option<String>,
    /// Apple's own genre label, surfaced raw.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    /// Preview clip URL. Absent means the track cannot be analyzed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_url: Option<String>,
    /// Full track duration in milliseconds, for sanity-checking a match.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    /// 0-1 confidence that this is the track that was asked for.
    pub match_score: f32,
    /// True when the match should be treated as a guess.
    pub uncertain: bool,
}

/// Audio-derived features.
///
/// A struct of options rather than a fixed pair, so a new measurement arrives
/// as a new optional field without moving the existing ones.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Features {
    /// Tempo, when it could be estimated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tempo: Option<TempoEstimate>,
    /// Key, when it could be estimated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<KeyEstimate>,
}

/// Properties of the audio that was analyzed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AudioInfo {
    /// Duration in seconds.
    pub duration_secs: f64,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Channel count before the mono downmix.
    pub source_channels: u16,
    /// Fraction of samples below -60 dBFS. A high value on a preview usually
    /// means the clip is an intro or an outro rather than the body of a track.
    pub silent_fraction: f64,
    /// Whether decoding stopped at the duration limit, so the features describe
    /// only the opening of a longer file. Absent when false.
    #[serde(default, skip_serializing_if = "is_false")]
    pub truncated: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// What was asked for.
///
/// Doubles as the input line format for `batch`: one JSON object per line.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Query {
    /// Artist to search for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    /// Title to search for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// iTunes store track ID, when the caller already has one. Takes priority
    /// over artist/title, because it identifies rather than describes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_id: Option<i64>,
    /// Opaque string echoed back untouched.
    ///
    /// Carries selecta's Music.app persistent ID through, so a batch result
    /// can be matched back to its row without relying on output ordering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_ref: Option<String>,
}

/// A failure, in the same shape wherever it happens.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AnalysisError {
    /// Stable discriminator; see [`crate::ErrorKind`].
    pub kind: String,
    /// Human-readable detail. Not stable; do not parse.
    pub message: String,
}

/// One analysis result.
///
/// Emitted for both success and failure — `batch` must never drop a row or
/// abort, so a failure is a result object with `status: "error"`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Analysis {
    /// See [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// DSP version these features were produced by.
    pub algorithm_version: u32,
    /// `"ok"` or `"error"`.
    pub status: String,
    /// The query this answers, echoed back.
    pub query: Query,
    /// The store track that was analyzed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track: Option<TrackMatch>,
    /// What was measured.
    pub features: Features,
    /// Properties of the audio the features came from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio: Option<AudioInfo>,
    /// True when this came from the on-disk cache rather than fresh analysis.
    pub cached: bool,
    /// Present exactly when `status` is `"error"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<AnalysisError>,
}

impl Analysis {
    /// A successful result.
    pub fn ok(
        query: Query,
        track: Option<TrackMatch>,
        features: Features,
        audio: AudioInfo,
    ) -> Self {
        Analysis {
            schema_version: SCHEMA_VERSION,
            algorithm_version: crate::ALGORITHM_VERSION,
            status: "ok".into(),
            query,
            track,
            features,
            audio: Some(audio),
            cached: false,
            error: None,
        }
    }

    /// A failed result. Carries whatever was learned before the failure, so a
    /// caller can see which track a decode failure was about.
    pub fn failed(query: Query, track: Option<TrackMatch>, err: &crate::Error) -> Self {
        Analysis {
            schema_version: SCHEMA_VERSION,
            algorithm_version: crate::ALGORITHM_VERSION,
            status: "error".into(),
            query,
            track,
            features: Features::default(),
            audio: None,
            cached: false,
            error: Some(AnalysisError {
                kind: err.kind().to_string(),
                message: err.to_string(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn features_omit_absent_fields() {
        let json = serde_json::to_string(&Features::default()).unwrap();
        assert_eq!(json, "{}");
    }

    #[test]
    fn a_batch_line_parses_with_only_the_fields_it_sets() {
        let q: Query = serde_json::from_str(r#"{"artist":"a","title":"b"}"#).unwrap();
        assert_eq!(q.artist.as_deref(), Some("a"));
        assert!(q.track_id.is_none() && q.client_ref.is_none());
        let q: Query = serde_json::from_str(r#"{"track_id":1,"client_ref":"x"}"#).unwrap();
        assert_eq!(q.track_id, Some(1));
        assert_eq!(q.client_ref.as_deref(), Some("x"));
    }

    fn tempo_json() -> String {
        serde_json::to_string(&TempoEstimate {
            bpm: 128.0,
            confidence: 0.9,
            uncertain: false,
            maturity: Maturity::Validated,
            source: "test".into(),
            beat_offset_secs: 0.0,
            canonical_window_bpm: [90.0, 180.0],
            alternates: Vec::new(),
            confidence_factors: Default::default(),
        })
        .unwrap()
    }

    #[test]
    fn the_confidence_factors_stay_out_of_the_consumer_contract() {
        // Diagnostic, so it is deliberately not a schema change. It still
        // round-trips, because a payload that cannot be read back is a bug.
        let json = tempo_json();
        assert!(!json.contains("confidence_factors"), "{json}");
        let back: TempoEstimate = serde_json::from_str(&json).unwrap();
        assert_eq!(back.confidence_factors, TempoConfidenceFactors::default());
    }

    #[test]
    fn maturity_is_a_lowercase_string_a_consumer_can_switch_on() {
        assert!(tempo_json().contains(r#""maturity":"validated""#));
        assert_eq!(
            serde_json::from_str::<Maturity>(r#""provisional""#).unwrap(),
            Maturity::Provisional
        );
    }

    #[test]
    fn an_estimate_without_a_maturity_is_rejected_rather_than_assumed() {
        // No serde default: a payload from before the field existed must fail
        // to parse, so the cache treats it as a miss. Defaulting would let a
        // stale row claim a maturity nothing measured.
        let without: serde_json::Value = {
            let mut v: serde_json::Value = serde_json::from_str(&tempo_json()).unwrap();
            v.as_object_mut().unwrap().remove("maturity");
            v
        };
        assert!(serde_json::from_value::<TempoEstimate>(without).is_err());
    }

    #[test]
    fn a_failure_is_a_result_object_carrying_a_stable_kind() {
        let err = crate::Error::NoPreview { track_id: 5 };
        let a = Analysis::failed(Query::default(), None, &err);
        assert_eq!(a.status, "error");
        assert_eq!(a.error.as_ref().unwrap().kind, "no_preview");
        assert_eq!(a.schema_version, SCHEMA_VERSION);
        // Round-trips, so a consumer can deserialize what we emit.
        let json = serde_json::to_string(&a).unwrap();
        assert_eq!(serde_json::from_str::<Analysis>(&json).unwrap(), a);
    }
}
