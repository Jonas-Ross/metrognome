//! Shared spectral machinery: STFT, mel filterbank, onset envelope,
//! autocorrelation.
//!
//! Nothing here knows about tempo or key. All window and hop sizes are derived
//! from the sample rate in *seconds*, never hardcoded in samples, so the same
//! code behaves identically on a 44.1 kHz preview and a 48 kHz capture tap.

use std::f32::consts::PI;
use std::sync::Arc;

use rustfft::{num_complex::Complex32, Fft, FftPlanner};

/// Analysis window for onset detection.
///
/// ~46 ms is the standard onset-detection compromise: long enough that a bass
/// drum's fundamental resolves into its own mel band, short enough that two
/// hits 60 ms apart stay separate events.
const ONSET_WINDOW_SECS: f32 = 0.046;

/// Onset hop as a fraction of the window.
///
/// 1/8 (87.5% overlap) rather than the more usual 1/4. The envelope frame rate
/// *is* the resolution of the autocorrelation lag axis, and at 1/4 one lag bin
/// near 174 BPM is worth about 6 BPM — too coarse to seed a tempo search.
const ONSET_HOP_DIVISOR: usize = 8;

/// Analysis window for chroma.
///
/// Analysis window for chroma.
///
/// ~185 ms (8192 samples at 44.1 kHz), four times the onset window. Key
/// detection trades time resolution for frequency resolution without regret:
/// 5.4 Hz bins are what make adjacent semitones separable from A2 (110 Hz)
/// upward, which is where [`CHROMA_FMIN`] comes from. At the onset window's
/// 46 ms the bins are 21.5 Hz wide and a whole octave of the bass register
/// collapses into one bin.
const CHROMA_WINDOW_SECS: f32 = 0.185;

/// Lowest frequency admitted to the chromagram.
///
/// A2. One semitone here is 6.5 Hz, just above the 5.4 Hz bin spacing, so this
/// is the lowest pitch the transform can actually resolve. Below it, adjacent
/// notes share bins and a sub-bass line would smear across pitch classes —
/// worse than useless, because kick drum energy lives there too.
pub const CHROMA_FMIN: f32 = 110.0;

/// Highest frequency admitted to the chromagram.
///
/// A7. Above this, partials from different notes are dense enough to overlap
/// and contribute more noise than tonal evidence.
pub const CHROMA_FMAX: f32 = 3520.0;

/// Number of mel bands in the onset filterbank.
///
/// 64 bands is enough to keep a hi-hat and a kick in separate bands (so their
/// fluxes add rather than mask) without making each band so narrow that
/// vibrato registers as an onset.
const N_MELS: usize = 64;

/// Low edge of the mel filterbank. Below this is rumble and DC offset.
const MEL_FMIN: f32 = 30.0;

/// High edge of the mel filterbank, capped at Nyquist.
///
/// Above ~11 kHz there is only cymbal wash, which is sustained rather than
/// transient and mostly adds noise to the flux.
const MEL_FMAX: f32 = 11_000.0;

/// Log-compression constant for the mel magnitudes.
///
/// `ln(1 + gamma*S)`: at gamma = 1000 a quiet hat and a loud kick contribute
/// comparably to the flux, which is what we want — tempo is carried by event
/// timing, not by event loudness.
const LOG_COMPRESSION_GAMMA: f32 = 1000.0;

/// Window, in seconds, of the moving average subtracted from the raw flux.
///
/// This is a high-pass at ~0.67 Hz. It removes build-ups and filter sweeps
/// (which otherwise dominate the autocorrelation at long lags) while sitting
/// safely below the 1.5-3 Hz band where beats actually live.
const FLUX_DETREND_SECS: f32 = 1.5;

/// Smallest power of two at or above `n`.
fn next_pow2(n: usize) -> usize {
    let mut p = 1;
    while p < n {
        p <<= 1;
    }
    p
}

/// A magnitude spectrogram stored row-major, one row per frame.
#[derive(Debug, Clone)]
pub struct Spectrogram {
    /// Number of frames.
    pub frames: usize,
    /// Number of frequency bins (`n_fft / 2 + 1`).
    pub bins: usize,
    /// Frame rate in Hz.
    pub fps: f32,
    /// Sample rate the spectrogram was computed at.
    pub sample_rate: u32,
    /// FFT size used.
    pub n_fft: usize,
    data: Vec<f32>,
}

