//! BPM and musical-key estimation from short audio clips.
//!
//! The crate is layered so that analysis never assumes where audio came from:
//!
//! - [`decode`] turns encoded bytes into mono `f32` PCM.
//! - [`dsp`] and [`tempo`] take PCM (or an envelope derived from it) and a
//!   sample rate, and know nothing else about the world.
//! - [`fetch`] is the only network path for audio, and it keeps bytes in memory.
//! - [`resolve`] and [`ratelimit`] are the iTunes-specific I/O, kept to one
//!   side so the analysis path can be driven without them.
//! - [`types`] is the versioned JSON contract with consumers.
//!
//! [`analyze_pcm`] is the entry point for "samples in, features out".
//!
//! Analysis entry points take PCM samples plus a sample rate and nothing else,
//! so a live capture tap can feed them exactly as a downloaded preview does.

#![warn(missing_docs)]

pub mod cache;
pub mod decode;
pub mod dsp;
pub mod error;
pub mod fetch;
pub mod key;
pub mod pipeline;
pub mod ratelimit;
pub mod resolve;
pub mod tempo;
pub mod testsig;
pub mod types;
pub mod validate;

pub use decode::{decode_bytes, Pcm, PcmStats};
pub use error::{Error, ErrorKind, Result};
pub use key::KeyProfile;
pub use pipeline::{analyze_pcm, analyze_pcm_with, AnalysisOptions, Analyzer, AnalyzerConfig};
pub use types::{
    Alternate, Analysis, AudioInfo, Features, KeyEstimate, Query, TempoEstimate, TrackMatch,
    SCHEMA_VERSION,
};

/// Bumped whenever a DSP change makes previously cached results incomparable.
///
/// The cache stores this alongside each row and treats a mismatch as a miss, so
/// an algorithm change never silently serves stale estimates.
pub const ALGORITHM_VERSION: u32 = 5;
