//! Musical key estimation.
//!
//! Chromagram, then correlation against key profiles for all 24 rotations. The
//! output carries both standard notation and Camelot, because the consumer is a
//! DJ-adjacent tool and Camelot is what harmonic mixing actually uses.

use crate::dsp::{Spectrogram, CHROMA_FMAX, CHROMA_FMIN};
use crate::types::{Alternate, KeyEstimate, KeyScoring, Maturity, UNCERTAIN_AT_OR_BELOW};

/// Published key data contradicts itself, so there is no reference to measure
/// key against. Synthetic material rules out a rotation error and nothing more.
/// DECISIONS.md entries 31 and 32.
pub const KEY_MATURITY: Maturity = Maturity::Provisional;

/// The `@N` on every key `source`. Consumers re-measure stored keys by that
/// label, so bump it whenever a change can move any key estimate or confidence.
pub const KEY_SCORER_VERSION: u32 = 3;

/// Pitch-class names, spelled the way the Camelot wheel spells them.
///
/// Flats throughout for the black keys, matching how the charts spell them.
const PITCH_NAMES: [&str; 12] = [
    "C", "Db", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B",
];

/// Which key profile set to correlate against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KeyProfile {
    /// Krumhansl-Schmuckler probe-tone profiles (1990).
    ///
    /// The usual baseline, from listener experiments on Western classical
    /// music. Known to confuse a key with its relative major or minor.
    Krumhansl,
    /// Profiles weighted for electronic dance music, after Shaath (2011).
    ///
    /// Heavier tonic and dominant, which pulls a relative-key confusion apart
    /// on material leaning on a repeated root. See DECISIONS.md for the
    /// provenance caveat on these coefficients.
    #[default]
    Edm,
}

impl KeyProfile {
    fn profiles(self) -> (&'static [f32; 12], &'static [f32; 12]) {
        match self {
            KeyProfile::Krumhansl => (&KS_MAJOR, &KS_MINOR),
            KeyProfile::Edm => (&EDM_MAJOR, &EDM_MINOR),
        }
    }

    fn source(self) -> String {
        let name = match self {
            KeyProfile::Krumhansl => "krumhansl",
            KeyProfile::Edm => "edm",
        };
        format!("metrognome/chroma-correlation-{name}@{KEY_SCORER_VERSION}")
    }

    /// Parse a profile name as accepted on the command line.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "krumhansl" | "ks" => Some(KeyProfile::Krumhansl),
            "edm" | "shaath" => Some(KeyProfile::Edm),
            _ => None,
        }
    }
}

const KS_MAJOR: [f32; 12] = [
    6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88,
];
const KS_MINOR: [f32; 12] = [
    6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17,
];
const EDM_MAJOR: [f32; 12] = [6.6, 2.0, 3.5, 2.3, 4.6, 4.0, 2.5, 5.2, 2.4, 3.7, 2.3, 3.4];
const EDM_MINOR: [f32; 12] = [6.5, 2.7, 3.5, 5.4, 2.6, 3.5, 2.5, 5.2, 4.0, 2.7, 4.3, 3.2];

/// Camelot number for a major key, by tonic pitch class.
///
/// The wheel is the circle of fifths with C major at 8B; a step clockwise is a
/// fifth up. This is that, arithmetically.
fn camelot_number_major(pc: usize) -> u8 {
    (((pc * 7) % 12 + 7) % 12 + 1) as u8
}

/// Camelot position, e.g. `"4A"`.
pub fn camelot(pc: usize, minor: bool) -> String {
    let pc = pc % 12;
    if minor {
        // A minor key sits at its relative major's number, on the A ring.
        format!("{}A", camelot_number_major((pc + 3) % 12))
    } else {
        format!("{}B", camelot_number_major(pc))
    }
}

/// A 12-bin pitch-class profile summed over the whole clip.
#[derive(Debug, Clone, Default)]
pub struct Chromagram {
    /// Energy per pitch class, C first.
    pub bins: [f32; 12],
    /// How many frames contributed. Zero means no usable audio.
    pub frames: usize,
    /// How much audio this chroma covers, window included.
    ///
    /// Not derivable from `frames`: the chroma window rounds to a power of
    /// two, so both its length and the hop between frames differ by sample
    /// rate, and the first frame costs a whole window rather than a hop.
    pub seconds: f32,
}

impl Chromagram {
    /// How far the chroma departs from flat, relative to its own level.
    ///
    /// The one thing profile correlation cannot see: Pearson correlation is
    /// scale- and offset-invariant, so a flat chroma with a 2% ripple fits a
    /// key profile as well as one with real tonal peaks. Sustained chords
    /// measure near 1.0, drums alone near 0.2, white noise near 0.005.
    pub fn salience(&self) -> f32 {
        let mean = self.bins.iter().sum::<f32>() / 12.0;
        if mean <= 1e-9 {
            return 0.0;
        }
        let var = self
            .bins
            .iter()
            .map(|v| (v - mean) * (v - mean))
            .sum::<f32>()
            / 12.0;
        var.sqrt() / mean
    }

    /// Effective number of pitch classes carrying tonal energy.
    ///
    /// The other thing profile correlation cannot see: a key is seven pitch
    /// classes, so a riff on three fits many profiles equally and the gap
    /// between them is not evidence. Percussion adds roughly equal energy to
    /// all twelve, so the level every class shares is subtracted first —
    /// otherwise drums lift the count and a sparse riff over a beat reads as
    /// fully stated. Diatonic progressions measure 5-7, a two-chord vamp near
    /// 5, a three-note riff under 4.
    pub fn tonal_pitch_classes(&self) -> f32 {
        let pedestal = self.bins.iter().copied().fold(f32::INFINITY, f32::min);
        let total: f32 = self.bins.iter().map(|v| v - pedestal).sum();
        if total <= 1e-9 {
            return 0.0;
        }
        // Inverse participation ratio: 1/sum(p^2) over the normalized shares.
        1.0 / self
            .bins
            .iter()
            .map(|v| ((v - pedestal) / total).powi(2))
            .sum::<f32>()
    }
}

