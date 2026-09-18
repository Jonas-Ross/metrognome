//! Accuracy checking against known answers.
//!
//! Two flavours, because they catch different things:
//!
//! - [`REFERENCE_TRACKS`] is a list of well-known electronic releases with
//!   widely documented tempos, spanning house, techno and drum & bass. Running
//!   it needs the network, and it is where octave handling on *real* recordings
//!   shows up.
//! - [`selftest`] does the same shape of check against synthesized audio, so
//!   the octave and metric-decoy behaviour can be verified anywhere, including
//!   on a CI box with no route to Apple.

use serde::{Deserialize, Serialize};

use crate::pipeline::{analyze_pcm_with, AnalysisOptions};
use crate::testsig::{self, Groove, Quality};
use crate::types::Features;

/// One track with a documented tempo.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ReferenceTrack {
    /// Artist, as the store spells it.
    pub artist: &'static str,
    /// Title, as the store spells it.
    pub title: &'static str,
    /// Commonly cited tempo in BPM.
    pub expected_bpm: f32,
    /// Commonly cited key, where one is well established. Empty otherwise —
    /// published key references are far less consistent than tempo ones, and
    /// inventing an expectation would make the table look more authoritative
    /// than it is.
    pub expected_key: &'static str,
    /// Rough idiom, so a failure pattern by genre is visible at a glance.
    pub genre: &'static str,
}

/// Reference set spanning the three idioms the tempo fold is built around.
///
/// Tempos are the commonly cited figures. Treat a 1-2 BPM disagreement as a
/// disagreement between sources, not a bug; an octave or a metric ratio out is
/// the thing this table exists to catch.
pub const REFERENCE_TRACKS: &[ReferenceTrack] = &[
    ReferenceTrack {
        artist: "Robin S",
        title: "Show Me Love",
        expected_bpm: 120.0,
        expected_key: "",
        genre: "house",
    },
    ReferenceTrack {
        artist: "Daft Punk",
        title: "Around the World",
        expected_bpm: 121.0,
        expected_key: "",
        genre: "house",
    },
    ReferenceTrack {
        artist: "Stardust",
        title: "Music Sounds Better with You",
        expected_bpm: 125.0,
        expected_key: "",
        genre: "house",
    },
    ReferenceTrack {
        artist: "Eric Prydz",
        title: "Call on Me",
        expected_bpm: 126.0,
        expected_key: "",
        genre: "house",
    },
    ReferenceTrack {
        artist: "The Chemical Brothers",
        title: "Hey Boy Hey Girl",
        expected_bpm: 130.0,
        expected_key: "",
        genre: "big beat / techno",
    },
    ReferenceTrack {
        artist: "Darude",
        title: "Sandstorm",
        expected_bpm: 136.0,
        expected_key: "B minor",
        genre: "trance",
    },
    ReferenceTrack {
        artist: "Underworld",
        title: "Born Slippy .NUXX",
        expected_bpm: 138.0,
        expected_key: "",
        genre: "techno",
    },
    ReferenceTrack {
        artist: "Roni Size / Reprazent",
        title: "Brown Paper Bag",
        expected_bpm: 170.0,
        expected_key: "",
        genre: "drum & bass",
    },
    ReferenceTrack {
        artist: "Goldie",
        title: "Inner City Life",
        expected_bpm: 172.0,
        expected_key: "",
        genre: "drum & bass",
    },
    ReferenceTrack {
        artist: "Pendulum",
        title: "Tarantula",
        expected_bpm: 174.0,
        expected_key: "",
        genre: "drum & bass",
    },
];

/// One row of a validation report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationRow {
    /// What was analyzed.
    pub label: String,
    /// Idiom, for reading failure patterns by genre.
    pub genre: String,
    /// The tempo that was expected.
    pub expected_bpm: f32,
    /// The tempo that came out, if any.
    pub estimated_bpm: Option<f32>,
    /// Key that was expected, empty when none is well established.
    pub expected_key: String,
    /// Key that came out, if any.
    pub estimated_key: Option<String>,
    /// Camelot position of the estimated key.
    pub camelot: Option<String>,
    /// Tempo confidence.
    pub tempo_confidence: Option<f32>,
    /// Key confidence.
    pub key_confidence: Option<f32>,
    /// How the estimate relates to the expectation. See [`Verdict`].
    pub verdict: Verdict,
    /// Anything that went wrong.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// How an estimate compares to its expectation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Within tolerance.
    Ok,
    /// Out by a factor of two — the failure the canonical fold exists to
    /// prevent, so seeing one here means the fold is not doing its job.
    OctaveError,
    /// Out by 3/2, 2/3, 4/3 or 3/4 — a metric confusion rather than an octave
    /// one, which the fold cannot catch and scoring has to.
    MetricError,
    /// Wrong in some other way.
    Wrong,
    /// Nothing came back.
    Missing,
}