impl Spectrogram {
    /// Magnitudes for frame `t`.
    pub fn frame(&self, t: usize) -> &[f32] {
        &self.data[t * self.bins..(t + 1) * self.bins]
    }

    /// Centre frequency of bin `b` in Hz.
    pub fn bin_hz(&self, b: usize) -> f32 {
        b as f32 * self.sample_rate as f32 / self.n_fft as f32
    }
}

/// Reusable STFT with a periodic Hann window.
pub struct Stft {
    n_fft: usize,
    hop: usize,
    window: Vec<f32>,
    fft: Arc<dyn Fft<f32>>,
}

impl Stft {
    /// New STFT with the given FFT size and hop, both in samples.
    pub fn new(n_fft: usize, hop: usize) -> Self {
        // Periodic (not symmetric) Hann: the correct choice when frames overlap,
        // because it sums to a constant under overlap-add.
        let window = (0..n_fft)
            .map(|i| 0.5 - 0.5 * (2.0 * PI * i as f32 / n_fft as f32).cos())
            .collect();
        let fft = FftPlanner::<f32>::new().plan_fft_forward(n_fft);
        Stft {
            n_fft,
            hop,
            window,
            fft,
        }
    }

    /// STFT sized for onset detection at `sample_rate`.
    pub fn for_onsets(sample_rate: u32) -> Self {
        let n_fft = next_pow2((ONSET_WINDOW_SECS * sample_rate as f32) as usize);
        Stft::new(n_fft, (n_fft / ONSET_HOP_DIVISOR).max(1))
    }

    /// STFT sized for chroma at `sample_rate`.
    pub fn for_chroma(sample_rate: u32) -> Self {
        let n_fft = next_pow2((CHROMA_WINDOW_SECS * sample_rate as f32) as usize);
        Stft::new(n_fft, (n_fft / 4).max(1))
    }

    /// Frame rate in Hz for this STFT at `sample_rate`.
    pub fn fps(&self, sample_rate: u32) -> f32 {
        sample_rate as f32 / self.hop as f32
    }

    /// Compute the magnitude spectrogram of `samples`.
    ///
    /// Returns an empty spectrogram when the input is shorter than one window
    /// rather than zero-padding: a single padded frame carries no useful
    /// spectral information and would make callers guess whether it was real.
    pub fn magnitudes(&self, samples: &[f32], sample_rate: u32) -> Spectrogram {
        let bins = self.n_fft / 2 + 1;
        if samples.len() < self.n_fft {
            return Spectrogram {
                frames: 0,
                bins,
                fps: self.fps(sample_rate),
                sample_rate,
                n_fft: self.n_fft,
                data: Vec::new(),
            };
        }
        let frames = (samples.len() - self.n_fft) / self.hop + 1;
        let mut data = vec![0.0f32; frames * bins];
        let mut scratch = vec![Complex32::default(); self.n_fft];

        for t in 0..frames {
            let start = t * self.hop;
            for (i, s) in scratch.iter_mut().enumerate() {
                *s = Complex32::new(samples[start + i] * self.window[i], 0.0);
            }
            self.fft.process(&mut scratch);
            let row = &mut data[t * bins..(t + 1) * bins];
            for (b, out) in row.iter_mut().enumerate() {
                *out = scratch[b].norm();
            }
        }

        Spectrogram {
            frames,
            bins,
            fps: self.fps(sample_rate),
            sample_rate,
            n_fft: self.n_fft,
            data,
        }
    }
}

fn hz_to_mel(hz: f32) -> f32 {
    2595.0 * (1.0 + hz / 700.0).log10()
}

fn mel_to_hz(mel: f32) -> f32 {
    700.0 * (10f32.powf(mel / 2595.0) - 1.0)
}

