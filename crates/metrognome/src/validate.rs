//! Accuracy checking against known answers.
//!
//! [`REFERENCE_TRACKS`] needs the network and is where octave handling on real
//! recordings shows up; [`selftest`] runs the same shape of check against
//! synthesized audio, so CI can catch a regression without reaching Apple.

use serde::{Deserialize, Serialize};

use crate::pipeline::{analyze_pcm_with, AnalysisOptions};
use crate::testsig::{self, Groove, Quality};
use crate::types::{Alternate, AudioInfo, Features, KeyScoring, TrackMatch};

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
/// An octave or metric ratio out is what this table exists to catch; treat a
/// 1-2 BPM disagreement as a disagreement between sources. Three of these
/// figures have been wrong where the estimator was right, so a disagreement is
/// a question about which side is wrong (DECISIONS.md, entries 27 and 32).
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
        // Checked against a published listing.
        expected_bpm: 127.0,
        expected_key: "D",
        genre: "big beat / techno",
    },
    ReferenceTrack {
        artist: "Darude",
        title: "Sandstorm",
        expected_bpm: 136.0,
        // The source gives a bare "B". Recording a mode it did not state would
        // be inventing half the fact, and the tonic alone already catches the
        // failure that matters: the estimator answered E, a fifth away.
        expected_key: "B",
        genre: "trance",
    },
    ReferenceTrack {
        artist: "Underworld",
        title: "Born Slippy .NUXX",
        // Checked against a published listing, which gives the same figure for
        // the original and the radio edit.
        expected_bpm: 140.0,
        expected_key: "Bb",
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
        // 155, not the 172 this list first carried. A public database gives 155
        // for both the album version and the radio edit, and the estimator had
        // been reading 155 all along. The two versions disagree on key (G and
        // A), so no key is claimed here.
        artist: "Goldie",
        title: "Inner City Life",
        expected_bpm: 155.0,
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

/// What a validation run measured, split by how far each half can be trusted.
///
/// Tempo and key are not equally validated, so they are never totalled: the
/// same ten tracks are a real accuracy measurement for tempo and a handful of
/// anecdotes for key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    /// Rows measured.
    pub total: usize,
    /// Rows whose tempo landed within [`BPM_TOLERANCE`].
    pub tempo_ok: usize,
    /// Rows carrying a key expectation at all. Most do not.
    pub key_checked: usize,
    /// Of those, how many agreed.
    pub key_agreed: usize,
}

impl Summary {
    /// Count a set of rows.
    pub fn of(rows: &[ValidationRow]) -> Self {
        Self {
            total: rows.len(),
            tempo_ok: rows.iter().filter(|r| r.tempo_ok()).count(),
            key_checked: rows.iter().filter(|r| r.key_ok.is_some()).count(),
            key_agreed: rows.iter().filter(|r| r.key_ok == Some(true)).count(),
        }
    }

    /// What makes the run exit non-zero.
    ///
    /// Tempo only. Published key data contradicts itself, so a key
    /// disagreement is reported and diagnosed but would fail a build on the
    /// reference rather than the estimator.
    pub fn failures(&self) -> usize {
        self.total - self.tempo_ok
    }
}

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
    /// Whether the key estimate flags itself a hint. This is what a consumer
    /// branches on, so the table shows it rather than leaving it to be
    /// inferred from the confidence. Absent when there is no key, which is
    /// also how a run recorded before the field existed reads back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_uncertain: Option<bool>,
    /// The factors behind the key confidence, when diagnostics were asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_scoring: Option<KeyScoring>,
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
    /// Tempo alternates on offer, best first. The score is comparable only
    /// within one track, and separates a near miss from a rout.
    #[serde(default)]
    pub tempo_alternates: Vec<Alternate>,
    /// Key alternates on offer, best first. Losing to the relative major, to
    /// the dominant, or to a key sharing no notes are three different failures.
    #[serde(default)]
    pub key_alternates: Vec<Alternate>,
}