/// Tolerance for calling a tempo correct, in BPM.
///
/// Wide enough to absorb disagreement between published sources and the fact
/// that a 30-second preview may sit over a tempo-mapped section; narrow enough
/// that a genuine mis-estimate cannot hide in it.
pub const BPM_TOLERANCE: f32 = 2.0;

/// Classify an estimate against its expectation.
pub fn verdict(expected: f32, estimated: Option<f32>) -> Verdict {
    let Some(got) = estimated else {
        return Verdict::Missing;
    };
    if (got - expected).abs() <= BPM_TOLERANCE {
        return Verdict::Ok;
    }
    let close = |ratio: f32| (got - expected * ratio).abs() <= BPM_TOLERANCE * ratio.max(1.0);
    if close(0.5) || close(2.0) || close(0.25) || close(4.0) {
        return Verdict::OctaveError;
    }
    if close(2.0 / 3.0) || close(1.5) || close(0.75) || close(4.0 / 3.0) {
        return Verdict::MetricError;
    }
    Verdict::Wrong
}

/// Build a row from features and an expectation.
pub fn row(
    label: impl Into<String>,
    genre: impl Into<String>,
    expected_bpm: f32,
    expected_key: &str,
    features: &Features,
) -> ValidationRow {
    let estimated_bpm = features.tempo.as_ref().map(|t| t.bpm);
    ValidationRow {
        label: label.into(),
        genre: genre.into(),
        expected_bpm,
        estimated_bpm,
        expected_key: expected_key.to_string(),
        estimated_key: features.key.as_ref().map(|k| k.key.clone()),
        camelot: features.key.as_ref().map(|k| k.camelot.clone()),
        tempo_confidence: features.tempo.as_ref().map(|t| t.confidence),
        key_confidence: features.key.as_ref().map(|k| k.confidence),
        verdict: verdict(expected_bpm, estimated_bpm),
        error: None,
    }
}

/// Render rows as a GitHub-flavoured markdown table.
pub fn render_table(rows: &[ValidationRow]) -> String {
    let mut out = String::new();
    out.push_str("| Track | Genre | Expected BPM | Estimated BPM | Verdict | Tempo conf. | Expected key | Estimated key | Camelot | Key conf. |\n");
    out.push_str("|---|---|---:|---:|---|---:|---|---|---|---:|\n");
    for r in rows {
        let dash = "—".to_string();
        out.push_str(&format!(
            "| {} | {} | {:.0} | {} | {} | {} | {} | {} | {} | {} |\n",
            r.label,
            r.genre,
            r.expected_bpm,
            r.estimated_bpm
                .map(|v| format!("{v:.2}"))
                .unwrap_or_else(|| dash.clone()),
            match r.verdict {
                Verdict::Ok => "ok",
                Verdict::OctaveError => "**OCTAVE**",
                Verdict::MetricError => "**METRIC**",
                Verdict::Wrong => "**wrong**",
                Verdict::Missing => "**none**",
            },
            r.tempo_confidence
                .map(|v| format!("{v:.2}"))
                .unwrap_or_else(|| dash.clone()),
            if r.expected_key.is_empty() {
                dash.clone()
            } else {
                r.expected_key.clone()
            },
            r.estimated_key.clone().unwrap_or_else(|| dash.clone()),
            r.camelot.clone().unwrap_or_else(|| dash.clone()),
            r.key_confidence.map(|v| format!("{v:.2}")).unwrap_or(dash),
        ));
    }
    out
}

