//! The band scorer: a key profile per register, fitted to labelled dance music.
//!
//! Correlating one twelve-bin chroma against a hand-made profile reads a bass
//! line, a pad and a hi-hat's ringing partials as the same kind of evidence.
//! Here each register keeps its own chroma and its own learned profile, and the
//! 24 key scores go through a softmax, so confidence is a probability that was
//! measured against held-out tracks rather than a product of hand-set ramps.
//! DECISIONS.md entry 45.

use super::{weights, Chromagram};
use crate::dsp::Spectrogram;

/// Registers the chroma is split into, as MIDI note ranges `[lo, hi)`.
///
/// Octaves through the bass and middle, where the root and the chord tones
/// live, then one wide band above C6 for leads and upper partials.
pub const BANDS: [(u8, u8); 5] = [(24, 36), (36, 48), (48, 60), (60, 72), (72, 96)];

/// Log-frequency grid resolution, in bins per semitone.
///
/// Three, so the whitening below can tell a note's peak from the trough
/// between notes; only the bin on each semitone is read as evidence.
const GRID_PER_SEMITONE: usize = 3;

/// Lowest grid pitch, C1. The bottom of [`BANDS`].
const GRID_MIDI_LO: f32 = 24.0;

/// Grid bins, C1 up to C8: past the top band by more than the whitening reach.
const GRID_BINS: usize = 84 * GRID_PER_SEMITONE;

/// Dynamic range the log compression keeps, relative to the clip's peak.
///
/// Without it a loud bass note outvotes every chord tone above it; measured on
/// held-out tracks, 10x to 1000x score alike and no compression scores worst.
const LOG_GAIN: f32 = 100.0;

/// Whitening window across frequency, in grid bins: six semitones.
///
/// Subtracting the local spectral level strips what percussion and timbre add
/// to every pitch class, leaving the peaks notes put there. Measured best of
/// 0, 12, 18 and 36 bins; none at all cost five points of accuracy.
const WHITEN_BINS: usize = 18;

/// A chroma per register, summed over the clip.
#[derive(Debug, Clone, Default)]
pub struct BandChroma {
    /// Whitened energy per pitch class in each of [`BANDS`], C first,
    /// normalized so all bands together sum to 12.
    pub bands: [[f32; 12]; BANDS.len()],
    /// All bands folded together, for the structure and coverage gates.
    pub total: Chromagram,
}

/// How each grid bin reads the spectrum: the strongest FFT bin within a sixth
/// of a semitone, or an interpolation where no bin falls that close.
enum GridTap {
    Peak(usize, usize),
    Lerp(usize, f32),
}

fn grid_taps(spec: &Spectrogram) -> Vec<GridTap> {
    let df = spec.sample_rate as f32 / spec.n_fft as f32;
    let hz = |midi: f32| 440.0 * 2f32.powf((midi - 69.0) / 12.0);
    (0..GRID_BINS)
        .map(|j| {
            let m = GRID_MIDI_LO + j as f32 / GRID_PER_SEMITONE as f32;
            let half = 0.5 / GRID_PER_SEMITONE as f32;
            let lo = (hz(m - half) / df).ceil() as usize;
            let hi = (hz(m + half) / df).floor() as usize;
            if hi >= lo {
                GridTap::Peak(lo.min(spec.bins - 1), hi.min(spec.bins - 1))
            } else {
                let x = hz(m) / df;
                GridTap::Lerp(x.floor() as usize, x.fract())
            }
        })
        .collect()
}

/// Build per-register chroma from a [`crate::dsp::Stft::for_key_bands`] spectrogram.
pub fn band_chroma(spec: &Spectrogram) -> BandChroma {
    let mut out = BandChroma::default();
    if spec.frames == 0 {
        return out;
    }
    let taps = grid_taps(spec);
    let mut grid = vec![0.0f32; spec.frames * GRID_BINS];
    for t in 0..spec.frames {
        let row = spec.frame(t);
        for (g, tap) in grid[t * GRID_BINS..(t + 1) * GRID_BINS]
            .iter_mut()
            .zip(&taps)
        {
            *g = match *tap {
                GridTap::Peak(lo, hi) => row[lo..=hi].iter().copied().fold(0.0, f32::max),
                GridTap::Lerp(i, fr) if i + 1 < row.len() => row[i] * (1.0 - fr) + row[i + 1] * fr,
                GridTap::Lerp(..) => 0.0,
            };
        }
    }
    let peak = grid.iter().copied().fold(0.0f32, f32::max);
    if peak <= 1e-9 {
        return out;
    }
    for g in grid.iter_mut() {
        *g = (LOG_GAIN * *g / peak).ln_1p();
    }

    let mut frame = [0.0f32; GRID_BINS];
    let mut prefix = [0.0f32; GRID_BINS + 1];
    for t in 0..spec.frames {
        frame.copy_from_slice(&grid[t * GRID_BINS..(t + 1) * GRID_BINS]);
        for j in 0..GRID_BINS {
            prefix[j + 1] = prefix[j] + frame[j];
        }
        for (b, &(lo, hi)) in BANDS.iter().enumerate() {
            for midi in lo..hi {
                let j = (usize::from(midi) - GRID_MIDI_LO as usize) * GRID_PER_SEMITONE;
                let a = j.saturating_sub(WHITEN_BINS / 2);
                let z = (j + WHITEN_BINS / 2).min(GRID_BINS);
                let level = (prefix[z] - prefix[a]) / (z - a) as f32;
                out.bands[b][usize::from(midi) % 12] += (frame[j] - level).max(0.0);
            }
        }
    }

    let sum: f32 = out.bands.iter().flatten().sum();
    if sum > 1e-9 {
        for v in out.bands.iter_mut().flatten() {
            *v *= 12.0 / sum;
        }
    }
    for band in &out.bands {
        for (acc, v) in out.total.bins.iter_mut().zip(band) {
            *acc += v;
        }
    }
    out.total.frames = spec.frames;
    out.total.seconds =
        spec.n_fft as f32 / spec.sample_rate as f32 + (spec.frames - 1) as f32 / spec.fps;
    out
}

/// Probability of each key, indexed `tonic * 2 + minor`.
pub(super) fn key_probabilities(chroma: &BandChroma) -> [f32; 24] {
    let mut scores = [0.0f32; 24];
    for tonic in 0..12 {
        for minor in 0..2 {
            let mut s = weights::MODE_BIAS[minor];
            for (w, band) in weights::PROFILES[minor].iter().zip(&chroma.bands) {
                for i in 0..12 {
                    s += w[i] * band[(tonic + i) % 12];
                }
            }
            scores[tonic * 2 + minor] = s;
        }
    }
    let top = scores.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let mut total = 0.0;
    for s in scores.iter_mut() {
        *s = (*s - top).exp();
        total += *s;
    }
    scores.map(|s| s / total)
}
