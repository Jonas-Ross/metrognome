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

## 9. Matching is a pure function over parsed results

`pick_best` takes an artist, a title and a slice of already-parsed candidates.
All the judgement lives there, and the HTTP client only supplies the slice. That
is what makes the interesting cases — a neutral qualifier, a remix competing
with its original, a title collision between two different acts — testable from
fixture JSON with no network in the suite.

The fixtures are hand-authored against the documented response schema rather
than captured, because the environment this was built in has no route to
`itunes.apple.com`. Field names and types match; replacing them with real
captures should need no code change.

## 10. A remix is not the track you asked for

Parenthesized qualifiers are split off the title and scored separately, with a
short list of *neutral* ones ("radio edit", "remastered", "album version", a
bare year) stripped because they name a different master of the same
performance. Everything else — "(Michael Woods Remix)", "(Live)" — counts
against the match.

The reason is not tidiness: a remix has its own tempo and often its own key, so
matching one to the other writes wrong feature data into the consumer's library,
which is worse than writing none. A match below 0.85 is flagged `uncertain`, and
the matched track's own metadata always comes back so a bad match is visible
rather than inferred.

## 11. Rate limiting is a token bucket with a small burst, not a fixed delay

Apple documents no number; ~20 requests/minute is the widely reported ceiling,
and exceeding it gets the address throttled — slower than being polite. The
limiter defaults to 18/minute with a burst of 5, so a handful of tracks start
immediately instead of being spaced from a standing start, while the refill rate
still bounds the long-run average.

The token arithmetic is a pure function over an explicit `now`, so the queueing
behaviour is unit-tested without sleeping. The bucket is allowed to go negative
and the debt becomes the wait, which is what makes a queue of concurrent callers
come out evenly spaced rather than all waking to race for one token.

## 12. Failures are results, not errors

`Analyzer::analyze` never returns `Err`. A failure is an `Analysis` with
`status: "error"`, the stable `ErrorKind` discriminator, and whatever was
learned before things went wrong — including the resolved track, so a caller can
see *which* track failed to decode. `batch` depends on this: one bad row must
not take the run with it, and a dropped row is worse than a reported failure
because the caller cannot tell it happened.

## 13. Chroma is averaged per pitch class, not summed

A semitone spans about one FFT bin at A2 and dozens at A7, so the number of bins
landing on each pitch class is wildly uneven. Summing raw magnitudes therefore
gives *white noise* a fixed, lopsided chroma that correlates around 0.7 with
some key — a drums-only preview came back as a confident A minor. Averaging
within each pitch class makes noise flat, which is what lets an untonal clip
report no key instead of a confident wrong one.

This is why the chroma window is 185 ms rather than the onset window's 46 ms,
and why the chromagram starts at A2 (110 Hz): that is the lowest pitch where one
semitone is wider than one bin, so below it adjacent notes are not separable
however the bins are weighted.

## 14. EDM-weighted key profiles by default, Krumhansl-Schmuckler one flag away

Krumhansl-Schmuckler profiles come from probe-tone experiments on Western
classical music. Their known failure is confusing a key with its relative major
or minor, which bites hardest on electronic music that leans on a repeated root
and may never state a leading tone. The default profile set is therefore the
EDM-weighted one (heavier tonic and dominant), after Shaath's work on KeyFinder.

**Caveat worth knowing:** the Krumhansl-Schmuckler coefficients here are the
widely published ones and can be checked against any reference. The EDM
coefficients were transcribed from secondary sources and have been verified only
against this crate's synthetic tests, which both sets pass. If the real
validation table shows key accuracy is worse than it should be,
`--key-profile krumhansl` is the A/B, and it is a one-line change to make it the
default.

## 15. Key names are spelled the way the Camelot wheel spells them

Flats for every black key: "Eb minor / 2A", never "D# minor". The consumer is a
DJ-adjacent tool and every harmonic mixing chart in the world uses these
spellings; printing the enharmonic equivalent just makes the reader translate.

## 16. Resolution is cached as well as analysis