/// Synthesized cases covering the same tempo range as [`REFERENCE_TRACKS`],
/// each carrying the octave or metric trap its idiom actually has.
fn selftest_cases(sample_rate: u32) -> Vec<(String, String, f32, &'static str, Vec<f32>)> {
    let secs = 30.0;
    let mut cases: Vec<(String, String, f32, &'static str, Vec<f32>)> = Vec::new();

    for (bpm, key_pc, quality, key_name) in [
        (120.0f32, 9u8, Quality::Minor, "A minor"),
        (124.0, 5, Quality::Minor, "F minor"),
        (128.0, 0, Quality::Major, "C major"),
        (136.0, 11, Quality::Minor, "B minor"),
        (138.0, 3, Quality::Minor, "Eb minor"),
    ] {
        // Four-on-the-floor: the trap is offbeat hats reading as double time.
        let mut sig = testsig::groove(bpm, secs, sample_rate, Groove::FourOnFloor);
        testsig::mix_at(
            &mut sig,
            &testsig::chord_progression(key_pc, quality, secs, sample_rate),
            0,
        );
        let genre = if bpm < 130.0 { "house" } else { "techno" };
        cases.push((
            format!("synthetic four-on-the-floor {bpm:.0}"),
            genre.to_string(),
            bpm,
            key_name,
            sig,
        ));
    }

    for (bpm, key_pc, quality, key_name) in [
        (170.0f32, 7u8, Quality::Minor, "G minor"),
        (174.0, 2, Quality::Minor, "D minor"),
        (176.0, 10, Quality::Major, "Bb major"),
    ] {
        // Breakbeat: the trap is the snare period reading as half time, which
        // is the 87-vs-174 drum & bass failure.
        let mut sig = testsig::groove(bpm, secs, sample_rate, Groove::Breakbeat);
        testsig::mix_at(
            &mut sig,
            &testsig::chord_progression(key_pc, quality, secs, sample_rate),
            0,
        );
        cases.push((
            format!("synthetic breakbeat {bpm:.0}"),
            "drum & bass".to_string(),
            bpm,
            key_name,
            sig,
        ));
    }

    // A bare click track at each end, as an unambiguous control.
    for bpm in [122.0f32, 172.0] {
        cases.push((
            format!("click track {bpm:.0}"),
            "control".to_string(),
            bpm,
            "",
            testsig::click_track(bpm, secs, sample_rate),
        ));
    }
    cases
}

/// Run the offline accuracy check.
///
/// Needs no network and no audio files: every case is synthesized here, which
/// is what lets it run on CI and in a sandbox.
pub fn selftest(sample_rate: u32, options: &AnalysisOptions) -> Vec<ValidationRow> {
    selftest_cases(sample_rate)
        .into_iter()
        .map(|(label, genre, bpm, key, sig)| {
            let features = analyze_pcm_with(&sig, sample_rate, options);
            row(label, genre, bpm, key, &features)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdicts_name_the_failure_mode() {
        assert_eq!(verdict(174.0, Some(174.3)), Verdict::Ok);
        assert_eq!(verdict(174.0, Some(87.0)), Verdict::OctaveError);
        assert_eq!(verdict(87.0, Some(174.0)), Verdict::OctaveError);
        assert_eq!(verdict(174.0, Some(116.0)), Verdict::MetricError);
        assert_eq!(verdict(124.0, Some(186.0)), Verdict::MetricError);
        assert_eq!(verdict(128.0, Some(101.0)), Verdict::Wrong);
        assert_eq!(verdict(128.0, None), Verdict::Missing);
    }

    #[test]
    fn the_reference_set_spans_the_three_idioms() {
        let bpms: Vec<f32> = REFERENCE_TRACKS.iter().map(|t| t.expected_bpm).collect();
        assert!(bpms.len() >= 8, "brief asks for 8-10 tracks");
        assert!(bpms.iter().any(|b| (120.0..=128.0).contains(b)), "house");
        assert!(bpms.iter().any(|b| (130.0..=145.0).contains(b)), "techno");
        assert!(bpms.iter().any(|b| (168.0..=178.0).contains(b)), "dnb");
        // Every reference tempo must survive the canonical fold unchanged, or
        // the table would be measuring the fold rather than the estimator.
        for t in REFERENCE_TRACKS {
            assert!(
                (crate::tempo::fold_to_canonical(t.expected_bpm) - t.expected_bpm).abs() < 0.01,
                "{} at {} is outside the canonical window",
                t.title,
                t.expected_bpm
            );
        }
    }

    #[test]
    fn the_table_renders_every_row() {
        let rows = vec![row("x", "house", 128.0, "C major", &Features::default())];
        let table = render_table(&rows);
        assert!(table.lines().count() == 3, "{table}");
        assert!(table.contains("**none**"));
    }

    #[test]
    #[ignore = "slow: synthesizes and analyzes ten 30-second clips"]
    fn selftest_finds_no_octave_or_metric_errors() {
        let rows = selftest(44_100, &AnalysisOptions::default());
        let bad: Vec<&ValidationRow> = rows.iter().filter(|r| r.verdict != Verdict::Ok).collect();
        assert!(bad.is_empty(), "{}", render_table(&rows));
    }
}
