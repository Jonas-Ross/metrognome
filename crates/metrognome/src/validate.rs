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
    /// How the tempo estimate relates to its expectation. See [`Verdict`].
    pub verdict: Verdict,
    /// Whether the key came out as expected. `None` when the case carries no
    /// key expectation, which is most of [`REFERENCE_TRACKS`] — published key
    /// references are far less consistent than tempo ones.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_ok: Option<bool>,
    /// Anything that went wrong.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// What the store actually returned, once resolved. A wrong estimate on
    /// the right recording and a right estimate on the wrong recording look
    /// identical in the tempo column, and they are completely different bugs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched: Option<MatchedTrack>,
}

/// What a reference query resolved to, and what the audio behind it looked
/// like. Only a live run has this; the synthetic selftest resolves nothing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchedTrack {
    /// Artist as the store spells it.
    pub artist: String,
    /// Title as the store spells it, qualifiers and all.
    pub title: String,
    /// 0-1 match score for the query that found it.
    pub match_score: f32,
    /// Whether the matcher flagged this match as a guess.
    pub uncertain: bool,
    /// Length of the preview that was analyzed.
    pub preview_secs: f64,
    /// Fraction of the preview below -60 dBFS. A high value means the clip is
    /// an intro, an outro or a breakdown rather than the body of the track.
    pub silent_fraction: f64,
    /// Tempo alternates that were on offer, best first, as `(bpm, relation)`.
    #[serde(default)]
    pub tempo_alternates: Vec<(f64, String)>,
}

impl ValidationRow {
    /// Whether this row is a pass: the tempo landed and, where a key was
    /// expected, the key did too. The tempo verdict alone is not enough —
    /// key estimation could be wrong on every case and still exit zero.
    pub fn passed(&self) -> bool {
        self.verdict == Verdict::Ok && self.key_ok != Some(false)
    }
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

/// Compare an estimated key against an expected one.
///
/// Spelling is normalized on both sides so that "Bb minor", "A# Minor" and
/// "bb min" all agree: enharmonic spelling is a notation choice, not a
/// different key, and the reference figures come from sources that pick
/// either one.
pub fn key_matches(expected: &str, estimated: &str) -> bool {
    fn canonical(s: &str) -> String {
        let lower = s.trim().to_lowercase();
        let (tonic, mode) = match lower.split_once(char::is_whitespace) {
            Some((t, m)) => (t, m.trim()),
            None => (lower.as_str(), ""),
        };
        let pc = match tonic {
            "c" | "b#" => 0,
            "c#" | "db" => 1,
            "d" => 2,
            "d#" | "eb" => 3,
            "e" | "fb" => 4,
            "f" | "e#" => 5,
            "f#" | "gb" => 6,
            "g" => 7,
            "g#" | "ab" => 8,
            "a" => 9,
            "a#" | "bb" => 10,
            "b" | "cb" => 11,
            _ => return lower.clone(),
        };
        let mode = if mode.starts_with("min") {
            "minor"
        } else {
            "major"
        };
        format!("{pc} {mode}")
    }
    canonical(expected) == canonical(estimated)
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
        matched: None,
        verdict: verdict(expected_bpm, estimated_bpm),
        key_ok: if expected_key.is_empty() {
            None
        } else {
            Some(
                features
                    .key
                    .as_ref()
                    .is_some_and(|k| key_matches(expected_key, &k.key)),
            )
        },
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
            match (&r.estimated_key, r.key_ok) {
                // Bolded only when it was checked and missed, so a wrong key is
                // as visible in the table as a wrong tempo.
                (Some(k), Some(false)) => format!("**{k}**"),
                (Some(k), _) => k.clone(),
                (None, Some(false)) => "**none**".to_string(),
                (None, _) => dash.clone(),
            },
            r.camelot.clone().unwrap_or_else(|| dash.clone()),
            r.key_confidence.map(|v| format!("{v:.2}")).unwrap_or(dash),
        ));
    }
    out
}