/// Triangular mel filterbank as (start_bin, weights) pairs.
///
/// Sparse rather than a dense matrix: each filter touches a handful of bins,
/// and the dense form would be 64 x 1025 floats of mostly zeros per call.
fn mel_filterbank(spec: &Spectrogram) -> Vec<(usize, Vec<f32>)> {
    let nyquist = spec.sample_rate as f32 / 2.0;
    let fmax = MEL_FMAX.min(nyquist);
    let mel_lo = hz_to_mel(MEL_FMIN);
    let mel_hi = hz_to_mel(fmax);
    let edges: Vec<f32> = (0..N_MELS + 2)
        .map(|i| mel_to_hz(mel_lo + (mel_hi - mel_lo) * i as f32 / (N_MELS + 1) as f32))
        .collect();

    let bin_hz = spec.sample_rate as f32 / spec.n_fft as f32;
    let mut out = Vec::with_capacity(N_MELS);
    for m in 0..N_MELS {
        let (lo, ctr, hi) = (edges[m], edges[m + 1], edges[m + 2]);
        let b_lo = (lo / bin_hz).ceil().max(0.0) as usize;
        let b_hi = ((hi / bin_hz).floor() as usize).min(spec.bins - 1);
        if b_hi < b_lo {
            out.push((b_lo.min(spec.bins - 1), vec![0.0]));
            continue;
        }
        let mut w = Vec::with_capacity(b_hi - b_lo + 1);
        for b in b_lo..=b_hi {
            let f = b as f32 * bin_hz;
            let v = if f <= ctr {
                (f - lo) / (ctr - lo).max(1e-9)
            } else {
                (hi - f) / (hi - ctr).max(1e-9)
            };
            w.push(v.max(0.0));
        }
        out.push((b_lo, w));
    }
    out
}

/// A percussive onset strength envelope.
#[derive(Debug, Clone)]
pub struct OnsetEnvelope {
    /// Detrended, zero-mean, unit-variance onset strength, one value per frame.
    pub values: Vec<f32>,
    /// Frame rate in Hz.
    pub fps: f32,
    /// Seconds to add to a frame time to get back to the time of the event
    /// that caused it. Flux at frame `t` compares two overlapping windows, so
    /// it fires as soon as a transient enters the newer one — up to a whole
    /// window before that transient is actually centred. Good to about
    /// +/- 20 ms; exact latency depends on how loud the surrounding audio is.
    pub latency_secs: f32,
    /// Ratio of the mean beat-band flux to its own standard deviation before
    /// normalization. Low values mean a smooth, beatless clip (an ambient
    /// intro), which is the main reason a confident-looking estimate is wrong.
    pub pulse_strength: f32,
}

/// Compute the onset strength envelope from a magnitude spectrogram.
///
/// Mel-band log-magnitude, half-wave-rectified first difference, summed across
/// bands, then detrended. This is the standard spectral-flux recipe; the mel
/// stage matters because it stops a single loud low-frequency band from
/// swamping the contribution of the hats that actually carry a fast grid.
pub fn onset_envelope(spec: &Spectrogram) -> OnsetEnvelope {
    if spec.frames < 2 {
        return OnsetEnvelope {
            values: Vec::new(),
            fps: spec.fps,
            latency_secs: 0.0,
            pulse_strength: 0.0,
        };
    }
    let fb = mel_filterbank(spec);

    // Log-compressed mel magnitudes, frame-major.
    let mut mel = vec![0.0f32; spec.frames * N_MELS];
    for t in 0..spec.frames {
        let row = spec.frame(t);
        for (m, (start, weights)) in fb.iter().enumerate() {
            let mut acc = 0.0f32;
            for (i, w) in weights.iter().enumerate() {
                if let Some(v) = row.get(start + i) {
                    acc += v * w;
                }
            }
            mel[t * N_MELS + m] = (1.0 + LOG_COMPRESSION_GAMMA * acc).ln();
        }
    }

    // Half-wave rectified first difference: only energy *increases* are onsets.
    let mut flux = vec![0.0f32; spec.frames];
    for t in 1..spec.frames {
        let mut acc = 0.0f32;
        for m in 0..N_MELS {
            let d = mel[t * N_MELS + m] - mel[(t - 1) * N_MELS + m];
            if d > 0.0 {
                acc += d;
            }
        }
        flux[t] = acc;
    }
    flux[0] = flux[1];

    let raw_mean = flux.iter().sum::<f32>() / flux.len() as f32;
    let raw_sd = std_dev(&flux, raw_mean);
    // Before detrending: how spiky is the flux relative to its own level? A
    // click track lands near 3, a sustained pad near 0.3.
    let pulse_strength = if raw_mean > 1e-9 {
        raw_sd / raw_mean
    } else {
        0.0
    };

    let win = ((FLUX_DETREND_SECS * spec.fps) as usize).max(3);
    let detrended = subtract_moving_average(&flux, win);
    let mean = detrended.iter().sum::<f32>() / detrended.len() as f32;
    let sd = std_dev(&detrended, mean);
    let values = if sd > 1e-9 {
        detrended.iter().map(|v| (v - mean) / sd).collect()
    } else {
        vec![0.0; detrended.len()]
    };

    let hop = spec.sample_rate as f32 / spec.fps;
    OnsetEnvelope {
        values,
        fps: spec.fps,
        latency_secs: (spec.n_fft as f32 - hop) / spec.sample_rate as f32,
        pulse_strength,
    }
}

