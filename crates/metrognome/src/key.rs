//! Musical key estimation.
//!
//! Chromagram, then correlation against key profiles for all 24 rotations. The
//! output carries both standard notation and Camelot, because the consumer is a
//! DJ-adjacent tool and Camelot is what harmonic mixing actually uses.

use crate::dsp::{Spectrogram, CHROMA_FMAX, CHROMA_FMIN};
use crate::types::{Alternate, KeyEstimate, UNCERTAIN_AT_OR_BELOW};

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

    fn source(self) -> &'static str {
        match self {
            KeyProfile::Krumhansl => "metrognome/chroma-correlation-krumhansl@1",
            KeyProfile::Edm => "metrognome/chroma-correlation-edm@1",
        }
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

/// Correlation at which a key is considered clearly stated.
///
/// Real music never approaches 1, since the chroma carries percussion too.
/// 0.75 is about what an unambiguous tonal centre reaches.
const KEY_CORRELATION_SATURATION: f32 = 0.75;

/// Correlation gap at which the winner is considered clearly ahead.
///
/// Relative major/minor pairs share six of seven notes, so they routinely sit
/// within 0.05 of each other. A tenth is a real separation.
const KEY_MARGIN_SATURATION: f32 = 0.10;

/// Chroma salience below which a clip is treated as having no tonal content.
///
/// Measured percussion sits at 0.20-0.25 and white noise near 0.005, so a floor
/// here zeroes out the confidence of anything that is only drums.
const TONALITY_FLOOR: f32 = 0.15;

/// Salience at which tonal content is no longer in doubt.
///
/// Sustained chords measure near 1.0 and a full arrangement near 0.8. Below
/// both, so heavy percussion is not penalized but a drums-only clip cannot
/// climb out.
const TONALITY_SATURATION: f32 = 0.55;

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
    if chroma.frames == 0 {
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

    let strength = (r1 / KEY_CORRELATION_SATURATION).clamp(0.0, 1.0);
    let margin = ((r1 - r2) / KEY_MARGIN_SATURATION).clamp(0.0, 1.0);
    // A gate rather than another factor: below the floor there is nothing
    // tonal to have an opinion about, however well the profiles happen to fit.
    let tonality = ((chroma.salience() - TONALITY_FLOOR) / (TONALITY_SATURATION - TONALITY_FLOOR))
        .clamp(0.0, 1.0);
    // Strength and margin neither substitute for the other: a strong
    // correlation that ties with the relative minor is still a coin flip, and a
    // clear winner among uniformly weak correlations is noise.
    let confidence = strength.sqrt() * margin.sqrt() * tonality;

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

    Some(KeyEstimate {
        key: format!("{} {mode}", PITCH_NAMES[tonic]),
        tonic: PITCH_NAMES[tonic].to_string(),
        mode: mode.to_string(),
        camelot: camelot(tonic, is_minor),
        confidence: (confidence * 1000.0).round() / 1000.0,
        uncertain: confidence <= UNCERTAIN_AT_OR_BELOW,
        source: profile.source().to_string(),
        alternates,
    })
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
        // The tonality gate is what stops this reading as a real key.
        let sig = testsig::click_track(122.0, 20.0, SR);
        match key_of(&sig, KeyProfile::Edm) {
            None => {}
            Some(est) => assert!(est.uncertain, "click track read as a key: {est:?}"),
        }
    }

    #[test]
    fn salience_separates_tonal_material_from_percussion() {
        let stft = Stft::for_chroma(SR);
        let chords = chromagram(
            &stft.magnitudes(&testsig::chord_progression(0, Quality::Major, 12.0, SR), SR),
        );
        let drums = chromagram(
            &stft.magnitudes(&testsig::groove(128.0, 12.0, SR, Groove::FourOnFloor), SR),
        );
        assert!(
            chords.salience() > TONALITY_SATURATION,
            "{}",
            chords.salience()
        );
        assert!(
            drums.salience() < TONALITY_FLOOR + 0.15,
            "{}",
            drums.salience()
        );
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

    #[test]
    fn profile_names_parse_the_way_the_cli_spells_them() {
        assert_eq!(KeyProfile::parse("edm"), Some(KeyProfile::Edm));
        assert_eq!(KeyProfile::parse("Krumhansl"), Some(KeyProfile::Krumhansl));
        assert_eq!(KeyProfile::parse("ks"), Some(KeyProfile::Krumhansl));
        assert_eq!(KeyProfile::parse("nonsense"), None);
    }
}