The brief asks for results to be cached by resolved store track ID, and they
are. But resolution is the rate-limited step: at 18 requests a minute, a second
pass over a ten-thousand-track library against a *fully populated* result cache
would still spend nine hours asking the search API to repeat itself. So a second
table maps a normalized `artist\u{1}title` (or `id:N`) to the match it resolved
to, and a cached resolution short-circuits the request.

The two caches are independent: a cached resolution does not imply a cached
analysis, which is what makes an algorithm-version bump re-analyze without also
re-resolving.

## 17. Cache keys include the analysis options

A row is a hit only when the track ID, the algorithm version *and* the analysis
options all match. Key detection depends on which profile set is selected, so
`--key-profile krumhansl` must not be served an answer computed under the EDM
profiles. Anything added later that changes the output has to appear in
`AnalysisOptions::cache_key` or it will silently serve the wrong thing.

A row whose payload no longer deserializes is treated as a miss rather than an
error. Re-analyzing costs one request; erroring would wedge the consumer until
someone deleted the file by hand.

## 18. Batch output is one line per input line, in input order

Work runs concurrently, bounded by a semaphore, but results are awaited in order
so output order matches input order. A line that is not valid JSON still
produces a result object, so a consumer reading positionally never loses
alignment.

`client_ref` exists for the consumers that would rather not rely on position at
all: selecta keys its library on Music.app persistent IDs, which mean nothing to
the store, and this carries one through untouched.

## 19. Key confidence needs a tonality gate, because correlation cannot see one

Pearson correlation is invariant to scale and offset. A chroma that is
essentially flat with a 2% ripple correlates with a key profile exactly as well
as one with unmistakable tonal peaks — which is how a click track came back as
B minor at 0.65 confidence during the first validation run. The profile fit was
genuinely good; there was simply nothing there for it to fit.

Confidence therefore multiplies the profile fit by a gate on chroma salience
(standard deviation over mean), ramping from 0.15 to 0.55. Measured on
synthetic material: sustained chords 0.92-1.00, a full arrangement 0.83, drums
alone 0.20-0.25, white noise 0.005. The gate is deliberately a multiplier rather
than another exponent factor — below the floor there is no tonal content to have
an opinion about, however well the profiles happen to fit.

## 20. Two validation commands, and the table goes to stderr

`selftest` runs the accuracy check against synthesized audio and needs nothing;
it gates CI, where an octave or metric regression would otherwise go unnoticed
until someone ran the real thing. `validate` runs the same check against ten
well-known releases with widely documented tempos across house, techno and drum
& bass, which is where octave handling meets real recordings.

Both print the human-readable table to **stderr** and a machine-readable report
to stdout. It would be easier to print the table on stdout, but "stdout is JSON
only" is worth more as a rule with no exceptions than as a rule with one
reasonable-sounding one.

Reference tempos are the commonly cited figures and sources disagree by a BPM or
two, so the tolerance is 2 BPM and the verdict column names *how* an estimate is
wrong — `OCTAVE`, `METRIC`, `wrong` — rather than just that it is. An octave
error appearing there would mean the canonical fold is not doing its job, which
is a different bug from a scoring one.

## 21. The resolution cache carries a matcher version; the analysis cache does not

`ALGORITHM_VERSION` gates the analysis cache, so a DSP change is never served
from stale rows. The resolution cache had no equivalent: it is keyed by query
text, so a change to how a query is matched against store results would never
reach anyone whose cache was already warm — they would keep being handed
whichever track the old scorer picked. `MATCHER_VERSION` now prefixes the
resolution key, so a matching change misses on purpose. Old rows are left in
place rather than deleted; the cache is disposable and a dead row costs bytes,
not correctness.

A resolution by store track ID is exempt. An ID identifies rather than
describes, so no amount of matching change can make it point somewhere else.

## 22. A cache hit reuses the analysis, never the match