impl MatchedTrack {
    /// What a live run resolved to, paired with the features it produced.
    pub fn of(track: &TrackMatch, audio: &AudioInfo, features: &Features) -> Self {
        let alternates = |a: Option<&Vec<Alternate>>| a.cloned().unwrap_or_default();
        MatchedTrack {
            artist: track.artist.clone(),
            title: track.title.clone(),
            match_score: track.match_score,
            uncertain: track.uncertain,
            preview_secs: audio.duration_secs,
            silent_fraction: audio.silent_fraction,
            tempo_alternates: alternates(features.tempo.as_ref().map(|t| &t.alternates)),
            key_alternates: alternates(features.key.as_ref().map(|k| &k.alternates)),
        }
    }
}

impl ValidationRow {
    /// Whether everything this row checked agreed. Drives the diagnostics, so
    /// that a key disagreement still gets explained even when the tempo landed.
    ///
    /// This is deliberately *not* what gates the run — see [`Summary`].
    pub fn passed(&self) -> bool {
        self.verdict == Verdict::Ok && self.key_ok != Some(false)
    }

    /// Whether the tempo landed within tolerance.
    pub fn tempo_ok(&self) -> bool {
        self.verdict == Verdict::Ok
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
/// Wide enough to absorb disagreement between sources, narrow enough that a
/// genuine mis-estimate cannot hide in it.
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
/// Spelling is normalized on both sides, so "Bb minor", "A# Minor" and "bb
/// min" all agree.
pub fn key_matches(expected: &str, estimated: &str) -> bool {
    let (Some((want_pc, want_minor)), Some((got_pc, got_minor))) =
        (parse_key(expected), parse_key(estimated))
    else {
        return false;
    };
    if want_pc != got_pc {
        return false;
    }
    match (want_minor, got_minor) {
        // An expectation that names no mode is satisfied by either. Public
        // tempo-and-key databases routinely publish a bare tonic, and recording
        // "B major" when the source said "B" would be inventing half the fact.
        (Some(want), Some(got)) => want == got,
        _ => true,
    }
}

/// Parse a key name into a pitch class and, when stated, its mode.
///
/// `Some((11, Some(true)))` is B minor, `Some((11, None))` is "B" with no mode
/// given. Enharmonic spellings collapse.
fn parse_key(s: &str) -> Option<(u8, Option<bool>)> {
    let lower = s.trim().to_lowercase();
    if lower.is_empty() {
        return None;
    }
    let (tonic, rest) = match lower.split_once(char::is_whitespace) {
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
        _ => return None,
    };
    let mode = if rest.is_empty() {
        None
    } else if rest.starts_with("min") {
        Some(true)
    } else {
        Some(false)
    };
    Some((pc, mode))
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
        key_uncertain: features.key.as_ref().map(|k| k.uncertain),
        key_scoring: features.key.as_ref().and_then(|k| k.scoring),
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

/// Placeholder for a column with nothing in it.
const DASH: &str = "—";

/// Two decimal places, or a dash when there is no number.
fn or_dash(v: Option<f32>) -> String {
    v.map_or_else(|| DASH.to_string(), |v| format!("{v:.2}"))
}

/// Render rows as a GitHub-flavoured markdown table.
pub fn render_table(rows: &[ValidationRow]) -> String {
    let mut out = String::new();
    out.push_str("| Track | Genre | Expected BPM | Estimated BPM | Verdict | Tempo conf. | Expected key | Estimated key | Camelot | Key conf. | Key kept |\n");
    out.push_str("|---|---|---:|---:|---|---:|---|---|---|---:|---|\n");
    for r in rows {
        let verdict = match r.verdict {
            Verdict::Ok => "ok",
            Verdict::OctaveError => "**OCTAVE**",
            Verdict::MetricError => "**METRIC**",
            Verdict::Wrong => "**wrong**",
            Verdict::Missing => "**none**",
        };
        let estimated_key = match (&r.estimated_key, r.key_ok) {
            // Bolded only when it was checked and missed, so a wrong key is as
            // visible in the table as a wrong tempo.
            (Some(k), Some(false)) => format!("**{k}**"),
            (Some(k), _) => k.clone(),
            (None, Some(false)) => "**none**".to_string(),
            (None, _) => DASH.to_string(),
        };
        out.push_str(&format!(
            "| {} | {} | {:.0} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            r.label,
            r.genre,
            r.expected_bpm,
            or_dash(r.estimated_bpm),
            verdict,
            or_dash(r.tempo_confidence),
            if r.expected_key.is_empty() {
                DASH
            } else {
                &r.expected_key
            },
            estimated_key,
            r.camelot.as_deref().unwrap_or(DASH),
            or_dash(r.key_confidence),
            // What a consumer actually does with the row: an uncertain key is
            // a hint it discards, so a confident wrong answer and a discarded
            // one do not look alike in the table.
            match r.key_uncertain {
                Some(true) => "no",
                Some(false) => "yes",
                None => DASH,
            },
        ));
    }
    out
}

/// One line of alternates, best first.
///
/// A key alternate identifies itself by its label; a tempo alternate has none,
/// because the number is the whole answer.
fn describe_alternates(alts: &[Alternate]) -> String {
    alts.iter()
        .map(|a| {
            let what = a.label.clone().unwrap_or_else(|| format!("{:.2}", a.value));
            format!("{what} {} score {:.3}", a.relation, a.score)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Render the per-failure detail that the table has no room for.
///
/// Which recording the query resolved to, whether the preview had music in it,
/// and whether the expected tempo was among the alternates — separating a
/// resolution bug from a beatless clip from a genuine scoring miss.
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
            out.push_str(&format!(
                "    alternates: {}\n",
                describe_alternates(&m.tempo_alternates)
            ));
            // The expected tempo being on the shortlist but not chosen is a
            // scoring problem; it being absent is an envelope problem.
            let near = m
                .tempo_alternates
                .iter()
                .any(|a| (a.value as f32 - r.expected_bpm).abs() <= BPM_TOLERANCE);
            out.push_str(&format!(
                "    expected {:.0} was {} the alternates\n",
                r.expected_bpm,
                if near { "AMONG" } else { "not among" }
            ));
        }
        if !m.key_alternates.is_empty() {
            // The relation matters more than the score here. A run of
            // `relative_minor` and `dominant` runners-up separated by
            // hundredths is the chromagram working and the tiebreak failing;
            // an unrelated key winning outright is the chromagram failing.
            out.push_str(&format!(
                "    key: {} conf {:.2}{}, runners-up: {}\n",
                r.estimated_key.as_deref().unwrap_or("none"),
                r.key_confidence.unwrap_or(0.0),
                match r.key_uncertain {
                    Some(true) => " (uncertain, a consumer discards this)",
                    _ => "",
                },
                describe_alternates(&m.key_alternates)
            ));
            // Confidence is the product of these, so the smallest one names
            // the reason. Without it a surprising number has to be reasoned
            // backwards from the alternates.
            if let Some(k) = &r.key_scoring {
                out.push_str(&format!(
                    "    key scoring: corr {:.3} vs runner-up {:.3} | strength {:.2} margin {:.2} structure {:.2} coverage {:.2} | salience {:.2}, tonal pitch classes {:.2}\n",
                    k.correlation,
                    k.runner_up,
                    k.strength,
                    k.margin,
                    k.structure,
                    k.coverage,
                    k.salience,
                    k.tonal_pitch_classes
                ));
            }
        }
    }
    out
}

/// Render the key scoring factors for every row that produced a key.
///
/// Separate from [`render_diagnostics`], which is failures only: a key that
/// scored low is not a failure — the reference set carries an expected key for
/// only three tracks — yet why it scored low is the whole question.
pub fn render_key_scoring(rows: &[ValidationRow]) -> String {
    let mut out = String::new();
    for r in rows {
        let Some(k) = &r.key_scoring else {
            continue;
        };
        out.push_str(&format!(
            "{:<42} {:<10} conf {:.2} {:<9} corr {:.3} vs {:.3} | strength {:.2} margin {:.2} structure {:.2} coverage {:.2} | sal {:.2} tpc {:.2}\n",
            r.label,
            r.estimated_key.as_deref().unwrap_or("none"),
            r.key_confidence.unwrap_or(0.0),
            match r.key_uncertain {
                Some(true) => "DISCARDED",
                Some(false) => "kept",
                None => "",
            },
            k.correlation,
            k.runner_up,
            k.strength,
            k.margin,
            k.structure,
            k.coverage,
            k.salience,
            k.tonal_pitch_classes,
        ));
    }
    out
}

/// One synthesized case: a signal with a known tempo and key.
struct SelftestCase {
    label: String,
    genre: &'static str,
    bpm: f32,
    key: &'static str,
    signal: Vec<f32>,
}

/// Synthesized cases covering the same tempo range as [`REFERENCE_TRACKS`],
/// each carrying the octave or metric trap its idiom actually has: the groove
/// column is what decides which trap, and [`Groove`] documents them.
fn selftest_cases(sample_rate: u32) -> Vec<SelftestCase> {
    let secs = 30.0;
    #[rustfmt::skip]
    let grooved: &[(f32, u8, Quality, &str, Groove, &str)] = &[
        (120.0,  9, Quality::Minor, "A minor",  Groove::FourOnFloor, "house"),
        (124.0,  5, Quality::Minor, "F minor",  Groove::FourOnFloor, "house"),
        (128.0,  0, Quality::Major, "C major",  Groove::FourOnFloor, "house"),
        (136.0, 11, Quality::Minor, "B minor",  Groove::FourOnFloor, "techno"),
        (138.0,  3, Quality::Minor, "Eb minor", Groove::FourOnFloor, "techno"),
        (170.0,  7, Quality::Minor, "G minor",  Groove::Breakbeat,   "drum & bass"),
        (174.0,  2, Quality::Minor, "D minor",  Groove::Breakbeat,   "drum & bass"),
        (176.0, 10, Quality::Major, "Bb major", Groove::Breakbeat,   "drum & bass"),
    ];

    let mut cases: Vec<SelftestCase> = grooved
        .iter()
        .map(|&(bpm, key_pc, quality, key, groove, genre)| {
            let mut signal = testsig::groove(bpm, secs, sample_rate, groove);
            testsig::mix_at(
                &mut signal,
                &testsig::chord_progression(key_pc, quality, secs, sample_rate),
                0,
            );
            let shape = match groove {
                Groove::FourOnFloor => "four-on-the-floor",
                Groove::Breakbeat => "breakbeat",
                Groove::OffbeatTrance => "offbeat trance",
            };
            SelftestCase {
                label: format!("synthetic {shape} {bpm:.0}"),
                genre,
                bpm,
                key,
                signal,
            }
        })
        .collect();

    // A bare click track at each end, as an unambiguous control.
    cases.extend([122.0f32, 172.0].map(|bpm| SelftestCase {
        label: format!("click track {bpm:.0}"),
        genre: "control",
        bpm,
        key: "",
        signal: testsig::click_track(bpm, secs, sample_rate),
    }));
    cases
}

/// Run the offline accuracy check.
///
/// Needs no network and no audio files: every case is synthesized here, which
/// is what lets it run on CI and in a sandbox.
pub fn selftest(sample_rate: u32, options: &AnalysisOptions) -> Vec<ValidationRow> {
    selftest_cases(sample_rate)
        .into_iter()
        .map(|c| {
            let features = analyze_pcm_with(&c.signal, sample_rate, options);
            row(c.label, c.genre, c.bpm, c.key, &features)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alt(value: f64, label: Option<&str>, relation: &str, score: f32) -> Alternate {
        Alternate {
            value,
            label: label.map(str::to_string),
            relation: relation.to_string(),
            score,
        }
    }

    #[test]
    fn a_row_saved_before_the_uncertain_flag_still_reads_back() {
        let json = serde_json::json!({
            "label": "x",
            "genre": "house",
            "expected_bpm": 128.0,
            "estimated_bpm": 128.0,
            "expected_key": "",
            "estimated_key": null,
            "camelot": null,
            "tempo_confidence": 0.9,
            "key_confidence": null,
            "verdict": "ok",
        });
        let row: ValidationRow = serde_json::from_value(json).expect("older row");
        assert_eq!(row.key_uncertain, None);
    }

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
            tempo_alternates: vec![
                alt(155.0, None, "double", 0.9),
                alt(77.5, None, "half", 0.4),
            ],
            key_alternates: vec![alt(9.0, Some("G major (9B)"), "relative_major", 0.71)],
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
            tempo_alternates: vec![alt(174.0, None, "double", 0.9)],
            key_alternates: Vec::new(),
        });
        let d = render_diagnostics(&[r]);
        assert!(d.contains("AMONG the alternates"), "{d}");
        assert!(!d.contains("UNCERTAIN"), "{d}");
        // No key alternates, no key line: a tempo failure on a track carrying
        // no key expectation should not grow a row of zeroes.
        assert!(!d.contains("runners-up"), "{d}");
    }

    #[test]
    fn diagnostics_name_the_runner_up_key_and_how_it_relates() {
        let mut r = row("x", "trance", 136.0, "B", &Features::default());
        r.estimated_key = Some("E minor".into());
        r.key_confidence = Some(1.0);
        r.matched = Some(MatchedTrack {
            artist: "Darude".into(),
            title: "Sandstorm".into(),
            match_score: 1.0,
            uncertain: false,
            preview_secs: 30.0,
            silent_fraction: 0.03,
            tempo_alternates: Vec::new(),
            key_alternates: vec![
                alt(10.0, Some("B minor (10A)"), "dominant", 0.612),
                alt(9.0, Some("G major (9B)"), "relative_major", 0.604),
            ],
        });
        let d = render_diagnostics(&[r]);
        // The whole point: a confident wrong answer whose runner-up is a
        // hundredth behind is a different bug from one that wins by a mile.
        assert!(d.contains("key: E minor conf 1.00"), "{d}");
        assert!(d.contains("B minor (10A) dominant score 0.612"), "{d}");
    }

    #[test]
    fn diagnostics_break_a_key_confidence_into_its_factors() {
        // The real-audio run this was built for: Sandstorm reported a wrong
        // key at 0.95 and the reason had to be reasoned backwards out of the
        // alternates. The factors say it outright.
        let mut r = row("x", "trance", 136.0, "B", &Features::default());
        r.estimated_key = Some("E minor".into());
        r.key_confidence = Some(0.95);
        r.key_uncertain = Some(false);
        r.key_scoring = Some(KeyScoring {
            correlation: 0.908,
            runner_up: 0.636,
            salience: 0.9,
            tonal_pitch_classes: 6.2,
            strength: 1.0,
            margin: 0.95,
            structure: 1.0,
            coverage: 1.0,
        });
        r.matched = Some(MatchedTrack {
            artist: "Darude".into(),
            title: "Sandstorm".into(),
            match_score: 1.0,
            uncertain: false,
            preview_secs: 30.0,
            silent_fraction: 0.03,
            tempo_alternates: Vec::new(),
            key_alternates: vec![alt(10.0, Some("B minor (10A)"), "dominant", 0.577)],
        });
        let d = render_diagnostics(&[r]);
        assert!(d.contains("corr 0.908 vs runner-up 0.636"), "{d}");
        assert!(d.contains("coverage 1.00"), "{d}");
        assert!(d.contains("tonal pitch classes 6.20"), "{d}");
    }

    #[test]
    fn key_scoring_is_reported_for_every_row_not_only_failures() {
        // Seven of the ten reference tracks carry no expected key, so they
        // always "pass" and never reach render_diagnostics — and they are
        // exactly the rows whose low confidence needs explaining.
        let mut passing = row("passes", "house", 128.0, "", &Features::default());
        passing.verdict = Verdict::Ok;
        passing.estimated_key = Some("F major".into());
        passing.key_confidence = Some(0.08);
        passing.key_uncertain = Some(true);
        passing.key_scoring = Some(KeyScoring {
            correlation: 0.780,
            runner_up: 0.774,
            salience: 0.8,
            tonal_pitch_classes: 6.0,
            strength: 1.0,
            margin: 0.08,
            structure: 1.0,
            coverage: 1.0,
        });
        assert!(passing.passed(), "row must pass, or this proves nothing");
        assert!(render_diagnostics(&[passing.clone()]).is_empty());

        let s = render_key_scoring(&[passing]);
        assert!(s.contains("corr 0.780 vs 0.774"), "{s}");
        assert!(s.contains("margin 0.08"), "{s}");
        assert!(s.contains("DISCARDED"), "{s}");
    }

    #[test]
    fn diagnostics_say_when_a_key_would_be_discarded() {
        let mut r = row("x", "house", 120.0, "F", &Features::default());
        r.estimated_key = Some("F major".into());
        r.key_confidence = Some(0.08);
        r.key_uncertain = Some(true);
        r.matched = Some(MatchedTrack {
            artist: "Robin S.".into(),
            title: "Show Me Love".into(),
            match_score: 1.0,
            uncertain: false,
            preview_secs: 30.0,
            silent_fraction: 0.01,
            tempo_alternates: Vec::new(),
            key_alternates: vec![alt(8.0, Some("A minor (8A)"), "other", 0.774)],
        });
        let d = render_diagnostics(&[r]);
        assert!(d.contains("a consumer discards this"), "{d}");
    }

    #[test]
    fn the_table_says_whether_a_key_survives_the_uncertain_flag() {
        let mut kept = row("kept", "house", 128.0, "", &Features::default());
        kept.estimated_key = Some("C major".into());
        kept.key_confidence = Some(0.9);
        kept.key_uncertain = Some(false);
        let mut dropped = row("dropped", "house", 128.0, "", &Features::default());
        dropped.estimated_key = Some("C major".into());
        dropped.key_confidence = Some(0.1);
        dropped.key_uncertain = Some(true);

        let t = render_table(&[kept, dropped]);
        assert!(t.contains("| Key kept |"), "{t}");
        let lines: Vec<&str> = t.lines().filter(|l| l.starts_with("| kept |")).collect();
        assert!(lines[0].trim_end().ends_with("| yes |"), "{:?}", lines[0]);
        let lines: Vec<&str> = t.lines().filter(|l| l.starts_with("| dropped |")).collect();
        assert!(lines[0].trim_end().ends_with("| no |"), "{:?}", lines[0]);
    }

    #[test]
    fn a_key_disagreement_is_reported_but_does_not_gate_the_run() {
        // The case this exists for: Sandstorm's tempo lands and its key does
        // not. The run must say so and still exit zero, because the key
        // reference is the weaker half of the comparison.
        let mut tempo_right_key_wrong = row("x", "trance", 136.0, "B", &Features::default());
        tempo_right_key_wrong.verdict = Verdict::Ok;
        tempo_right_key_wrong.key_ok = Some(false);

        let mut both_right = row("y", "house", 128.0, "", &Features::default());
        both_right.verdict = Verdict::Ok;

        let s = Summary::of(&[tempo_right_key_wrong.clone(), both_right]);
        assert_eq!(s.failures(), 0, "a key miss must not fail the run");
        assert_eq!((s.tempo_ok, s.key_checked, s.key_agreed), (2, 1, 0));

        // It is still a diagnosable row, or the disagreement goes unexplained.
        assert!(!tempo_right_key_wrong.passed());
    }

    #[test]
    fn a_tempo_miss_still_fails_the_run() {
        let mut r = row("x", "techno", 138.0, "", &Features::default());
        r.verdict = Verdict::Wrong;
        assert_eq!(Summary::of(&[r]).failures(), 1);
    }

    #[test]
    fn a_passing_row_needs_no_diagnostics() {
        let features = Features {
            tempo: Some(crate::types::TempoEstimate {
                bpm: 128.0,
                confidence: 0.9,
                uncertain: false,
                maturity: crate::types::Maturity::Validated,
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
    fn a_tonic_without_a_mode_matches_either_mode() {
        // Public databases publish a bare "B" for Sandstorm. That must accept
        // B minor and B major, and still reject the fifth-away answer the
        // estimator actually gave.
        assert!(key_matches("B", "B minor"));
        assert!(key_matches("B", "B major"));
        assert!(!key_matches("B", "E minor"));
        // A stated mode is still enforced in both directions.
        assert!(!key_matches("B minor", "B major"));
        assert!(key_matches("B minor", "B"));
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
                maturity: crate::types::Maturity::Validated,
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
                maturity: crate::types::Maturity::Provisional,
                source: "test".into(),
                alternates: Vec::new(),
                scoring: None,
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
