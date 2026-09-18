//! BPM and musical-key estimation from short audio clips.
//!
//! The crate is layered so that analysis never assumes where audio came from:
//!
//! - [`decode`] turns encoded bytes into mono `f32` PCM.
//! - [`fetch`] is the only network path for audio, and it keeps bytes in memory.
//!
//! Analysis entry points take PCM samples plus a sample rate and nothing else,
//! so a live capture tap can feed them exactly as a downloaded preview does.

#![warn(missing_docs)]

pub mod decode;
pub mod error;
pub mod fetch;
pub mod testsig;

pub use decode::{decode_bytes, Pcm, PcmStats};
pub use error::{Error, ErrorKind, Result};

/// Bumped whenever a DSP change makes previously cached results incomparable.
///
/// The cache stores this alongside each row and treats a mismatch as a miss, so
/// an algorithm change never silently serves stale estimates.
pub const ALGORITHM_VERSION: u32 = 1;
