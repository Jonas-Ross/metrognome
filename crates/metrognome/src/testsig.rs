//! Synthetic signal generators.
//!
//! Every tempo and key test builds its own input here, so the DSP is testable
//! with no network and no audio files in the repo. `selftest` runs these too.

use std::f32::consts::TAU;

/// Deterministic white-noise source.
///
/// A seeded xorshift keeps percussive test signals byte-identical between runs
/// — a flaky DSP test is worse than no test.
pub struct Noise(u32);

impl Noise {
    /// New generator with the given non-zero seed.
    pub fn new(seed: u32) -> Self {
        Noise(if seed == 0 { 0x2545_f491 } else { seed })
    }

    /// Next sample in [-1, 1).
    pub fn next_sample(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

/// A pure sine at `freq` Hz.
pub fn sine(freq: f32, secs: f32, sample_rate: u32) -> Vec<f32> {
    let n = (secs * sample_rate as f32) as usize;
    let w = TAU * freq / sample_rate as f32;
    (0..n).map(|i| 0.5 * (w * i as f32).sin()).collect()
}

/// Midi note number to frequency in Hz (A4 = 440 Hz = note 69).
pub fn midi_to_hz(note: f32) -> f32 {
    440.0 * 2f32.powf((note - 69.0) / 12.0)
}

/// Additive tone with a few harmonics, so chroma sees realistic partials
/// rather than a single bin.
fn tone(freq: f32, secs: f32, sample_rate: u32, amp: f32) -> Vec<f32> {
    let n = (secs * sample_rate as f32) as usize;
    let mut out = vec![0.0f32; n];
    // 1/k falloff over four partials approximates a soft synth/organ timbre;
    // more partials would start smearing energy into neighbouring pitch classes.
    for k in 1..=4 {
        let f = freq * k as f32;
        if f > sample_rate as f32 / 2.0 {
            break;
        }
        let w = TAU * f / sample_rate as f32;
        let a = amp / k as f32;
        for (i, s) in out.iter_mut().enumerate() {
            *s += a * (w * i as f32).sin();
        }
    }
    out
}

/// Mix `src` into `dst` starting at sample `at`, clipping at the end of `dst`.
pub fn mix_at(dst: &mut [f32], src: &[f32], at: usize) {
    for (i, v) in src.iter().enumerate() {
        match dst.get_mut(at + i) {
            Some(d) => *d += v,
            None => break,
        }
    }
}

/// Short exponentially decaying noise burst — a hi-hat-ish transient.
pub fn noise_burst(secs: f32, sample_rate: u32, amp: f32, noise: &mut Noise) -> Vec<f32> {
    let n = (secs * sample_rate as f32) as usize;
    (0..n)
        .map(|i| {
            let env = (-8.0 * i as f32 / n.max(1) as f32).exp();
            noise.next_sample() * env * amp
        })
        .collect()
}

/// Decaying pitched sine — a tom, or the tonal part of a snare.
pub fn drum_hit(freq: f32, secs: f32, sample_rate: u32, amp: f32, decay: f32) -> Vec<f32> {
    let n = (secs * sample_rate as f32) as usize;
    let w = TAU * freq / sample_rate as f32;
    (0..n)
        .map(|i| {
            let t = i as f32 / n.max(1) as f32;
            amp * (-decay * t).exp() * (w * i as f32).sin()
        })
        .collect()
}

/// A kick drum: broadband click, then a body whose pitch sweeps down to `freq`.
///
/// The sweep is not decoration: a fixed-pitch 55 Hz sine spells a clean
/// harmonic series on A, so a drums-only signal built from fixed-pitch hits
/// would appear to have a key. Real kicks sweep, which is why they read as
/// percussion rather than as a bass note.
pub fn kick_hit(freq: f32, secs: f32, sample_rate: u32, amp: f32, noise: &mut Noise) -> Vec<f32> {
    let n = (secs * sample_rate as f32) as usize;
    let sr = sample_rate as f32;
    let mut out = Vec::with_capacity(n);
    let mut phase = 0.0f32;
    for i in 0..n {
        let t = i as f32 / sr;
        // Starts an octave and a half up and settles within ~25 ms.
        let f = freq * (1.0 + 1.5 * (-40.0 * t).exp());
        phase += TAU * f / sr;
        let env = (-9.0 * i as f32 / n.max(1) as f32).exp();
        out.push(amp * env * phase.sin());
    }
    mix_at(
        &mut out,
        &noise_burst(0.006, sample_rate, amp * 0.5, noise),
        0,
    );
    out
}

/// A bare click track: one transient per beat at exactly `bpm`.
///
/// This is the tempo estimator's ground truth — if it cannot recover the tempo
/// from unambiguous impulses, nothing further is worth debugging.
pub fn click_track(bpm: f32, secs: f32, sample_rate: u32) -> Vec<f32> {
    let n = (secs * sample_rate as f32) as usize;
    let mut out = vec![0.0f32; n];
    let mut noise = Noise::new(0x1234_5678);
    let period = 60.0 / bpm * sample_rate as f32;
    let click = noise_burst(0.01, sample_rate, 0.9, &mut noise);
    let mut beat = 0usize;
    loop {
        let at = (beat as f32 * period).round() as usize;
        if at >= n {
            break;
        }
        mix_at(&mut out, &click, at);
        beat += 1;
    }
    out
}

/// Which rhythmic idiom to synthesize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Groove {
    /// Four-on-the-floor kick, offbeat hats, backbeat clap. The octave trap is
    /// the offbeat hat layer, which reads as double tempo.
    FourOnFloor,
    /// Half-time-feeling breakbeat: kick on 1 and the "and" of 3, snare on 2
    /// and 4. The octave trap is the snare period, which reads as half tempo —
    /// this is exactly the 87-vs-174 drum & bass failure.
    Breakbeat,
    /// Four-on-the-floor with an offbeat stab as loud as the kick. The trap is
    /// a grid at 2/3 of the true tempo: period 1.5 beats, so it alternates kick
    /// and stab, hitting something strong every time while explaining two
    /// thirds of the pattern.
    OffbeatTrance,
}

/// Synthesize a drum pattern at `bpm` with the given groove.
///
/// Unlike [`click_track`] this deliberately contains sub- and super-beat
/// periodicities, so it exercises octave resolution rather than peak-picking.
pub fn groove(bpm: f32, secs: f32, sample_rate: u32, groove: Groove) -> Vec<f32> {
    let n = (secs * sample_rate as f32) as usize;
    let mut out = vec![0.0f32; n];
    let mut noise = Noise::new(0xdead_beef);
    let sr = sample_rate as f32;
    let beat = 60.0 / bpm * sr;

    // The click in the kick matters for more than realism: onset detection
    // works on spectral flux, and a body-only kick puts all its energy in one
    // or two mel bands, where a broadband hi-hat would out-flux it and invert
    // the pattern's accent structure.
    let kick = kick_hit(55.0, 0.18, sample_rate, 0.95, &mut noise);
    let snare = {
        let mut s = noise_burst(0.12, sample_rate, 0.55, &mut noise);
        mix_at(&mut s, &drum_hit(190.0, 0.08, sample_rate, 0.18, 12.0), 0);
        s
    };
    // Closed hats sit far below kick and snare on a real drum bus — roughly
    // -18 dBFS against -6. Getting that balance right matters for octave tests:
    // hats that are too loud make a wrong grid that lands on them score as well
    // as the right grid that lands on kicks.
    let hat = noise_burst(0.035, sample_rate, 0.10, &mut noise);
    // An offbeat stab: a short tonal hit with a broadband edge, loud enough to
    // rival the kick. Used only by OffbeatTrance.
    let stab = {
        let mut s = drum_hit(320.0, 0.10, sample_rate, 0.60, 9.0);
        mix_at(&mut s, &noise_burst(0.02, sample_rate, 0.35, &mut noise), 0);
        s
    };

    let bars = (n as f32 / (beat * 4.0)).ceil() as usize;
    for bar in 0..bars {
        let bar0 = bar as f32 * beat * 4.0;
        let at = |b: f32| (bar0 + b * beat).round() as usize;
        match groove {
            Groove::FourOnFloor => {
                for b in 0..4 {
                    mix_at(&mut out, &kick, at(b as f32));
                    mix_at(&mut out, &hat, at(b as f32 + 0.5));
                }
                mix_at(&mut out, &snare, at(1.0));
                mix_at(&mut out, &snare, at(3.0));
            }
            Groove::OffbeatTrance => {
                for b in 0..4 {
                    mix_at(&mut out, &kick, at(b as f32));
                    // Deliberately kick-weight, not hat-weight: the whole point
                    // is an offbeat a sparse grid is happy to land on.
                    mix_at(&mut out, &stab, at(b as f32 + 0.5));
                }
            }
            Groove::Breakbeat => {
                mix_at(&mut out, &kick, at(0.0));
                mix_at(&mut out, &kick, at(2.5));
                mix_at(&mut out, &snare, at(1.0));
                mix_at(&mut out, &snare, at(3.0));
                // Sixteenth hats: the densest layer, four times the beat rate.
                for i in 0..16 {
                    mix_at(&mut out, &hat, at(i as f32 * 0.25));
                }
            }
        }
    }
    out
}

/// Triad quality for [`chord_progression`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    /// Major triad.
    Major,
    /// Minor triad.
    Minor,
}

