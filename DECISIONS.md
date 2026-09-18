# Design decisions

Forks taken without asking, and the tradeoff behind each. Newest last.

## 1. Two crates in a workspace, not one crate with `src/bin`

A workspace member boundary is enforced by the compiler: `metrognome-cli` cannot
reach into library internals, and the library cannot accidentally depend on
`clap` or on the process's stdout. A single package with `src/lib.rs` plus
`src/main.rs` would give the same two crates but not the same discipline about
dependencies, and the dependency split is the thing worth enforcing — `anyhow`
and `clap` genuinely must not leak into the analysis path.

Cost: one more `Cargo.toml` and a `[workspace.dependencies]` table to keep
versions aligned.

## 2. Mono downmix by averaging, at the decoder boundary

Everything downstream is single-channel, so the downmix happens once, in
`decode`, rather than being a parameter every analysis function has to carry.
Averaging (not summing) keeps samples inside [-1, 1] for any channel count, so
the silence and peak thresholds mean the same thing regardless of source.

Cost: a hard-panned mono-incompatible mix loses material to phase cancellation.
Real releases are mono-compatible, and the alternative — analyzing channels
separately and merging estimates — doubles the DSP cost for a case that does not
occur in commercial music.

## 3. Corrupt packets are skipped, not fatal

`symphonia` reports a recoverable `DecodeError` for a damaged frame. A preview
is ~1300 frames; losing one is a rounding error against 30 seconds of audio,
while aborting turns a cosmetic glitch into a track with no features at all.
Demuxer-level errors and `ResetRequired` still fail hard, because those mean the
byte stream is not what we think it is.

## 4. No resampling; window sizes are derived from the sample rate

Analysis windows are specified in seconds and converted to the nearest power of
two at the actual sample rate. The alternative — resampling everything to a
canonical rate — needs a decent anti-aliasing filter to avoid polluting the
chromagram, which is more code and more CPU than just sizing the FFT correctly.

Cost: FFT sizes differ between a 44.1 kHz preview and a 48 kHz capture, so
frame-rate-dependent numbers must never be hardcoded in frames.