fn std_dev(x: &[f32], mean: f32) -> f32 {
    if x.len() < 2 {
        return 0.0;
    }
    (x.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / x.len() as f32).sqrt()
}

/// Subtract a centred moving average, using a running sum so cost is O(n).
fn subtract_moving_average(x: &[f32], win: usize) -> Vec<f32> {
    let n = x.len();
    let half = win / 2;
    let mut prefix = vec![0.0f64; n + 1];
    for i in 0..n {
        prefix[i + 1] = prefix[i] + f64::from(x[i]);
    }
    (0..n)
        .map(|i| {
            let lo = i.saturating_sub(half);
            let hi = (i + half + 1).min(n);
            let avg = (prefix[hi] - prefix[lo]) / (hi - lo) as f64;
            x[i] - avg as f32
        })
        .collect()
}

/// Normalized autocorrelation of `x`, index 0 == lag 0 == 1.0.
///
/// Computed through the FFT (Wiener-Khinchin) because the lag range of interest
/// is wide; the direct form would be O(n * max_lag).
pub fn autocorrelation(x: &[f32], max_lag: usize) -> Vec<f32> {
    let n = x.len();
    if n == 0 {
        return Vec::new();
    }
    // Zero-pad past 2n so the circular correlation the FFT computes equals the
    // linear one we actually want.
    let size = next_pow2(2 * n);
    let mut planner = FftPlanner::<f32>::new();
    let fwd = planner.plan_fft_forward(size);
    let inv = planner.plan_fft_inverse(size);

    let mut buf = vec![Complex32::default(); size];
    for (i, v) in x.iter().enumerate() {
        buf[i] = Complex32::new(*v, 0.0);
    }
    fwd.process(&mut buf);
    for v in buf.iter_mut() {
        *v = Complex32::new(v.norm_sqr(), 0.0);
    }
    inv.process(&mut buf);

    let zero = buf[0].re;
    let lags = max_lag.min(n - 1) + 1;
    if zero.abs() < 1e-12 {
        return vec![0.0; lags];
    }
    (0..lags).map(|k| buf[k].re / zero).collect()
}

/// Convolve `x` with a normalized Hann kernel of `len` samples (forced odd).
///
/// Used to widen onset spikes before tempo scoring. Without it the comb-filter
/// score is a knife edge — an onset is one or two frames wide, so a 0.03% tempo
/// error already walks the grid off the spikes — and no practical search step
/// can find the peak. Smoothing trades a little precision for a searchable
/// landscape, and also absorbs the timing jitter real recordings have.
pub fn smooth(x: &[f32], len: usize) -> Vec<f32> {
    let len = len.max(1) | 1;
    if len == 1 || x.is_empty() {
        return x.to_vec();
    }
    let half = len / 2;
    let kernel: Vec<f32> = (0..len)
        .map(|i| 0.5 - 0.5 * (2.0 * PI * (i as f32 + 0.5) / len as f32).cos())
        .collect();
    let norm: f32 = kernel.iter().sum();
    (0..x.len())
        .map(|i| {
            let mut acc = 0.0f32;
            for (k, w) in kernel.iter().enumerate() {
                let j = i as isize + k as isize - half as isize;
                if j >= 0 && (j as usize) < x.len() {
                    acc += x[j as usize] * w;
                }
            }
            acc / norm
        })
        .collect()
}