/// Sustained triads spelling a diatonic progression in `root_pc`.
///
/// Key detection is tested on this rather than a single chord: one triad only
/// pins down three pitch classes, which several keys share.
pub fn chord_progression(root_pc: u8, quality: Quality, secs: f32, sample_rate: u32) -> Vec<f32> {
    // Scale degrees of a I-IV-V-I (major) / i-VI-VII-i (minor) cycle: enough
    // distinct pitch classes to disambiguate relative major/minor pairs.
    let (steps, triads): (&[i32], &[Quality]) = match quality {
        Quality::Major => (
            &[0, 5, 7, 0],
            &[
                Quality::Major,
                Quality::Major,
                Quality::Major,
                Quality::Major,
            ],
        ),
        Quality::Minor => (
            &[0, 8, 10, 0],
            &[
                Quality::Minor,
                Quality::Major,
                Quality::Major,
                Quality::Minor,
            ],
        ),
    };

    let n = (secs * sample_rate as f32) as usize;
    let mut out = vec![0.0f32; n];
    let chord_secs = secs / steps.len() as f32;
    for (i, (&step, &q)) in steps.iter().zip(triads).enumerate() {
        // Root at C3-ish keeps every partial comfortably inside the analysis
        // band (65-2100 Hz) at any transposition.
        let root = 48.0 + f32::from(root_pc) + step as f32;
        let third = if q == Quality::Major { 4.0 } else { 3.0 };
        let notes = [root, root + third, root + 7.0, root + 12.0];
        let at = (i as f32 * chord_secs * sample_rate as f32) as usize;
        for note in notes {
            let t = tone(midi_to_hz(note), chord_secs, sample_rate, 0.22);
            mix_at(&mut out, &t, at);
        }
    }
    out
}

