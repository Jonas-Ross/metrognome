//! metrognome's analysis behind a plain C ABI, for a WebAssembly host.
//!
//! No wasm-bindgen: the surface is four functions and a JSON string, and a raw
//! ABI keeps the build to `cargo build --target wasm32-unknown-unknown`. The
//! host decodes audio to mono PCM itself; nothing here knows it came from a
//! browser.
//!
//! Protocol: `mg_alloc` a buffer of `len` samples, write PCM into it, call
//! `mg_analyze`, read the byte count it returns of UTF-8 JSON at `mg_result_ptr`,
//! then `mg_free` the buffer.

use std::cell::RefCell;

use metrognome::{analyze_pcm_with, AnalysisOptions, Features, KeyProfile, ALGORITHM_VERSION};
use serde::Serialize;

/// What the host gets back from one analysis.
#[derive(Debug, Serialize)]
pub struct Report {
    /// Bumped when the DSP changes; a page can show which estimator it ran.
    pub algorithm_version: u32,
    /// The same features the CLI emits under `features`.
    pub features: Features,
}

/// Analyze mono PCM. `profile` is a name [`KeyProfile::parse`] accepts.
pub fn analyze(samples: &[f32], sample_rate: u32, profile: KeyProfile) -> Report {
    let options = AnalysisOptions {
        key_profile: profile,
        explain_key_scoring: false,
    };
    Report {
        algorithm_version: ALGORITHM_VERSION,
        features: analyze_pcm_with(samples, sample_rate, &options),
    }
}

thread_local! {
    static RESULT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// Allocate room for `len` samples and hand the host a pointer to fill.
#[no_mangle]
pub extern "C" fn mg_alloc(len: usize) -> *mut f32 {
    let mut buf = vec![0.0f32; len];
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

/// Release a buffer from [`mg_alloc`].
///
/// # Safety
/// `ptr` and `len` must be exactly what one earlier `mg_alloc` call returned
/// and was given, and the buffer must not be used afterwards.
#[no_mangle]
pub unsafe extern "C" fn mg_free(ptr: *mut f32, len: usize) {
    drop(Vec::from_raw_parts(ptr, len, len));
}

/// Analyze `len` samples at `ptr`; `krumhansl` non-zero picks that key profile
/// over the default. Returns the result's length in bytes.
///
/// # Safety
/// `ptr` must point to `len` initialized samples, as from [`mg_alloc`].
#[no_mangle]
pub unsafe extern "C" fn mg_analyze(
    ptr: *const f32,
    len: usize,
    sample_rate: u32,
    krumhansl: u32,
) -> usize {
    let samples = std::slice::from_raw_parts(ptr, len);
    let profile = if krumhansl != 0 {
        KeyProfile::Krumhansl
    } else {
        KeyProfile::Edm
    };
    let json = serde_json::to_vec(&analyze(samples, sample_rate, profile))
        .unwrap_or_else(|e| format!(r#"{{"error":"{e}"}}"#).into_bytes());
    RESULT.with(|r| {
        *r.borrow_mut() = json;
        r.borrow().len()
    })
}

/// Where the last [`mg_analyze`] result lives. Valid until the next call.
#[no_mangle]
pub extern "C" fn mg_result_ptr() -> *const u8 {
    RESULT.with(|r| r.borrow().as_ptr())
}

#[cfg(test)]
mod tests {
    use super::*;
    use metrognome::testsig::{self, Groove, Quality};

    #[test]
    fn a_report_carries_tempo_and_key() {
        let sr = 44_100;
        let mut sig = testsig::groove(124.0, 20.0, sr, Groove::FourOnFloor);
        testsig::mix_at(
            &mut sig,
            &testsig::chord_progression(5, Quality::Minor, 20.0, sr),
            0,
        );

        let report = analyze(&sig, sr, KeyProfile::Edm);
        assert!((report.features.tempo.as_ref().expect("tempo").bpm - 124.0).abs() < 1.0);
        assert_eq!(report.features.key.as_ref().expect("key").camelot, "4A");
    }

    #[test]
    fn the_abi_round_trips_through_json() {
        let sig = testsig::click_track(128.0, 10.0, 44_100);
        let ptr = mg_alloc(sig.len());
        unsafe {
            std::slice::from_raw_parts_mut(ptr, sig.len()).copy_from_slice(&sig);
            let n = mg_analyze(ptr, sig.len(), 44_100, 0);
            let bytes = std::slice::from_raw_parts(mg_result_ptr(), n);
            let v: serde_json::Value = serde_json::from_slice(bytes).unwrap();
            assert_eq!(v["algorithm_version"], ALGORITHM_VERSION);
            assert!(v["features"]["tempo"]["bpm"].is_number(), "{v}");
            mg_free(ptr, sig.len());
        }
    }

    #[test]
    fn silence_yields_no_confident_feature() {
        let report = analyze(&vec![0.0; 44_100 * 5], 44_100, KeyProfile::Edm);
        assert!(report.features.tempo.as_ref().is_none_or(|t| t.uncertain));
        assert!(report.features.key.as_ref().is_none_or(|k| k.uncertain));
    }
}
