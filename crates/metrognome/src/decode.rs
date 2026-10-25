//! In-memory audio decoding to mono `f32` PCM.
//!
//! `decode_bytes` sniffs the container and hands back [`Pcm`], which is all the
//! DSP layer ever sees. Audio is never written to disk: the bytes exist only as
//! long as the analysis takes.

use std::io::Cursor;

use symphonia::core::audio::GenericAudioBufferRef;
use symphonia::core::codecs::audio::well_known::CODEC_ID_MP3;
use symphonia::core::codecs::audio::{AudioDecoderOptions, CODEC_ID_NULL_AUDIO};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

use crate::error::{Error, Result};

/// Mono PCM at a known sample rate.
///
/// Analysis entry points take this, or a bare `&[f32]` plus rate; nothing here
/// knows where the samples came from.
#[derive(Debug, Clone)]
pub struct Pcm {
    /// Interleaving is not a concern: channels are already downmixed to mono.
    pub samples: Vec<f32>,
    /// Whether decoding stopped at [`MAX_DECODE_SECS`] rather than at the end.
    pub truncated: bool,
    /// Sample rate in Hz, as reported by the decoder.
    pub sample_rate: u32,
    /// Channel count of the *source*, retained for diagnostics only.
    pub source_channels: u16,
}

impl Pcm {
    /// Duration in seconds.
    pub fn duration_secs(&self) -> f64 {
        if self.sample_rate == 0 {
            return 0.0;
        }
        self.samples.len() as f64 / f64::from(self.sample_rate)
    }

    /// Cheap summary used by `--stats` and by confidence scoring.
    pub fn stats(&self) -> PcmStats {
        let n = self.samples.len();
        let mut peak = 0.0f32;
        let mut sum_sq = 0.0f64;
        let mut silent = 0usize;
        for &s in &self.samples {
            let a = s.abs();
            if a > peak {
                peak = a;
            }
            sum_sq += f64::from(s) * f64::from(s);
            // -60 dBFS: below this a sample carries no usable onset information.
            if a < 0.001 {
                silent += 1;
            }
        }
        let rms = if n == 0 {
            0.0
        } else {
            (sum_sq / n as f64).sqrt()
        };
        PcmStats {
            sample_rate: self.sample_rate,
            source_channels: self.source_channels,
            frames: n,
            duration_secs: self.duration_secs(),
            peak,
            rms,
            silent_fraction: if n == 0 {
                1.0
            } else {
                silent as f64 / n as f64
            },
        }
    }
}

/// Summary statistics over decoded PCM.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct PcmStats {
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Channel count before the mono downmix.
    pub source_channels: u16,
    /// Mono frame count.
    pub frames: usize,
    /// Duration in seconds.
    pub duration_secs: f64,
    /// Largest absolute sample value.
    pub peak: f32,
    /// Root mean square level.
    pub rms: f64,
    /// Fraction of samples below -60 dBFS.
    pub silent_fraction: f64,
}

/// Decode an encoded audio buffer to mono `f32` PCM.
///
/// `extension_hint` is an optional container hint ("m4a", "mp3", …). Symphonia
/// probes regardless; the hint only shortens the search.
/// Longest audio this will decode, in seconds.
///
/// The fetch limit bounds encoded bytes, which says almost nothing about
/// duration: 8 MB of WAV is nine minutes and costs 32 s of analysis, and the
/// same 8 MB of low-bitrate AAC is hours. Four times the longest preview leaves
/// room for a live-capture buffer without letting one row stall a batch.
/// Truncating beats erroring: a long file still gets an answer for its opening.
pub const MAX_DECODE_SECS: f64 = 120.0;