/// Encode mono or interleaved samples as a 16-bit PCM WAV file.
///
/// Used only to feed the decoder real container bytes in tests; nothing in the
/// production path writes audio anywhere.
pub fn wav_bytes(samples: &[f32], sample_rate: u32, channels: u16) -> Vec<u8> {
    let bits = 16u16;
    let block_align = channels * bits / 8;
    let byte_rate = sample_rate * u32::from(block_align);
    let data_len = (samples.len() * 2) as u32;

    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32_767.0).round() as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noise_is_deterministic() {
        let a: Vec<f32> = (0..8).map(|_| Noise::new(9).next_sample()).collect();
        let mut n = Noise::new(9);
        let b: Vec<f32> = (0..8).map(|_| n.next_sample()).collect();
        assert_eq!(a[0], b[0]);
        assert!(
            b.iter().any(|&v| v != b[0]),
            "generator must not be constant"
        );
    }

    #[test]
    fn click_track_has_expected_beat_count() {
        let sr = 22_050;
        let sig = click_track(120.0, 4.0, sr);
        assert_eq!(sig.len(), 4 * sr as usize);
        // 120 BPM over 4 s is 8 beats; count transients above half peak.
        let peak = sig.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        // A burst is broadband noise, so it crosses zero many times; count
        // onsets with a refractory period rather than a level crossing.
        let refractory = sr as usize / 10;
        let mut hits = 0;
        let mut next_allowed = 0usize;
        for (i, &v) in sig.iter().enumerate() {
            if i >= next_allowed && v.abs() > peak * 0.5 {
                hits += 1;
                next_allowed = i + refractory;
            }
        }
        assert_eq!(hits, 8, "expected 8 beats");
    }
}