The analysis cache is keyed by store track ID, which is right: the audio is the
same whoever asked for it. The match is not. `match_score` and `uncertain`
describe how well *this* query matched, so serving them from whichever query
filled the row meant a sloppy fuzzy query could inherit an earlier exact one's
score and be reported as a confident match. The whole point of returning matched
metadata is that a bad match is visible, and this quietly hid one. A hit now
returns cached features and audio with the freshly resolved track.

## 23. Key correctness counts in the validation verdict

`Verdict` classifies the tempo, and the report carries both expected and
estimated keys — but the pass/fail only ever read the tempo, so `selftest` and
the CI accuracy gate would have exited zero with key estimation wrong on every
case. A row now passes only when the tempo lands *and*, where a key was
expected, the key matches. Comparison is enharmonic: "Bb minor" and "A# minor"
are the same key, and published references pick either spelling.

Most of `REFERENCE_TRACKS` carries no expected key, and that stays true —
inventing expectations to make the column look full would make the table more
authoritative than it is. No expectation means no opinion, not a pass.

## 24. A metrical tie-break, added because the live table asked for it

The first run against real recordings came back 5/10, and the per-row
diagnostics split the failures cleanly. Every query resolved to the right
recording (scores 0.96-1.00) and every preview was 0-3% silent, so neither bad
matching nor beatless clips explain anything.

Two rows were genuine scoring failures, and both had the true tempo already on
the shortlist: Sandstorm read 90.71 against a true 136.07 — exactly 2/3 — and
Brown Paper Bag read 97.22 against a true 170.03. Both reported a confidence of
0.00, and confidence *is* the margin between leader and rival, so in both the
two grids scored level.

`comb_score` never charges a grid for the beats it declines to explain. A
sparse grid is a subset of a dense one's structure, so sampling every third
beat of a real groove posts a high mean and a low spread precisely by skipping
the beats that would have cost it. Level between two metrically-related grids
therefore is not level, and the faster reading is the better answer.
`break_metrical_tie` prefers a faster metrically-related finalist within 4% of
the leader.

The margin is deliberately tight so the rule only fires where the estimator is
already reporting that it cannot tell the two apart. It cannot overturn a
confident correct reading, which is the property that makes a prior tuned on
two observations acceptable rather than reckless.

It does not fix Brown Paper Bag: 170.03/97.22 is 7/4, which `metrically_related`
does not cover and which is not being added on the strength of one track. That
row stays wrong and stays flagged at 0.00 confidence, which is the behaviour the
project wants when it cannot tell — a track in an unusual meter being reported
as "no idea" is correct, not a bug to paper over.

The remaining three failures are a separate question from scoring. In each the
estimator was internally self-consistent at its own answer and never proposed
the expected figure at all: Hey Boy Hey Girl at 127 against an expected 130
(2.4%), Born Slippy at 140.09 against 138 (1.5%, and it matched a *Remastered*
master), Inner City Life at 155 against 172. Published tempo figures disagree,
masters differ, and `REFERENCE_TRACKS` is hand-entered, so those are as likely
to be wrong expectations as wrong estimates. They are not being "fixed" by
tuning until they agree — that would be fitting the estimator to three numbers
of unverified provenance. They need independent ground truth first.

## 25. What the live table says about key detection

Only one reference track carries an expected key, and the estimator got it
wrong: Sandstorm came back E minor at confidence **1.00** against an expected B
minor, a fifth out and one step away on the Camelot wheel. Maximum confidence
on a wrong answer is the worst failure shape this project has, because the whole
contract with selecta is that confidence can be trusted.

The other nine rows are unvalidated, and most of them report confidences between
0.04 and 0.30, so they at least flag themselves as guesses. The 1.00 is the
outlier and the problem.

Two candidate causes, not yet distinguished: the EDM profile coefficients were
transcribed from secondary sources and only ever verified against synthetic
chord progressions (see entry 14), and a 30-second preview of a trance record is
quite likely to be a breakdown that genuinely sits on the dominant. Telling
those apart needs real key ground truth, which is why the reference set needs
expected keys from a source that is not this estimator.