/// Render the per-failure detail that the table has no room for.
///
/// The table says a row is wrong. This says what it was wrong *about*: which
/// recording the query actually resolved to, whether the preview had any music
/// in it, and whether the expected tempo was among the alternates. Those three
/// separate a resolution bug from a beatless clip from a genuine scoring miss,
/// and the table alone cannot tell them apart.
pub fn render_diagnostics(rows: &[ValidationRow]) -> String {
    let mut out = String::new();
    for r in rows.iter().filter(|r| !r.passed()) {
        out.push_str(&format!("{}\n", r.label));
        if let Some(e) = &r.error {
            out.push_str(&format!("    failed: {e}\n"));
        }
        let Some(m) = &r.matched else {
            continue;
        };
        let flag = if m.uncertain { "  <- UNCERTAIN" } else { "" };
        out.push_str(&format!(
            "    matched: {} - {} [{:.2}]{}\n",
            m.artist, m.title, m.match_score, flag
        ));
        out.push_str(&format!(
            "    preview: {:.1}s, {:.0}% silent\n",
            m.preview_secs,
            m.silent_fraction * 100.0
        ));
        if !m.tempo_alternates.is_empty() {
            let alts: Vec<String> = m
                .tempo_alternates
                .iter()
                .map(|(bpm, rel)| format!("{bpm:.2} ({rel})"))
                .collect();
            out.push_str(&format!("    alternates: {}\n", alts.join(", ")));
            // The expected tempo being on the shortlist but not chosen is a
            // scoring problem; it being absent is an envelope problem.
            let near = m
                .tempo_alternates
                .iter()
                .any(|(bpm, _)| (*bpm as f32 - r.expected_bpm).abs() <= BPM_TOLERANCE);
            out.push_str(&format!(
                "    expected {:.0} was {} the alternates\n",
                r.expected_bpm,
                if near { "AMONG" } else { "not among" }
            ));
        }
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
        // Both the missing tempo and the unmet key expectation are called out.
        assert_eq!(table.matches("**none**").count(), 2, "{table}");
    }

    #[test]
    #[ignore = "slow: synthesizes and analyzes ten 30-second clips"]
    fn selftest_finds_no_octave_or_metric_errors() {
        let rows = selftest(44_100, &AnalysisOptions::default());
        let bad: Vec<&ValidationRow> = rows.iter().filter(|r| !r.passed()).collect();
        assert!(bad.is_empty(), "{}", render_table(&rows));
    }

    #[test]
    fn diagnostics_separate_a_bad_match_from_a_bad_estimate() {
        let mut r = row("x", "dnb", 172.0, "", &Features::default());
        r.matched = Some(MatchedTrack {
            artist: "Goldie".into(),
            title: "Inner City Life (Radio Edit)".into(),
            match_score: 0.82,
            uncertain: true,
            preview_secs: 30.0,
            silent_fraction: 0.4,
            tempo_alternates: vec![(155.0, "double".into()), (77.5, "half".into())],
        });
        let d = render_diagnostics(&[r]);
        assert!(d.contains("Inner City Life (Radio Edit)"), "{d}");
        assert!(d.contains("UNCERTAIN"), "{d}");
        assert!(d.contains("40% silent"), "{d}");
        // 172 is nowhere near 155 or 77.5, so this reads as an envelope problem
        // rather than the scorer picking the wrong candidate off the shortlist.
        assert!(d.contains("not among the alternates"), "{d}");

        let mut r = row("y", "dnb", 174.0, "", &Features::default());
        r.matched = Some(MatchedTrack {
            artist: "Pendulum".into(),
            title: "Tarantula".into(),
            match_score: 1.0,
            uncertain: false,
            preview_secs: 30.0,
            silent_fraction: 0.01,
            tempo_alternates: vec![(174.0, "double".into())],
        });
        let d = render_diagnostics(&[r]);
        assert!(d.contains("AMONG the alternates"), "{d}");
        assert!(!d.contains("UNCERTAIN"), "{d}");
    }

    #[test]
    fn a_passing_row_needs_no_diagnostics() {
        let features = Features {
            tempo: Some(crate::types::TempoEstimate {
                bpm: 128.0,
                confidence: 0.9,
                uncertain: false,
                source: "test".into(),
                beat_offset_secs: 0.0,
                canonical_window_bpm: [90.0, 180.0],
                alternates: Vec::new(),
            }),
            key: None,
        };
        assert!(render_diagnostics(&[row("x", "house", 128.0, "", &features)]).is_empty());
    }

    #[test]
    fn enharmonic_spellings_are_the_same_key() {
        assert!(key_matches("Bb minor", "A# Minor"));
        assert!(key_matches("F# major", "Gb major"));
        assert!(key_matches("C major", "c maj"));
        assert!(!key_matches("A minor", "A major"));
        assert!(!key_matches("A minor", "C minor"));
    }

    #[test]
    fn a_wrong_key_fails_the_row_even_when_the_tempo_is_right() {
        let features = Features {
            tempo: Some(crate::types::TempoEstimate {
                bpm: 128.0,
                confidence: 0.9,
                uncertain: false,
                source: "test".into(),
                beat_offset_secs: 0.0,
                canonical_window_bpm: [90.0, 180.0],
                alternates: Vec::new(),
            }),
            key: Some(crate::types::KeyEstimate {
                key: "A minor".into(),
                tonic: "A".into(),
                mode: "minor".into(),
                camelot: "8A".into(),
                confidence: 0.9,
                uncertain: false,
                source: "test".into(),
                alternates: Vec::new(),
            }),
        };
        let r = row("x", "house", 128.0, "C major", &features);
        assert_eq!(r.verdict, Verdict::Ok);
        assert_eq!(r.key_ok, Some(false));
        assert!(!r.passed(), "a wrong key must fail the row");

        // No expectation means no opinion, not a failure.
        let r = row("x", "house", 128.0, "", &features);
        assert_eq!(r.key_ok, None);
        assert!(r.passed());
    }
}