/// Build a chromagram from a chroma-sized magnitude spectrogram.
///
/// Each frame is normalized to unit sum first, so a loud drop and a quiet
/// breakdown get an equal vote.
pub fn chromagram(spec: &Spectrogram) -> Chromagram {
    let mut out = Chromagram::default();
    if spec.frames == 0 {
        return out;
    }
    // Precompute each bin's pitch class once; it depends only on geometry.
    let pitch_class: Vec<Option<usize>> = (0..spec.bins)
        .map(|b| {
            let f = spec.bin_hz(b);
            if !(CHROMA_FMIN..=CHROMA_FMAX).contains(&f) {
                return None;
            }
            // MIDI pitch, then fold to a pitch class. Nearest-semitone
            // assignment is adequate because the window is sized so that a
            // semitone is wider than a bin across the admitted range.
            let midi = 69.0 + 12.0 * (f / 440.0).log2();
            Some((midi.round() as i32).rem_euclid(12) as usize)
        })
        .collect();

    // Not uniform: a semitone spans one bin near A2 and dozens near A7, so
    // summing raw magnitudes would make white noise correlate strongly with
    // some key. Averaging per class instead keeps noise flat.
    let mut bins_per_class = [0.0f32; 12];
    for pc in pitch_class.iter().flatten() {
        bins_per_class[*pc] += 1.0;
    }
    if bins_per_class.contains(&0.0) {
        return out;
    }

    for t in 0..spec.frames {
        let row = spec.frame(t);
        let mut frame = [0.0f32; 12];
        for (b, pc) in pitch_class.iter().enumerate() {
            if let Some(pc) = pc {
                frame[*pc] += row[b];
            }
        }
        let mut total = 0.0f32;
        for (v, count) in frame.iter_mut().zip(bins_per_class) {
            *v /= count;
            total += *v;
        }
        if total <= 1e-9 {
            continue;
        }
        for (acc, v) in out.bins.iter_mut().zip(frame) {
            *acc += v / total;
        }
        out.frames += 1;
    }
    if out.frames > 0 && spec.fps > 0.0 {
        // One whole window, then a hop for each frame after the first.
        out.seconds =
            spec.n_fft as f32 / spec.sample_rate as f32 + (out.frames - 1) as f32 / spec.fps;
    }
    out
}

/// Pearson correlation between a chroma vector rotated to `tonic` and a profile.
fn correlate(chroma: &[f32; 12], profile: &[f32; 12], tonic: usize) -> f32 {
    let rotated: Vec<f32> = (0..12).map(|i| chroma[(tonic + i) % 12]).collect();
    let mean_a = rotated.iter().sum::<f32>() / 12.0;
    let mean_b = profile.iter().sum::<f32>() / 12.0;
    let mut num = 0.0f32;
    let mut da = 0.0f32;
    let mut db = 0.0f32;
    for i in 0..12 {
        let x = rotated[i] - mean_a;
        let y = profile[i] - mean_b;
        num += x * y;
        da += x * x;
        db += y * y;
    }
    let den = (da * db).sqrt();
    if den < 1e-9 {
        0.0
    } else {
        num / den
    }
}

/// Correlation below which the best-fitting profile is no better than what
/// percussion alone produces.
///
/// Measured: a click track 0.35, four-on-the-floor 0.40, a breakbeat 0.43,
/// against 0.53-0.91 for real tracks carrying harmony. This is the term that
/// rejects a drum loop, so it starts where drums stop.
const KEY_CORRELATION_FLOOR: f32 = 0.45;

/// Correlation at which a key is considered clearly stated.
///
/// Real music never approaches 1, since the chroma carries percussion too.
/// 0.85 is about what an unambiguous tonal centre reaches over a full mix.
const KEY_CORRELATION_SATURATION: f32 = 0.85;

/// Correlation lead, as a fraction of the winner's own correlation, at which
/// the margin term reaches 1/e of the way to 1.
///
/// Relative major/minor pairs share six of seven notes and routinely lead by
/// under 0.10 of the winner; clean single-key material leads by 0.17-0.40. The
/// term saturates exponentially rather than clamping, so a clear winner never
/// quite reaches certainty and a close one is not rounded up to it.
const KEY_MARGIN_SCALE: f32 = 0.10;

/// Chroma salience below which there is no structure to correlate against.
///
/// A guard against a flat chroma, not a measure of how tonal a track is:
/// correlation is offset-invariant, so noise with a 0.5% ripple fits a profile
/// as well as a chord does. White noise measures 0.005 against 0.105 for the
/// least tonal real track in a 31-track sample, so the two are separable by
/// two orders of magnitude and the guard does not need to be a ramp.
const STRUCTURE_FLOOR: f32 = 0.02;

/// Salience above which a chroma is no longer suspected of being flat.
///
/// Below every real track measured, because salience tracks arrangement
/// density rather than whether a key exists: a sparse ambient piece reads 0.77
/// and a dense club mix 0.15, and both can state a key perfectly well.
const STRUCTURE_SATURATION: f32 = 0.10;