/// Decode an encoded audio buffer to mono `f32` PCM.
///
/// `extension_hint` is an optional container hint ("m4a", "mp3", …). Symphonia
/// probes regardless; the hint only shortens the search. Stops at
/// [`MAX_DECODE_SECS`], flagging [`Pcm::truncated`].
pub fn decode_bytes(bytes: Vec<u8>, extension_hint: Option<&str>) -> Result<Pcm> {
    if bytes.is_empty() {
        return Err(Error::Decode("empty input".into()));
    }

    let mss = MediaSourceStream::new(Box::new(Cursor::new(bytes)), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = extension_hint {
        hint.with_extension(ext);
    }

    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| Error::Decode(format!("probe: {e}")))?;

    let (track_id, params) = format
        .tracks()
        .iter()
        .find_map(|t| match &t.codec_params {
            Some(CodecParameters::Audio(p)) if p.codec != CODEC_ID_NULL_AUDIO => Some((t.id, p)),
            _ => None,
        })
        .ok_or_else(|| Error::Decode("no decodable audio track".into()))?;

    // Trims MP3 encoder delay; isomp4 ignores edit lists, so AAC priming stays.
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default().gapless(true))
        .map_err(|e| Error::Decode(format!("codec: {e}")))?;

    let mut samples: Vec<f32> = Vec::new();
    let mut planes: Vec<Vec<f32>> = Vec::new();
    // symphonia 0.5 clamped MP3 synthesis and 0.6 does not; the key weights
    // were fitted on clamped MP3s, and AAC was never clamped.
    let clamp = params.codec == CODEC_ID_MP3;
    let mut truncated = false;
    let mut sample_rate = params.sample_rate.unwrap_or(0);
    let mut source_channels = params.channels.as_ref().map_or(0, |c| c.count() as u16);

    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            // A stream cut short keeps what decoded before the cut.
            Err(symphonia::core::errors::Error::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                tracing::warn!("stream ended mid-packet; analyzing what decoded");
                break;
            }
            Err(symphonia::core::errors::Error::ResetRequired) => {
                // Track list changed mid-stream; previews never do this, and
                // continuing would silently splice unrelated audio together.
                return Err(Error::Decode("stream reset required".into()));
            }
            Err(e) => return Err(Error::Decode(format!("demux: {e}"))),
        };
        if packet.track_id != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(buf) => {
                let spec = buf.spec();
                if sample_rate == 0 {
                    sample_rate = spec.rate();
                }
                if source_channels == 0 {
                    source_channels = spec.channels().count() as u16;
                }
                append_mono(&buf, clamp, &mut planes, &mut samples);
                if sample_rate > 0 {
                    let cap = (MAX_DECODE_SECS * f64::from(sample_rate)) as usize;
                    if samples.len() >= cap {
                        samples.truncate(cap);
                        truncated = true;
                        break;
                    }
                }
            }
            // A corrupt packet inside a preview is recoverable: the frames it
            // would have contributed are a rounding error against 30 seconds,
            // and bailing out would turn a cosmetic glitch into a hard failure.
            Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
            Err(e) => return Err(Error::Decode(format!("decode: {e}"))),
        }
    }

    if sample_rate == 0 {
        return Err(Error::Decode("decoder reported no sample rate".into()));
    }
    if samples.is_empty() {
        return Err(Error::UnusableAudio("decoded zero frames".into()));
    }

    if truncated {
        tracing::warn!(
            limit_secs = MAX_DECODE_SECS,
            "audio truncated: longer than the decode limit"
        );
    }

    Ok(Pcm {
        samples,
        truncated,
        sample_rate,
        source_channels: source_channels.max(1),
    })
}

/// Downmix a decoded buffer of any sample format into `out` as mono `f32`.
///
/// Averaging rather than summing keeps the result inside [-1, 1] regardless of
/// channel count, so downstream thresholds (silence, peak) stay meaningful.
/// `planes` is scratch space reused across packets.
fn append_mono(
    buf: &GenericAudioBufferRef<'_>,
    clamp: bool,
    planes: &mut Vec<Vec<f32>>,
    out: &mut Vec<f32>,
) {
    buf.copy_to_vecs_planar(planes);
    if clamp {
        for plane in planes.iter_mut() {
            plane.iter_mut().for_each(|v| *v = v.clamp(-1.0, 1.0));
        }
    }
    let Some((first, rest)) = planes.split_first() else {
        return;
    };
    let start = out.len();
    out.extend_from_slice(first);
    let mixed = &mut out[start..];
    for plane in rest.iter() {
        mixed.iter_mut().zip(plane).for_each(|(m, v)| *m += v);
    }
    let channels = 1 + rest.len() as u32;
    for m in mixed {
        // Float WAV can carry NaN and Inf. Zero is silence; a
        // non-finite sample is a poisoned analysis.
        let v = *m / channels as f32;
        *m = if v.is_finite() { v } else { 0.0 };
    }
}

#[cfg(test)]
mod cap_tests {
    use super::*;

    #[test]
    fn a_long_file_is_truncated_rather_than_analyzed_whole() {
        // The fetch limit bounds encoded bytes, not duration: 8 MB of WAV is
        // minutes of audio and was costing tens of seconds of analysis.
        let sr = 8_000;
        let secs = MAX_DECODE_SECS + 30.0;
        let wav = crate::testsig::wav_bytes(&vec![0.1f32; (sr as f64 * secs) as usize], sr, 1);
        let pcm = decode_bytes(wav, Some("wav")).expect("decode");
        assert!(pcm.truncated, "should have stopped at the limit");
        assert!(
            (pcm.duration_secs() - MAX_DECODE_SECS).abs() < 0.01,
            "got {}",
            pcm.duration_secs()
        );
    }

