//! BPM and musical-key estimation from short audio clips.
//!
//! Layered so analysis never assumes where audio came from: [`dsp`] and
//! [`tempo`] see only PCM and a sample rate, while [`fetch`], [`resolve`] and
//! [`ratelimit`] hold all the iTunes-specific I/O. [`analyze_pcm`] is the
//! samples-in, features-out entry point; [`types`] is the JSON contract.

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
    Alternate, Analysis, AudioInfo, Features, KeyEstimate, Maturity, Query, TempoEstimate,
    TrackMatch, SCHEMA_VERSION,
};

/// Bumped whenever a DSP change makes previously cached results incomparable.
///
/// The cache stores this alongside each row and treats a mismatch as a miss, so
/// an algorithm change never silently serves stale estimates.
pub const ALGORITHM_VERSION: u32 = 9;
