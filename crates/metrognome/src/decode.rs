//! In-memory audio decoding to mono `f32` PCM.
//!
//! `decode_bytes` sniffs the container and hands back [`Pcm`], which is all the
//! DSP layer ever sees. Audio is never written to disk: the bytes exist only as
//! long as the analysis takes.

use std::io::Cursor;

use symphonia::core::audio::{AudioBufferRef, Signal};
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

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

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions {
                enable_gapless: true,
                ..Default::default()
            },
            &MetadataOptions::default(),
        )
        .map_err(|e| Error::Decode(format!("probe: {e}")))?;

    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| Error::Decode("no decodable audio track".into()))?;
    let track_id = track.id;

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| Error::Decode(format!("codec: {e}")))?;

    let mut samples: Vec<f32> = Vec::new();
    let mut truncated = false;
    let mut sample_rate = track.codec_params.sample_rate.unwrap_or(0);
    let mut source_channels = track.codec_params.channels.map_or(0, |c| c.count() as u16);

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            // Symphonia signals clean end-of-stream as an UnexpectedEof io error.
            Err(symphonia::core::errors::Error::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break
            }
            Err(symphonia::core::errors::Error::ResetRequired) => {
                // Track list changed mid-stream; previews never do this, and
                // continuing would silently splice unrelated audio together.
                return Err(Error::Decode("stream reset required".into()));
            }
            Err(e) => return Err(Error::Decode(format!("demux: {e}"))),
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(buf) => {
                let spec = *buf.spec();
                if sample_rate == 0 {
                    sample_rate = spec.rate;
                }
                if source_channels == 0 {
                    source_channels = spec.channels.count() as u16;
                }
                append_mono(&buf, &mut samples);
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

/// Downmix any symphonia buffer variant into `out` as mono `f32`.
///
/// Averaging rather than summing keeps the result inside [-1, 1] regardless of
/// channel count, so downstream thresholds (silence, peak) stay meaningful.
fn append_mono(buf: &AudioBufferRef<'_>, out: &mut Vec<f32>) {
    macro_rules! mix {
        ($b:expr, $conv:expr) => {{
            let b = $b;
            let channels = b.spec().channels.count();
            let frames = b.frames();
            out.reserve(frames);
            if channels == 0 {
                return;
            }
            for i in 0..frames {
                let mut acc = 0.0f32;
                for ch in 0..channels {
                    #[allow(clippy::redundant_closure_call)]
                    {
                        acc += ($conv)(b.chan(ch)[i]);
                    }
                }
                // Float WAV can carry NaN and Inf. Zero is silence; a
                // non-finite sample is a poisoned analysis.
                let v = acc / channels as f32;
                out.push(if v.is_finite() { v } else { 0.0 });
            }
        }};
    }

    match buf {
        AudioBufferRef::F32(b) => mix!(b, |v: f32| v),
        AudioBufferRef::F64(b) => mix!(b, |v: f64| v as f32),
        AudioBufferRef::S32(b) => mix!(b, |v: i32| v as f32 / i32::MAX as f32),
        AudioBufferRef::S24(b) => {
            mix!(b, |v: symphonia::core::sample::i24| v.inner() as f32
                / 8_388_608.0)
        }
        AudioBufferRef::S16(b) => mix!(b, |v: i16| f32::from(v) / 32_768.0),
        AudioBufferRef::S8(b) => mix!(b, |v: i8| f32::from(v) / 128.0),
        AudioBufferRef::U32(b) => {
            mix!(b, |v: u32| (v as f64 / 2_147_483_648.0 - 1.0) as f32)
        }
        AudioBufferRef::U24(b) => {
            mix!(b, |v: symphonia::core::sample::u24| (v.inner() as f32
                / 8_388_608.0)
                - 1.0)
        }
        AudioBufferRef::U16(b) => mix!(b, |v: u16| (f32::from(v) / 32_768.0) - 1.0),
        AudioBufferRef::U8(b) => mix!(b, |v: u8| (f32::from(v) / 128.0) - 1.0),
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

    #[test]
    fn rejects_empty_and_garbage_input() {
        assert!(matches!(
            decode_bytes(Vec::new(), None),
            Err(Error::Decode(_))
        ));
        assert!(decode_bytes(vec![0u8; 512], Some("m4a")).is_err());
    }
}
