//! Samples in, features out.
//!
//! Pure DSP over mono PCM and a sample rate, with no idea a preview URL, a
//! file or a browser exists. Every front end, from the CLI's preview pipeline
//! to the WebAssembly build, passes through here.

use crate::dsp::{onset_envelope, Stft};
use crate::key::{chromagram, estimate_key_scored, KeyProfile};
use crate::tempo::estimate_tempo;
use crate::types::Features;

/// Knobs for the DSP itself, as opposed to the network around it.
#[derive(Debug, Clone, Copy, Default)]
pub struct AnalysisOptions {
    /// Which key profile set to correlate against.
    pub key_profile: KeyProfile,
    /// Attach the per-factor breakdown behind the key confidence.
    ///
    /// Off by default: the diagnostic commands want it, a batch of three
    /// thousand tracks does not.
    pub explain_key_scoring: bool,
}

impl AnalysisOptions {
    /// A stable string identifying these options, used as part of the cache
    /// key. Anything that changes the output must appear here, or a cache hit
    /// will serve an answer produced under different settings.
    pub fn cache_key(&self) -> String {
        // The breakdown is part of the stored payload, not just a rendering of
        // it, so a row cached without it cannot serve a caller that asked for
        // it — nor the reverse.
        format!(
            "key_profile={:?},explain_key_scoring={}",
            self.key_profile, self.explain_key_scoring
        )
        .to_ascii_lowercase()
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
            estimate_key_scored(&chroma, options.key_profile).map(|(mut est, scoring)| {
                if options.explain_key_scoring {
                    est.scoring = Some(scoring);
                }
                est
            })
        },
    );
    Features { tempo, key }
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

    #[test]
    fn the_cache_key_changes_with_anything_that_changes_the_output() {
        let a = AnalysisOptions {
            key_profile: KeyProfile::Edm,
            ..Default::default()
        };
        let b = AnalysisOptions {
            key_profile: KeyProfile::Krumhansl,
            ..Default::default()
        };
        assert_ne!(a.cache_key(), b.cache_key());
    }

    #[test]
    fn the_cache_key_separates_diagnostic_rows_from_plain_ones() {
        // A hit serves its stored payload verbatim, so a row cached without the
        // breakdown would answer a request for it with nothing.
        let plain = AnalysisOptions::default();
        let explained = AnalysisOptions {
            explain_key_scoring: true,
            ..Default::default()
        };
        assert_ne!(plain.cache_key(), explained.cache_key());
    }

    #[test]
    fn the_scoring_breakdown_is_attached_only_when_asked() {
        let sig = crate::testsig::chord_progression(9, crate::testsig::Quality::Minor, 8.0, 44_100);
        let plain = analyze_pcm_with(&sig, 44_100, &AnalysisOptions::default());
        assert!(plain.key.as_ref().expect("key").scoring.is_none());

        let explained = analyze_pcm_with(
            &sig,
            44_100,
            &AnalysisOptions {
                explain_key_scoring: true,
                ..Default::default()
            },
        );
        let key = explained.key.as_ref().expect("key");
        let s = key.scoring.expect("scoring");
        // The confidence is exactly the product of the reported factors, so a
        // surprising number can always be attributed to one of them.
        let product = s.strength * s.margin * s.structure * s.coverage;
        assert!(
            (product - key.confidence).abs() < 0.002,
            "{product} vs {}",
            key.confidence
        );
        assert!(s.correlation > s.runner_up);
    }
}
