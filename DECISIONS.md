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

## 5. Tempo is reported in a canonical one-octave window (90-180 BPM)

Octave errors are the dominant failure mode in tempo estimation, and the usual
mitigation — a prior that nudges toward 120 — still leaves a drum & bass track
free to come back as 87. Instead every candidate is folded by doubling or
halving until it lands in [90, 180), which holds house (120-128), techno and
trance (130-145) and drum & bass (170-176) in a single octave. An octave error
then cannot be *expressed*, only a metric one.

Cost: genuinely slow music is reported at double time — an 85 BPM hip-hop track
reads as 170. This is the right trade for a library of dance music, and both the
half and the double are always present in `alternates` so a consumer can
override. The window is a named constant, not a scattered literal, because a
different library might want a different octave.

## 6. Scoring: phase-aligned comb filter with a consistency penalty, not raw ACF

Autocorrelation is used only to *propose* candidates. Ranking them is done by
laying a beat grid over the onset envelope and measuring `mean - stddev` of the
onset strength at the grid points.

Two things drove this. First, the mean alone is not enough: a grid at 2/3 or 4/5
of the true tempo can land on *something* every time — a hi-hat rather than a
kick — and score a competitive mean. Penalizing spread asks for beats that are
consistently strong, which is what a real tempo gives you. Second, the ACF's own
peak heights are not comparable across the octave relationships we care about.

Rejected along the way: adding a bar-level term (autocorrelation at four beats)
on the theory that a real tempo has a real bar. Measured on synthetic grooves it
fired just as happily on metric decoys, because busy 16th percussion is periodic
at almost any subdivision. It made one case better and another worse, so it is
not in.

## 7. Two-stage tempo search: smoothed to find, unsmoothed to judge

The comb score against a raw onset envelope is a knife edge — onsets are one or
two frames wide, so a 0.03% tempo error already walks the grid off them. No
practical search step finds that peak. The search therefore runs against an
envelope smoothed with a 30 ms Hann kernel, which widens the peak into something
a 0.1% grid can locate.

But smoothing also lifts a wrong grid toward a right one, because it lets quiet
events bleed into slots where loud ones should be — precisely the distinction
that separates a true tempo from a 2/3 decoy. So the finalists are rescored on
the unsmoothed envelope, and the reported BPM comes from a final precision pass
there too. Measured on synthetic grooves, this restores a 8-37% margin between
the true tempo and its best decoy, where scoring on the smoothed envelope left
around 1%.

## 8. Confidence is a product, not an average

Three factors — how far above background the beat grid sits, how much it beats
the best *unrelated* reading, and how periodic the envelope is at that rate at
all. They multiply, so any one of them can veto. Averaging would let a strong
comb score on a beatless intro (where the flux is noise and some alignment
always looks good) report high confidence, which is the single most damaging
thing this tool could do to a consumer that trusts it.