    #[test]
    fn a_preview_length_clip_is_not_truncated() {
        let sr = 8_000;
        let wav = crate::testsig::wav_bytes(&vec![0.1f32; sr as usize * 30], sr, 1);
        let pcm = decode_bytes(wav, Some("wav")).expect("decode");
        assert!(!pcm.truncated);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testsig;

    #[test]
    fn decodes_wav_round_trip() {
        let src = testsig::sine(440.0, 1.0, 44_100);
        let wav = testsig::wav_bytes(&src, 44_100, 1);
        let pcm = decode_bytes(wav, Some("wav")).expect("decode");
        assert_eq!(pcm.sample_rate, 44_100);
        assert_eq!(pcm.source_channels, 1);
        // 16-bit quantization is the only loss, so lengths must match exactly.
        assert_eq!(pcm.samples.len(), src.len());
        let stats = pcm.stats();
        assert!((stats.duration_secs - 1.0).abs() < 1e-6);
        assert!(stats.peak > 0.4 && stats.peak <= 1.0);
        assert!(stats.rms > 0.2);
    }

    #[test]
    fn downmixes_stereo_to_mono() {
        // Hard-panned opposite phase: a correct average cancels to silence.
        let left = testsig::sine(440.0, 0.5, 44_100);
        let right: Vec<f32> = left.iter().map(|v| -v).collect();
        let mut interleaved = Vec::with_capacity(left.len() * 2);
        for i in 0..left.len() {
            interleaved.push(left[i]);
            interleaved.push(right[i]);
        }
        let wav = testsig::wav_bytes(&interleaved, 44_100, 2);
        let pcm = decode_bytes(wav, Some("wav")).expect("decode");
        assert_eq!(pcm.source_channels, 2);
        assert_eq!(pcm.samples.len(), left.len());
        assert!(
            pcm.stats().peak < 0.01,
            "expected cancellation to near-zero"
        );
    }

    /// A WAV of `frames` copies of one little-endian sample, in any PCM layout.
    fn wav_of(format_tag: u16, bits: u16, sample: &[u8], frames: usize) -> Vec<u8> {
        let data_len = (sample.len() * frames) as u32;
        let block = bits / 8;
        let mut w = Vec::new();
        w.extend_from_slice(b"RIFF");
        w.extend_from_slice(&(36 + data_len).to_le_bytes());
        w.extend_from_slice(b"WAVEfmt ");
        w.extend_from_slice(&16u32.to_le_bytes());
        w.extend_from_slice(&format_tag.to_le_bytes());
        w.extend_from_slice(&1u16.to_le_bytes());
        w.extend_from_slice(&8_000u32.to_le_bytes());
        w.extend_from_slice(&(8_000 * u32::from(block)).to_le_bytes());
        w.extend_from_slice(&block.to_le_bytes());
        w.extend_from_slice(&bits.to_le_bytes());
        w.extend_from_slice(b"data");
        w.extend_from_slice(&data_len.to_le_bytes());
        for _ in 0..frames {
            w.extend_from_slice(sample);
        }
        w
    }

    #[test]
    fn every_pcm_sample_format_scales_to_the_same_level() {
        // Half scale in each layout the downmix used to convert by hand.
        let cases: [(&str, u16, u16, Vec<u8>); 5] = [
            ("u8", 1, 8, vec![192]),
            ("s16", 1, 16, 16_384i16.to_le_bytes().to_vec()),
            ("s24", 1, 24, 4_194_304i32.to_le_bytes()[..3].to_vec()),
            ("s32", 1, 32, (1i32 << 30).to_le_bytes().to_vec()),
            ("f32", 3, 32, 0.5f32.to_le_bytes().to_vec()),
        ];
        for (name, tag, bits, sample) in cases {
            let pcm = decode_bytes(wav_of(tag, bits, &sample, 800), Some("wav"))
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(pcm.samples.len(), 800, "{name}");
            assert!(
                pcm.samples.iter().all(|&v| (v - 0.5).abs() < 1e-6),
                "{name}: got {}",
                pcm.samples[0]
            );
        }
    }

    const HOT_MP3: &[u8] = include_bytes!("../tests/fixtures/hot_noise.mp3");

    #[test]
    fn mp3_output_is_clamped_like_symphonia_0_5() {
        // White noise boosted 10 dB past full scale, so synthesis overshoots.
        let pcm = decode_bytes(HOT_MP3.to_vec(), Some("mp3")).expect("decode");
        assert!(pcm.duration_secs() > 0.9, "got {}", pcm.duration_secs());
        assert!(pcm.stats().peak <= 1.0, "peak {}", pcm.stats().peak);
    }

    #[test]
    fn an_mp3_behind_a_large_id3_tag_still_probes() {
        // Embedded cover art routinely passes the probe's 1 MB scan depth.
        let pad = 1_500_000u32;
        let mut bytes = b"ID3\x03\x00\x00".to_vec();
        bytes.extend((0..4).rev().map(|i| ((pad >> (7 * i)) & 0x7f) as u8));
        bytes.resize(bytes.len() + pad as usize, 0);
        bytes.extend_from_slice(HOT_MP3);
        let pcm = decode_bytes(bytes, Some("mp3")).expect("decode");
        assert!(pcm.duration_secs() > 0.9, "got {}", pcm.duration_secs());
    }

    #[test]
    fn rejects_empty_and_garbage_input() {
        assert!(matches!(
            decode_bytes(Vec::new(), None),
            Err(Error::Decode(_))
        ));
        assert!(decode_bytes(vec![0u8; 512], Some("m4a")).is_err());
    }
}