/// Linear interpolation into a slice at a fractional index.
///
/// Returns 0.0 outside the slice, which is the right neutral value for both the
/// zero-mean onset envelope and the normalized autocorrelation.
pub fn interp_at(x: &[f32], pos: f32) -> f32 {
    if pos < 0.0 || x.is_empty() {
        return 0.0;
    }
    let i = pos.floor() as usize;
    if i + 1 >= x.len() {
        return *x.get(i).unwrap_or(&0.0);
    }
    let frac = pos - i as f32;
    x[i] * (1.0 - frac) + x[i + 1] * frac
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testsig;

    #[test]
    fn stft_shape_and_sizes_follow_sample_rate() {
        let s = Stft::for_onsets(44_100);
        assert_eq!(s.n_fft, 2048);
        assert_eq!(s.hop, 256);
        assert!((s.fps(44_100) - 172.27).abs() < 0.1);

        let c = Stft::for_chroma(44_100);
        assert_eq!(c.n_fft, 8192);
        assert_eq!(c.hop, 2048);
        // The lowest admitted pitch must stay resolvable: one semitone at
        // CHROMA_FMIN has to be wider than one bin.
        let bin_hz = 44_100.0 / c.n_fft as f32;
        let semitone_hz = CHROMA_FMIN * (2f32.powf(1.0 / 12.0) - 1.0);
        assert!(semitone_hz > bin_hz, "{semitone_hz} vs {bin_hz}");
        // 48 kHz must land on the next size up rather than silently changing
        // the analysis duration.
        assert_eq!(Stft::for_onsets(48_000).n_fft, 4096);
    }

    #[test]
    fn stft_locates_a_sine_in_the_right_bin() {
        let sr = 44_100;
        let sig = testsig::sine(1000.0, 1.0, sr);
        let stft = Stft::for_onsets(sr);
        let spec = stft.magnitudes(&sig, sr);
        assert!(spec.frames > 100);
        let frame = spec.frame(spec.frames / 2);
        let peak = frame
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap()
            .0;
        assert!(
            (spec.bin_hz(peak) - 1000.0).abs() < 30.0,
            "peak at {} Hz",
            spec.bin_hz(peak)
        );
    }

    #[test]
    fn short_input_yields_no_frames() {
        let stft = Stft::for_onsets(44_100);
        let spec = stft.magnitudes(&[0.0; 100], 44_100);
        assert_eq!(spec.frames, 0);
        assert_eq!(onset_envelope(&spec).values.len(), 0);
    }

    #[test]
    fn onset_envelope_spikes_on_clicks_and_flatlines_on_a_sine() {
        let sr = 44_100;
        let stft = Stft::for_onsets(sr);

        let clicks = testsig::click_track(120.0, 8.0, sr);
        let env = onset_envelope(&stft.magnitudes(&clicks, sr));
        assert!(env.pulse_strength > 1.0, "pulse {}", env.pulse_strength);

        let steady = testsig::sine(440.0, 8.0, sr);
        let flat = onset_envelope(&stft.magnitudes(&steady, sr));
        assert!(
            flat.pulse_strength < env.pulse_strength / 3.0,
            "steady tone should read far less percussive: {} vs {}",
            flat.pulse_strength,
            env.pulse_strength
        );
    }

    #[test]
    fn autocorrelation_recovers_a_known_period() {
        let period = 37usize;
        let x: Vec<f32> = (0..2000)
            .map(|i| if i % period == 0 { 1.0 } else { 0.0 })
            .collect();
        let acf = autocorrelation(&x, 200);
        assert!((acf[0] - 1.0).abs() < 1e-5);
        let best = (10..200)
            .max_by(|a, b| acf[*a].partial_cmp(&acf[*b]).unwrap())
            .unwrap();
        assert_eq!(best, period);
    }

    #[test]
    fn interp_at_is_linear_and_clamps() {
        let x = [0.0, 2.0, 4.0];
        assert!((interp_at(&x, 0.5) - 1.0).abs() < 1e-6);
        assert!((interp_at(&x, 1.25) - 2.5).abs() < 1e-6);
        assert_eq!(interp_at(&x, -1.0), 0.0);
        assert_eq!(interp_at(&x, 99.0), 0.0);
    }

    #[test]
    fn smoothing_spreads_a_spike_and_preserves_area() {
        let mut x = vec![0.0f32; 21];
        x[10] = 1.0;
        let y = smooth(&x, 5);
        assert!(y[10] < 1.0 && y[10] > 0.0);
        assert!(y[9] > 0.0 && y[11] > 0.0);
        let area: f32 = y.iter().sum();
        assert!((area - 1.0).abs() < 1e-5, "area {area}");
        // An odd length is forced, and length 1 is the identity.
        assert_eq!(smooth(&x, 1), x);
        assert_eq!(smooth(&x, 4), smooth(&x, 5));
    }

    #[test]
    fn detrending_removes_a_slow_ramp() {
        let x: Vec<f32> = (0..1000).map(|i| i as f32 / 100.0).collect();
        let out = subtract_moving_average(&x, 201);
        // Interior points sit on the ramp's own local mean, so they cancel.
        assert!(out[500].abs() < 0.05, "{}", out[500]);
    }
}
