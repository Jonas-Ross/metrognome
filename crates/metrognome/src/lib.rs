//! BPM and musical-key estimation from short audio clips.
//!
//! Layered so analysis never assumes where audio came from: [`dsp`] and
//! [`tempo`] see only PCM and a sample rate, while `fetch`, `resolve` and
//! `ratelimit` hold all the iTunes-specific I/O behind the default `net`
//! feature. Without it the crate is pure DSP and builds for wasm32.
//! [`analyze_pcm`] is the samples-in, features-out entry point; [`types`] is
//! the JSON contract.

#![warn(missing_docs)]

pub mod analyze;
#[cfg(feature = "net")]
pub mod cache;
pub mod decode;
pub mod dsp;
pub mod error;
#[cfg(feature = "net")]
pub mod fetch;
pub mod key;
#[cfg(feature = "net")]
pub mod pipeline;
#[cfg(feature = "net")]
pub mod ratelimit;
#[cfg(feature = "net")]
pub mod resolve;
pub mod tempo;
pub mod testsig;
pub mod types;
pub mod validate;

pub use analyze::{analyze_pcm, analyze_pcm_with, AnalysisOptions};
pub use decode::{decode_bytes, Pcm, PcmStats};
pub use error::{Error, ErrorKind, Result};
pub use key::KeyProfile;
#[cfg(feature = "net")]
pub use pipeline::{Analyzer, AnalyzerConfig};
pub use types::{
    Alternate, Analysis, AudioInfo, Features, KeyEstimate, Maturity, Query, TempoEstimate,
    TrackMatch, SCHEMA_VERSION,
};

/// Bumped whenever a DSP change makes previously cached results incomparable.
///
/// The cache stores this alongside each row and treats a mismatch as a miss, so
/// an algorithm change never silently serves stale estimates.
pub const ALGORITHM_VERSION: u32 = 12;