/// Seconds of audio below which no key is named, however well a profile fits.
///
/// Averaging is what flattens noise, so a short chroma is not flat: over 300
/// random draws the worst confidence reaches 0.89 on a fifth of a second,
/// against 0.09 at this length. Duration rather than a frame count because the
/// chroma window rounds to a power of two, so the same frame count is 1.6s at
/// 44.1kHz and 3.0s at 48kHz — and measured by seconds the two rates agree,
/// which by frames they do not.
const MIN_CHROMA_SECS: f32 = 1.5;

/// Tonal pitch classes below which a chroma cannot name a key at all.
///
/// Around a triad's worth: enough to fit a profile, not enough to choose
/// between the profiles that fit. See [`Chromagram::tonal_pitch_classes`].
const COVERAGE_FLOOR: f32 = 3.5;

/// Tonal pitch classes at which the key is fully determined.
///
/// A diatonic progression exercises 5-7; every synthetic key lands at 5.0 or
/// above, so a real progression is not penalized for stopping short of seven.
const COVERAGE_SATURATION: f32 = 5.5;

/// Linear ramp from `floor` to `saturation`, clamped to 0-1.
///
/// Three of the four confidence factors are this shape, so they are worth
/// reading as the same kind of thing.
fn ramp(v: f32, floor: f32, saturation: f32) -> f32 {
    ((v - floor) / (saturation - floor)).clamp(0.0, 1.0)
}

/// How `other` relates to the chosen key, for the alternates list.
fn relation(tonic: usize, minor: bool, other_tonic: usize, other_minor: bool) -> &'static str {
    let interval = (other_tonic + 12 - tonic) % 12;
    match (minor, other_minor, interval) {
        (false, true, 9) => "relative_minor",
        (true, false, 3) => "relative_major",
        (a, b, 0) if a != b => "parallel",
        (a, b, 7) if a == b => "dominant",
        (a, b, 5) if a == b => "subdominant",
        _ => "other",
    }
}

/// Estimate the key from a chromagram.
///
/// Returns `None` when there is no tonal content to work with at all, rather
/// than naming a key nothing supports.
pub fn estimate_key(chroma: &Chromagram, profile: KeyProfile) -> Option<KeyEstimate> {
    estimate_key_scored(chroma, profile).map(|(est, _)| est)
}

/// As [`estimate_key`], also returning the factors behind the confidence.
///
/// The diagnostic commands use this; the analysis path attaches it only when
/// asked, so a normal payload stays free of the estimator's internals.
pub fn estimate_key_scored(
    chroma: &Chromagram,
    profile: KeyProfile,
) -> Option<(KeyEstimate, KeyScoring)> {
    if chroma.seconds < MIN_CHROMA_SECS {
        return None;
    }
    let (major, minor) = profile.profiles();
    let mut scores: Vec<(f32, usize, bool)> = Vec::with_capacity(24);
    for tonic in 0..12 {
        scores.push((correlate(&chroma.bins, major, tonic), tonic, false));
        scores.push((correlate(&chroma.bins, minor, tonic), tonic, true));
    }
    // Tonic then mode as tiebreakers, so an entirely flat chroma still gives a
    // deterministic (and, thanks to the confidence below, plainly untrusted)
    // answer rather than depending on sort order.
    scores.sort_by(|a, b| {
        b.0.total_cmp(&a.0)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
    });

    let (r1, tonic, is_minor) = scores[0];
    let r2 = scores[1].0;
    if r1 <= 0.0 {
        return None;
    }

    // Ramped from a floor rather than from zero: every correlation a real track
    // produces sits above what percussion reaches, so the useful discrimination
    // is all in that band and measuring from 0 wastes it.
    let strength = ramp(r1, KEY_CORRELATION_FLOOR, KEY_CORRELATION_SATURATION);
    // Relative to the winner, so a lead of 0.05 over a correlation of 0.95
    // counts for less than the same lead over 0.30.
    let margin = 1.0 - (-((r1 - r2) / r1) / KEY_MARGIN_SCALE).exp();
    // Gates on the chroma itself: below the salience floor the chroma is flat
    // and any fit is an accident, and below the coverage floor there are too
    // few pitch classes to choose a key from, however well the profiles fit.
    let salience = chroma.salience();
    let tonal_pitch_classes = chroma.tonal_pitch_classes();
    let structure = ramp(salience, STRUCTURE_FLOOR, STRUCTURE_SATURATION);
    let coverage = ramp(tonal_pitch_classes, COVERAGE_FLOOR, COVERAGE_SATURATION);
    // No factor substitutes for another: a correlation that ties with the
    // relative minor is a coin flip, a clear winner among weak correlations is
    // noise, and a clear winner over three pitch classes is a riff several keys
    // would claim.
    let confidence = crate::types::normalize_confidence(strength * margin * structure * coverage);

    let mode = if is_minor { "minor" } else { "major" };
    let alternates = scores[1..4]
        .iter()
        .map(|&(score, t, m)| Alternate {
            // The Camelot number, so wheel distance is arithmetic. The letter
            // and the key name live in the label.
            value: f64::from(camelot_number_major(if m { (t + 3) % 12 } else { t })),
            label: Some(format!(
                "{} {} ({})",
                PITCH_NAMES[t],
                if m { "minor" } else { "major" },
                camelot(t, m)
            )),
            relation: relation(tonic, is_minor, t, m).to_string(),
            score: (score * 1000.0).round() / 1000.0,
        })
        .collect();

    let scoring = KeyScoring {
        correlation: r1,
        runner_up: r2,
        salience,
        tonal_pitch_classes,
        strength,
        margin,
        structure,
        coverage,
    };

    Some((
        KeyEstimate {
            key: format!("{} {mode}", PITCH_NAMES[tonic]),
            tonic: PITCH_NAMES[tonic].to_string(),
            mode: mode.to_string(),
            camelot: camelot(tonic, is_minor),
            confidence,
            uncertain: confidence <= UNCERTAIN_AT_OR_BELOW,
            maturity: KEY_MATURITY,
            source: profile.source(),
            alternates,
            scoring: None,
        },
        scoring,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::Stft;
    use crate::testsig::{self, Groove, Quality};

    const SR: u32 = 44_100;

    fn key_of(signal: &[f32], profile: KeyProfile) -> Option<KeyEstimate> {
        let stft = Stft::for_chroma(SR);
        let chroma = chromagram(&stft.magnitudes(signal, SR));
        estimate_key(&chroma, profile)
    }

    #[test]
    fn a_key_estimate_declares_itself_provisional() {
        let sig = testsig::chord_progression(9, Quality::Minor, 20.0, SR);
        let k = key_of(&sig, KeyProfile::default()).expect("key estimate");
        assert_eq!(k.maturity, Maturity::Provisional);
    }

    #[test]
    fn camelot_matches_the_published_wheel() {
        assert_eq!(camelot(0, false), "8B"); // C major
        assert_eq!(camelot(9, true), "8A"); // A minor
        assert_eq!(camelot(7, false), "9B"); // G major
        assert_eq!(camelot(4, true), "9A"); // E minor
        assert_eq!(camelot(6, false), "2B"); // F# major
        assert_eq!(camelot(3, true), "2A"); // Eb minor
        assert_eq!(camelot(5, true), "4A"); // F minor
        assert_eq!(camelot(11, false), "1B"); // B major
        assert_eq!(camelot(8, true), "1A"); // Ab minor
    }

    #[test]
    fn every_key_maps_to_a_distinct_camelot_position() {
        let mut seen = std::collections::BTreeSet::new();
        for pc in 0..12 {
            for minor in [false, true] {
                assert!(seen.insert(camelot(pc, minor)), "duplicate at {pc} {minor}");
            }
        }
        assert_eq!(seen.len(), 24);
    }

    #[test]
    fn relative_keys_share_a_camelot_number() {
        // A minor is the relative minor of C major: 8A and 8B.
        assert_eq!(
            camelot(9, true).trim_end_matches('A'),
            camelot(0, false).trim_end_matches('B')
        );
    }

    #[test]
    fn major_progressions_are_recovered_on_both_profile_sets() {
        for profile in [KeyProfile::Edm, KeyProfile::Krumhansl] {
            for pc in [0usize, 5, 7, 10] {
                let sig = testsig::chord_progression(pc as u8, Quality::Major, 12.0, SR);
                let est = key_of(&sig, profile).expect("key");
                assert_eq!(
                    est.key,
                    format!("{} major", PITCH_NAMES[pc]),
                    "{profile:?} got {} (conf {})",
                    est.key,
                    est.confidence
                );
            }
        }
    }

    #[test]
    fn minor_progressions_are_recovered_on_both_profile_sets() {
        for profile in [KeyProfile::Edm, KeyProfile::Krumhansl] {
            for pc in [9usize, 2, 5, 3] {
                let sig = testsig::chord_progression(pc as u8, Quality::Minor, 12.0, SR);
                let est = key_of(&sig, profile).expect("key");
                assert_eq!(
                    est.key,
                    format!("{} minor", PITCH_NAMES[pc]),
                    "{profile:?} got {} (conf {})",
                    est.key,
                    est.confidence
                );
            }
        }
    }

    #[test]
    fn every_key_is_recovered_on_both_profile_sets() {
        // A profile transcribed with a rotation error, or chroma binning off
        // by a constant, passes the eight spot checks above and fails elsewhere
        // on the circle. Too few tracks carry a documented key to catch it.
        let mut wrong = Vec::new();
        for profile in [KeyProfile::Edm, KeyProfile::Krumhansl] {
            for pc in 0..12u8 {
                for (quality, mode) in [(Quality::Major, "major"), (Quality::Minor, "minor")] {
                    let sig = testsig::chord_progression(pc, quality, 8.0, SR);
                    let est = key_of(&sig, profile).expect("key");
                    let want = format!("{} {mode}", PITCH_NAMES[usize::from(pc)]);
                    if est.key != want {
                        wrong.push(format!("{profile:?} {want} -> {}", est.key));
                    }
                }
            }
        }
        assert!(wrong.is_empty(), "{} of 48: {wrong:#?}", wrong.len());
    }

    #[test]
    fn camelot_on_the_estimate_agrees_with_the_named_key() {
        let sig = testsig::chord_progression(5, Quality::Minor, 12.0, SR);
        let est = key_of(&sig, KeyProfile::Edm).unwrap();
        assert_eq!(est.key, "F minor");
        assert_eq!(est.camelot, "4A");
        assert_eq!(est.tonic, "F");
        assert_eq!(est.mode, "minor");
    }

    #[test]
    fn the_relative_key_shows_up_as_a_named_alternate() {
        let sig = testsig::chord_progression(0, Quality::Major, 12.0, SR);
        let est = key_of(&sig, KeyProfile::Edm).unwrap();
        let rel = est
            .alternates
            .iter()
            .find(|a| a.relation == "relative_minor")
            .unwrap_or_else(|| panic!("no relative minor in {:?}", est.alternates));
        assert_eq!(rel.label.as_deref(), Some("A minor (8A)"));
        // The Camelot number is the numeric value, so wheel distance is
        // arithmetic: a relative key shares its number with the chosen one.
        assert_eq!(rel.value, 8.0);
        assert!(est.camelot.starts_with('8'));
    }

    /// Mix a tonal signal with percussion at the given relative levels.
    fn over_drums(tonal: &[f32], tonal_gain: f32, drum_gain: f32) -> Vec<f32> {
        let drums = testsig::groove(128.0, 12.0, SR, Groove::FourOnFloor);
        tonal
            .iter()
            .zip(&drums)
            .map(|(t, d)| t * tonal_gain + d * drum_gain)
            .collect()
    }

    /// A lead riff on three pitch classes: E, G, B.
    fn three_note_riff() -> Vec<f32> {
        testsig::note_sequence(
            &[&[64.0], &[71.0], &[67.0], &[64.0], &[71.0], &[67.0]],
            12.0,
            SR,
        )
    }

    /// Fifths with no third, so nothing in the clip states a mode.
    fn power_chords() -> Vec<f32> {
        let e5: [f32; 4] = [40.0, 47.0, 52.0, 59.0];
        let b5: [f32; 4] = [47.0, 54.0, 59.0, 66.0];
        testsig::note_sequence(&[&e5, &b5, &e5, &b5], 12.0, SR)
    }

    #[test]
    fn a_riff_on_three_pitch_classes_is_not_a_confident_key() {
        // The regression this guards: correlation happily fits a profile to a
        // chroma with nine near-empty bins, and used to report it at 1.00.
        for (name, sig) in [
            ("riff", three_note_riff()),
            ("riff over drums", over_drums(&three_note_riff(), 1.0, 0.8)),
            (
                "riff over loud drums",
                over_drums(&three_note_riff(), 0.8, 1.5),
            ),
            ("power chords", power_chords()),
            (
                "power chords over drums",
                over_drums(&power_chords(), 1.0, 0.8),
            ),
            (
                "power chords over loud drums",
                over_drums(&power_chords(), 0.8, 1.5),
            ),
        ] {
            let est = key_of(&sig, KeyProfile::Edm).expect("key");
            assert!(
                est.uncertain,
                "{name} read as a confident {} at {}",
                est.key, est.confidence
            );
        }
    }

    #[test]
    fn a_single_pitch_class_yields_no_usable_key() {
        for (name, groups) in [
            ("drone", vec![vec![57.0f32], vec![57.0]]),
            ("bare fifth", vec![vec![57.0, 64.0], vec![57.0, 64.0]]),
        ] {
            let refs: Vec<&[f32]> = groups.iter().map(|g| g.as_slice()).collect();
            let sig = testsig::note_sequence(&refs, 12.0, SR);
            let est = key_of(&sig, KeyProfile::Edm).expect("key");
            assert_eq!(est.confidence, 0.0, "{name}: {est:?}");
            assert!(est.uncertain, "{name}: {est:?}");
        }
    }

    #[test]
    fn material_that_fits_no_key_well_is_not_confident() {
        // Symmetric harmony divides the octave evenly, so it sits far from
        // every profile while still being loud, varied and spread across the
        // chroma — it clears the salience and coverage gates on its own. Only
        // the correlation term stands between it and a confident answer.
        let wt1: [f32; 3] = [60.0, 64.0, 68.0];
        let wt2: [f32; 3] = [62.0, 66.0, 70.0];
        let dim1: [f32; 4] = [60.0, 63.0, 66.0, 69.0];
        let dim2: [f32; 4] = [61.0, 64.0, 67.0, 70.0];
        for (name, sig) in [
            (
                "whole-tone chords",
                testsig::note_sequence(&[&wt1, &wt2, &wt1, &wt2], 12.0, SR),
            ),
            (
                "diminished sevenths",
                testsig::note_sequence(&[&dim1, &dim2, &dim1, &dim2], 12.0, SR),
            ),
        ] {
            let est = key_of(&sig, KeyProfile::Edm).expect("key");
            assert!(
                est.uncertain,
                "{name} read as a confident {} at {}",
                est.key, est.confidence
            );
        }
    }

    #[test]
    fn a_relative_pair_coin_flip_is_not_confident() {
        // C major and A minor triads alternating: both keys fit the chroma, and
        // nothing summed over a whole clip says which one is home.
        let c: [f32; 4] = [48.0, 52.0, 55.0, 60.0];
        let am: [f32; 4] = [45.0, 48.0, 52.0, 57.0];
        let sig = testsig::note_sequence(&[&c, &am, &c, &am], 12.0, SR);
        let est = key_of(&sig, KeyProfile::Edm).expect("key");
        assert!(est.uncertain, "coin flip read as confident: {est:?}");
    }

    #[test]
    fn no_synthetic_key_reaches_full_confidence() {
        // Chroma correlation over a 30-second clip cannot earn certainty, so
        // 1.00 must stay out of reach even on noiseless single-key material.
        for profile in [KeyProfile::Edm, KeyProfile::Krumhansl] {
            for pc in 0..12u8 {
                for quality in [Quality::Major, Quality::Minor] {
                    let sig = testsig::chord_progression(pc, quality, 8.0, SR);
                    let est = key_of(&sig, profile).expect("key");
                    assert!(
                        est.confidence < 1.0,
                        "{profile:?} {} reported certainty",
                        est.key
                    );
                }
            }
        }
    }

    #[test]
    fn every_clean_key_stays_confident_on_both_profile_sets() {
        // The other half of the bargain: tightening confidence must not flag a
        // clean, unambiguous progression as a hint.
        let mut weak = Vec::new();
        for profile in [KeyProfile::Edm, KeyProfile::Krumhansl] {
            for pc in 0..12u8 {
                for (quality, mode) in [(Quality::Major, "major"), (Quality::Minor, "minor")] {
                    let sig = testsig::chord_progression(pc, quality, 8.0, SR);
                    let est = key_of(&sig, profile).expect("key");
                    if est.uncertain {
                        weak.push(format!(
                            "{profile:?} {} {mode} = {}",
                            PITCH_NAMES[usize::from(pc)],
                            est.confidence
                        ));
                    }
                }
            }
        }
        assert!(
            weak.is_empty(),
            "{} of 48 flagged uncertain: {weak:#?}",
            weak.len()
        );
    }

    #[test]
    fn tonal_pitch_classes_ignores_the_pedestal_percussion_adds() {
        let stft = Stft::for_chroma(SR);
        let count = |sig: &[f32]| chromagram(&stft.magnitudes(sig, SR)).tonal_pitch_classes();

        let prog = testsig::chord_progression(4, Quality::Minor, 12.0, SR);
        let riff = three_note_riff();
        assert!(count(&prog) > COVERAGE_SATURATION, "{}", count(&prog));
        assert!(count(&riff) < COVERAGE_FLOOR + 0.5, "{}", count(&riff));

        // Drums spread energy over all twelve classes. Without the pedestal
        // subtraction this is what let a riff over a beat look fully stated.
        let dry = count(&riff);
        let wet = count(&over_drums(&riff, 1.0, 0.8));
        assert!(
            wet - dry < 1.0,
            "percussion moved the count from {dry} to {wet}"
        );
    }

    #[test]
    fn confidence_tracks_how_far_ahead_the_winner_is() {
        // A two-chord vamp states fewer pitch classes than a four-chord
        // progression and must not claim as much.
        let am: [f32; 4] = [45.0, 48.0, 52.0, 57.0];
        let f: [f32; 4] = [41.0, 45.0, 48.0, 53.0];
        let vamp = key_of(
            &testsig::note_sequence(&[&am, &f, &am, &f], 12.0, SR),
            KeyProfile::Edm,
        )
        .expect("key");
        let prog = key_of(
            &testsig::chord_progression(9, Quality::Minor, 12.0, SR),
            KeyProfile::Edm,
        )
        .expect("key");
        assert_eq!(vamp.key, "A minor");
        assert_eq!(prog.key, "A minor");
        assert!(!vamp.uncertain, "vamp: {vamp:?}");
        assert!(
            vamp.confidence < prog.confidence,
            "vamp {} should trail progression {}",
            vamp.confidence,
            prog.confidence
        );
    }

    #[test]
    fn drums_alone_do_not_produce_a_confident_key() {
        let sig = testsig::groove(128.0, 20.0, SR, Groove::FourOnFloor);
        match key_of(&sig, KeyProfile::Edm) {
            None => {}
            Some(est) => assert!(
                est.uncertain,
                "percussion must not read as a confident key: {est:?}"
            ),
        }
    }

    #[test]
    fn a_click_track_does_not_produce_a_confident_key() {
        // Broadband clicks correlate with a key profile about as well as
        // anything does, because correlation cannot see how flat the chroma is.
        // The correlation floor is what stops this reading as a real key.
        let sig = testsig::click_track(122.0, 20.0, SR);
        match key_of(&sig, KeyProfile::Edm) {
            None => {}
            Some(est) => assert!(est.uncertain, "click track read as a key: {est:?}"),
        }
    }

    #[test]
    fn salience_separates_a_flat_chroma_from_a_structured_one() {
        // What salience is for after the recalibration: telling noise from
        // everything else. It does not separate drums from harmony — measured
        // percussion and a dense real mix both sit near 0.2 — and the
        // correlation floor is what does that instead.
        let stft = Stft::for_chroma(SR);
        let chroma = |sig: &[f32]| chromagram(&stft.magnitudes(sig, SR));
        let mut n = testsig::Noise::new(5);
        let noise: Vec<f32> = (0..SR as usize * 12)
            .map(|_| n.next_sample() * 0.3)
            .collect();

        let chords = chroma(&testsig::chord_progression(0, Quality::Major, 12.0, SR)).salience();
        let drums = chroma(&testsig::groove(128.0, 12.0, SR, Groove::FourOnFloor)).salience();
        let noise = chroma(&noise).salience();

        assert!(noise < STRUCTURE_FLOOR, "noise {noise}");
        assert!(drums > STRUCTURE_SATURATION, "drums {drums}");
        assert!(chords > STRUCTURE_SATURATION, "chords {chords}");
    }

    #[test]
    fn the_correlation_floor_is_what_rejects_percussion() {
        // Drums correlate with some profile about as well as anything does,
        // and no better than the floor. Harmony clears it outright.
        let stft = Stft::for_chroma(SR);
        let best = |sig: &[f32]| {
            let c = chromagram(&stft.magnitudes(sig, SR));
            estimate_key_scored(&c, KeyProfile::Edm)
                .map(|(_, s)| s.correlation)
                .unwrap_or(0.0)
        };
        for (label, sig) in [
            ("click", testsig::click_track(122.0, 16.0, SR)),
            (
                "four on floor",
                testsig::groove(128.0, 16.0, SR, Groove::FourOnFloor),
            ),
            (
                "breakbeat",
                testsig::groove(170.0, 16.0, SR, Groove::Breakbeat),
            ),
        ] {
            let r = best(&sig);
            assert!(r < KEY_CORRELATION_FLOOR, "{label} correlated {r}");
        }
        let chords = best(&testsig::chord_progression(9, Quality::Minor, 16.0, SR));
        assert!(chords > KEY_CORRELATION_SATURATION, "chords {chords}");
    }

    #[test]
    fn scoring_reads_back_under_the_name_the_factor_used_to_have() {
        let json = serde_json::json!({
            "correlation": 0.9, "runner_up": 0.6, "salience": 0.5,
            "tonal_pitch_classes": 6.0, "strength": 1.0, "margin": 0.9,
            "tonality": 0.8, "coverage": 1.0,
        });
        let s: KeyScoring = serde_json::from_value(json).expect("pre-rename scoring");
        assert_eq!(s.structure, 0.8);
    }

    #[test]
    fn a_clip_too_short_to_average_names_no_key() {
        // Noise over a short chroma has not been flattened yet, so the salience
        // floor does not catch it and a random chroma shape correlates as well
        // as anything. Confidence reached 0.89 on a fifth of a second.
        //
        // Both rates, because the chroma window rounds to a power of two: the
        // cutoff is 33 frames at 44.1kHz and 18 at 48kHz, and a frame count
        // would have made it 1.6s at one and 3.0s at the other.
        for sr in [44_100u32, 48_000] {
            let stft = Stft::for_chroma(sr);
            for secs in [0.2f32, 0.5, 1.0, MIN_CHROMA_SECS - 0.1] {
                for seed in 1..25u32 {
                    let mut g = testsig::Noise::new(seed);
                    let sig: Vec<f32> = (0..(sr as f32 * secs) as usize)
                        .map(|_| g.next_sample() * 0.3)
                        .collect();
                    let chroma = chromagram(&stft.magnitudes(&sig, sr));
                    assert!(
                        estimate_key_scored(&chroma, KeyProfile::Edm).is_none(),
                        "{sr}Hz, {secs}s, seed {seed} produced a key"
                    );
                }
            }
        }
    }

    #[test]
    fn the_short_clip_cutoff_is_the_same_duration_at_every_sample_rate() {
        // The regression this guards: the chroma window rounds to a power of
        // two, so both it and the hop change with the sample rate. A frame
        // count put the cutoff at 1.6s against 3.0s, and counting hops without
        // the window still put it at 1.67s against 1.79s — either way the same
        // clip named a key at one rate and nothing at the other. The rates
        // below straddle both rounding steps.
        for sr in [22_050u32, 44_100, 48_000, 88_200, 96_000] {
            let stft = Stft::for_chroma(sr);
            let key_of = |secs: f32| {
                let sig = testsig::chord_progression(9, Quality::Minor, secs, sr);
                let chroma = chromagram(&stft.magnitudes(&sig, sr));
                (
                    estimate_key_scored(&chroma, KeyProfile::Edm).is_some(),
                    chroma.seconds,
                )
            };
            let (short, short_secs) = key_of(MIN_CHROMA_SECS - 0.1);
            assert!(!short, "{sr}Hz: named a key from {short_secs}s of chroma");
            let (long, long_secs) = key_of(MIN_CHROMA_SECS + 0.1);
            assert!(long, "{sr}Hz: named no key from {long_secs}s of chroma");
        }
    }

    #[test]
    fn white_noise_does_not_favour_any_key() {
        // The bin-count normalization exists for this: without it the uneven
        // number of FFT bins per pitch class gives noise a lopsided chroma.
        let mut n = testsig::Noise::new(7);
        let sig: Vec<f32> = (0..SR as usize * 8)
            .map(|_| n.next_sample() * 0.3)
            .collect();
        let stft = Stft::for_chroma(SR);
        let chroma = chromagram(&stft.magnitudes(&sig, SR));
        let mean = chroma.bins.iter().sum::<f32>() / 12.0;
        let spread = chroma
            .bins
            .iter()
            .map(|v| (v - mean).abs() / mean)
            .fold(0.0f32, f32::max);
        assert!(spread < 0.10, "noise chroma is lopsided: {:?}", chroma.bins);
    }

    #[test]
    fn silence_yields_no_key() {
        assert!(key_of(&vec![0.0f32; SR as usize * 2], KeyProfile::Edm).is_none());
    }

    /// Scorer output on fixed material, as (key, confidence, correlation).
    /// Re-pin only alongside a [`KEY_SCORER_VERSION`] bump.
    const PINNED_VERSION: u32 = 3;
    type Pin = (&'static str, KeyProfile, Option<(&'static str, f32, f32)>);
    #[rustfmt::skip]
    const PINNED: &[Pin] = &[
        ("C major", KeyProfile::Edm, Some(("C major", 0.977, 0.960))),
        ("C major", KeyProfile::Krumhansl, Some(("C major", 0.963, 0.938))),
        ("A minor", KeyProfile::Edm, Some(("A minor", 0.903, 0.957))),
        ("A minor", KeyProfile::Krumhansl, Some(("A minor", 0.915, 0.947))),
        ("F# major", KeyProfile::Edm, Some(("F# major", 0.930, 0.926))),
        ("F# major", KeyProfile::Krumhansl, Some(("F# major", 0.919, 0.933))),
        ("Eb minor", KeyProfile::Edm, Some(("Eb minor", 0.930, 0.940))),
        ("Eb minor", KeyProfile::Krumhansl, Some(("Eb minor", 0.873, 0.868))),
        ("Eb minor over drums", KeyProfile::Edm, Some(("Eb minor", 0.934, 0.939))),
        ("Eb minor over drums", KeyProfile::Krumhansl, Some(("Eb minor", 0.880, 0.869))),
        ("relative coin flip", KeyProfile::Edm, Some(("C major", 0.133, 0.853))),
        ("relative coin flip", KeyProfile::Krumhansl, Some(("C major", 0.051, 0.821))),
        ("riff over drums", KeyProfile::Edm, Some(("E minor", 0.191, 0.848))),
        ("riff over drums", KeyProfile::Krumhansl, Some(("E minor", 0.167, 0.819))),
        ("power chords", KeyProfile::Edm, Some(("B major", 0.000, 0.833))),
        ("power chords", KeyProfile::Krumhansl, Some(("B major", 0.000, 0.844))),
        ("whole-tone chords", KeyProfile::Edm, Some(("Bb major", 0.000, 0.247))),
        ("whole-tone chords", KeyProfile::Krumhansl, Some(("Bb major", 0.000, 0.242))),
        ("drums", KeyProfile::Edm, Some(("D minor", 0.000, 0.401))),
        ("drums", KeyProfile::Krumhansl, Some(("Bb major", 0.000, 0.336))),
        ("clicks", KeyProfile::Edm, Some(("B minor", 0.000, 0.356))),
        ("clicks", KeyProfile::Krumhansl, Some(("B minor", 0.000, 0.393))),
        ("noise", KeyProfile::Edm, Some(("G minor", 0.000, 0.338))),
        ("noise", KeyProfile::Krumhansl, Some(("D minor", 0.000, 0.362))),
        ("short clip", KeyProfile::Edm, None),
        ("short clip", KeyProfile::Krumhansl, None),
    ];

    fn pinned_material() -> Vec<(&'static str, Vec<f32>)> {
        let c: [f32; 4] = [48.0, 52.0, 55.0, 60.0];
        let am: [f32; 4] = [45.0, 48.0, 52.0, 57.0];
        let wt1: [f32; 3] = [60.0, 64.0, 68.0];
        let wt2: [f32; 3] = [62.0, 66.0, 70.0];
        let mut n = testsig::Noise::new(7);
        vec![
            (
                "C major",
                testsig::chord_progression(0, Quality::Major, 8.0, SR),
            ),
            (
                "A minor",
                testsig::chord_progression(9, Quality::Minor, 8.0, SR),
            ),
            (
                "F# major",
                testsig::chord_progression(6, Quality::Major, 8.0, SR),
            ),
            (
                "Eb minor",
                testsig::chord_progression(3, Quality::Minor, 8.0, SR),
            ),
            (
                "Eb minor over drums",
                over_drums(
                    &testsig::chord_progression(3, Quality::Minor, 12.0, SR),
                    1.0,
                    0.8,
                ),
            ),
            (
                "relative coin flip",
                testsig::note_sequence(&[&c, &am, &c, &am], 12.0, SR),
            ),
            ("riff over drums", over_drums(&three_note_riff(), 1.0, 0.8)),
            ("power chords", power_chords()),
            (
                "whole-tone chords",
                testsig::note_sequence(&[&wt1, &wt2, &wt1, &wt2], 12.0, SR),
            ),
            (
                "drums",
                testsig::groove(128.0, 12.0, SR, Groove::FourOnFloor),
            ),
            ("clicks", testsig::click_track(122.0, 12.0, SR)),
            (
                "noise",
                (0..SR as usize * 8)
                    .map(|_| n.next_sample() * 0.3)
                    .collect(),
            ),
            (
                "short clip",
                testsig::chord_progression(0, Quality::Major, 1.0, SR),
            ),
        ]
    }

    #[test]
    fn key_output_only_changes_with_the_scorer_version() {
        // selecta targets stored keys by their `@N` label, so an output change
        // under an unchanged label strands every key measured before it.
        let stft = Stft::for_chroma(SR);
        let mut observed = Vec::new();
        for (name, sig) in pinned_material() {
            let chroma = chromagram(&stft.magnitudes(&sig, SR));
            for profile in [KeyProfile::Edm, KeyProfile::Krumhansl] {
                let got = estimate_key_scored(&chroma, profile)
                    .map(|(e, s)| (e.key, e.confidence, s.correlation));
                observed.push((name, profile, got));
            }
        }
        // Loose enough for libm differences between platforms, far tighter
        // than any deliberate scoring change moves a number.
        let matches = observed.len() == PINNED.len()
            && observed
                .iter()
                .zip(PINNED)
                .all(|((n, p, got), (pn, pp, want))| {
                    n == pn
                        && p == pp
                        && match (got, want) {
                            (None, None) => true,
                            (Some((k, c, r)), Some((wk, wc, wr))) => {
                                k == wk && (c - wc).abs() <= 0.005 && (r - wr).abs() <= 0.005
                            }
                            _ => false,
                        }
                });
        let table: String = observed
            .iter()
            .map(|(n, p, got)| match got {
                None => format!("    ({n:?}, KeyProfile::{p:?}, None),\n"),
                Some((k, c, r)) => {
                    format!("    ({n:?}, KeyProfile::{p:?}, Some(({k:?}, {c:.3}, {r:.3}))),\n")
                }
            })
            .collect();
        assert!(
            matches,
            "key scorer output changed: bump KEY_SCORER_VERSION, then set \
             PINNED_VERSION to match and PINNED to:\n{table}"
        );
        assert_eq!(
            PINNED_VERSION, KEY_SCORER_VERSION,
            "KEY_SCORER_VERSION and PINNED_VERSION move together"
        );
    }

    #[test]
    fn profile_names_parse_the_way_the_cli_spells_them() {
        assert_eq!(KeyProfile::parse("edm"), Some(KeyProfile::Edm));
        assert_eq!(KeyProfile::parse("Krumhansl"), Some(KeyProfile::Krumhansl));
        assert_eq!(KeyProfile::parse("ks"), Some(KeyProfile::Krumhansl));
        assert_eq!(KeyProfile::parse("nonsense"), None);
    }
}
