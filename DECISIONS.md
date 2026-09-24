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

## 24. A metrical tie-break, added because the live table asked for it — and withdrawn (see 27)

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

## 26. The reference table was wrong before the estimator was

Inner City Life sat in `REFERENCE_TRACKS` at 172 BPM. A lookup against a public
database puts it at 155, for both the album version and the radio edit, which is
what the estimator had been reporting all along with a confidence of 0.71. The
row that looked like the second-worst failure in the table was the estimator
being right and the list being wrong.

This is why entry 24 refused to tune the scorer until the expected figures had
independent backing. Tuning to close an 11% gap would have broken a correct
answer to satisfy a number typed in from memory.

Sandstorm went the other way: 136 confirmed, so its 90.71 was a real 2/3 scoring
failure, and the metrical tie-break is aimed at a bug that exists.

Consequences, all of them about provenance rather than DSP:

- Inner City Life is now 155, with a comment saying where that came from.
- Hey Boy Hey Girl (130) and Born Slippy .NUXX (138) are marked unverified,
  because they are still hand-entered and one of their neighbours was wrong.
- `key_matches` accepts an expectation that names a tonic with no mode. The
  source for Sandstorm gives a bare "B"; recording "B minor" would invent half
  the fact, and the tonic alone already catches what matters, which is that the
  estimator answered E — a fifth away, the classic chroma confusion, at
  confidence 1.00.
- Inner City Life still carries no expected key, because the album version and
  the radio edit are published in different keys and the store may serve either.

## 27. Recall in the comb score, replacing the tie-break of entry 24

Entry 24's tie-break did not fire on the live audio, and the reason it did not
is that the reasoning behind it was wrong. It rested on reading a confidence of
0.00 as "the two grids scored level". They did not: `rival` in `confidence()`
deliberately skips candidates that are metrically related to the winner, so a
confidence of 0.00 says nothing whatsoever about the gap between 90.71 and its
own 3/2. The gap was simply larger than the 4% the tie-break allowed.

The tie-break is removed rather than widened. Widening it would be tuning a
threshold until a number came out right, on a rationale already shown to be
false.

The real defect is in `comb_score`, which only ever measured how good a grid's
own points are and never charged it for onsets it missed. That is precision
without recall. A sparse grid is a subset of a dense one's structure, so
sampling every third beat posts a high mean and a tight spread precisely by
skipping the beats that would have cost it. The score is now
`precision * recall`, where recall is the share of onset energy falling within
30 ms of a beat. Precision punishes a grid that is too fast, because its points
land on nothing; recall punishes one that is too slow. The window is fixed in
time rather than as a fraction of the period, or a slow grid would get a
proportionally wider window and claim the same onsets a fast grid must hit
precisely — reintroducing the bias.

**Confidence deliberately still reads precision alone.** Recall is genre
dependent in a way that is not a statement about certainty: a breakbeat with
sixteenth hats genuinely carries much of its onset energy off the beat grid, and
folding that into confidence dropped the synthetic drum & bass cases from 0.98
to 0.57 — a change that would have selecta discarding drum & bass for being
drum & bass. Recall decides which grid wins. It does not decide how sure we are
of the winner.

The selftest stays 10/10 with confidences unchanged. Whether this fixes
Sandstorm is a question only the live run can answer, which is why the
diagnostics now print each alternate's score: if the right tempo still loses,
the next run says by how much instead of leaving it to be guessed at again.

## 28. The miss penalty is subtracted, not multiplied

Entry 27 shipped `precision * recall` with precision clamped at zero, on the
reasoning that a grid whose spread exceeds its level "has nothing to say". That
reasoning holds for synthetic audio and fails for real music, where the spread
between a strong downbeat and a weak one routinely *does* exceed the level, so
the clamp fired on nearly every candidate. The live run showed it plainly: the
alternates came back at `score 0.000` almost across the board, including the
correct answer. With every score equal, the ranking fell to sort order. Brown
Paper Bag came right by luck and Born Slippy regressed from 140.09 to 93.24 the
same way.

The penalty is now subtracted in the envelope's own units:
`mean - CONSISTENCY_PENALTY * sd - MISS_PENALTY * (1 - recall)`. Ordering is
preserved across the whole real line, so a candidate that is merely poor still
ranks above one that is worse, which is the entire job of a scoring function.

The general lesson is in the test, not the constant: synthetic signals are
cleaner than anything this will ever analyze, so a clamp or a floor that never
triggers on a test fixture can still trigger on every real track.
`scores_stay_ordered_on_an_envelope_with_uneven_beats` builds an envelope whose
grid spread exceeds its mean and asserts the candidates stay distinct and
correctly ordered. It fails against the clamped version.

## 29. `validate` never reads the cache

Two live runs came back byte-identical across a real DSP change. The scoring in
entry 28 never executed: `ALGORITHM_VERSION` was bumped in the commit that
introduced recall and not in the one that fixed it, so every track was answered
from the on-disk cache, and a cached row is indistinguishable from a fresh one
in the table.

Bumping the version is the documented rule and it was still missed one commit
after being followed correctly. So the fix is not only to bump it: `validate`
now builds its analyzer with no cache at all. It measures the algorithm, so a
cached row measures nothing, and an accuracy check that can pass on stale data
is a check that will eventually pass on stale data. The cost is ten uncached
lookups on a command that is already rate-limited and run by hand.

`analyze` and `batch` are unchanged — for them the cache is the point, and a
version bump is the correct and sufficient control.

## 30. Confidence reads scale-free factors, never `mean - sd`

The first live run of entry 28's scoring got the tempo right on Sandstorm
(136.07) and Brown Paper Bag (170.03) and reported confidence **0.00** on both.
selecta branches on confidence, so a correct answer at 0.00 is thrown away —
the worst outcome available, and worse than the wrong answer it replaced.

The cause is a category error. `confidence` was handed the winner's
`mean - CONSISTENCY_PENALTY * sd` and divided it by a fixed saturation
constant. That difference is a sound way to *rank* grids on one track, where
every candidate shares an envelope; it is meaningless against a global
threshold, because on real music it is routinely negative — entry 28's own
comment says so. Negative clarity clamps to zero and the product collapses.

Confidence now takes `mean` and `sd` separately:

- **clarity** = `mean / 2.5`, beat strength in z-scored units, which is
  comparable across tracks by construction.
- **evenness** = `mean / (mean + sd)`, scale-free: 1 for beats of equal
  strength, 0.5 where the spread equals the level. This carries what the
  subtraction was for — a grid alternating kick, hat, nothing fails here — with
  no dependence on how loud the track is.
- **margin** = the winner's score over the best unrelated reading, as an
  absolute gap in the same units rather than a ratio. The old ratio form
  divided by the winner's score, which inverts when that score is negative.

A second bug fell out of the same read: with no unrelated finalist, `rival`
defaulted to a score of `0.0`. Comb scores on real music are negative, so
"nothing competes with this reading" scored as "a rival beat it" — the most
confident case in the search producing the least confident output. It is now an
`Option`, and `None` saturates the margin.

`ALGORITHM_VERSION` goes to 5: confidences are not comparable across this.

What this does not do is change which tempo wins. Ranking still uses
`score`, untouched, so the accuracy column of the next run should be identical
to the last one and only the confidence column should move. If a tempo moves,
this change did something it was not supposed to.

## 31. Tempo ships validated, key ships provisional

The live validation set measures tempo well and key barely at all, and totalling
them into one "7 of 10" hid that. Tempo references agree across published
sources; key references do not. The screenshots that corrected entry 27's
reference figures also showed the same source listing one recording of Inner
City Life in G and its radio edit in A — the same music, two keys.

So the two halves are reported separately and only tempo gates the run. A key
disagreement still prints and still gets diagnosed with its runners-up; it just
cannot fail a build, because a red build there would be measuring the reference
rather than the estimator.

What is actually established about key: it recovers all twenty-four keys from
unambiguous synthetic material on both profile sets. That rules out a rotation
error in the transcribed profiles or a constant offset in the chroma binning,
which matters because the one real track with a documented key disagrees by
exactly one step on the circle of fifths — indistinguishable, on a single case,
from a systematic rotation. It is not one. Beyond that, nothing is established,
and the confidence field is how a consumer is told so.

This is a scope call, not a deferral: metrognome reports measurements with their
uncertainty, and claiming a validated key would be reporting a measurement we
have not made.

## 32. The reference set is fully verified; key holds at provisional (amends 31)

Both remaining disagreements were the reference, not the estimator. Published
listings give Born Slippy .NUXX as 140 (the same for the original and the radio
edit) against the 138 written here from memory, and Hey Boy Hey Girl as 127
against 130. metrognome read 140.09 and 126.99. Every tempo in the set is now
checked against an outside source and every one of them lands.

That is the third hand-entered figure in this set to be wrong — Inner City Life
was 172 in entry 27 and is 155 — and the estimator has not yet been wrong on a
figure that was verified first. The rule that follows: a disagreement is a
question about the reference until the reference has been checked, and tuning
the scorer toward an unverified number would have moved the algorithm away from
correct answers three times over.

Those listings also carry keys, which takes the documented key cases from one to
three. Born Slippy reads Bb against a published Bb, Hey Boy Hey Girl reads D
against a published D, and Sandstorm still reads E minor against a published B.
Two of three, where entry 31 had zero of one.

The call in 31 stands anyway. Two agreements are not validation, the sample is
three, and the one disagreement is the confidently wrong shape that matters
most — Sandstorm reports its wrong key at confidence 1.00 while Born Slippy
reports its right one at 0.65. A feature whose confidence is highest where it is
wrong has not earned being called validated. What has changed is that key now
has evidence for it rather than only against, and the reference set carries
enough documented keys to notice a regression.

## 33. The tempo/key trust split is a payload field, not documentation

Entries 31 and 32 settled that tempo is validated and key is not. Until now that
split lived only in prose, which meant selecta would have had to hardcode "trust
tempo, distrust key" — a rule that goes stale silently the day key is validated,
and goes stale wrongly the day a third feature arrives.

So every estimate carries `maturity: "validated" | "provisional"` alongside its
confidence. The two answer different questions and a consumer needs both:
confidence is how sure the estimator is about this clip, maturity is whether the
estimator has ever been measured against real recordings. A provisional feature
can be highly confident and wrong — Sandstorm reports its wrong key at 1.00 —
which is exactly why confidence alone cannot carry the warning.

The field is always present rather than optional, so `SCHEMA_VERSION` goes to 2:
a consumer can now count on reading it, and that guarantee is what the number
tracks. `ALGORITHM_VERSION` does not move, because no measurement changed.

Deliberately not given a serde default. A cache row written before this field
existed fails to parse and is treated as a miss, which costs one re-analysis; a
default would have let a stale row assert a maturity nothing ever measured.

## 34. Key confidence measures how determined the key is, not how well a profile fits

Entries 31 to 33 kept circling the same embarrassment: Sandstorm reported a
wrong key at confidence 1.00 while Born Slippy reported a right one at 0.65.
That is not a close call going the wrong way, it is a confidence that ranks
wrong answers above right ones, and a consumer that trusts it is worse off than
one that ignores key entirely.

Synthetic material reproduces it exactly, with no reference to argue about. Two
fifths alternating with no third between them — E5 and B5, four bars — scored B
major at confidence 1.00. A lead riff on three pitch classes scored E minor at
1.00. A single sustained note scored A major at 0.68 and was not flagged
uncertain. None of those clips contains enough information to name a key, and
the estimator claimed certainty on all of them.

Two causes, both in the scoring rather than in the chroma:

The first is saturation. Confidence multiplied three factors, each a ratio
clamped at 1.0, and every threshold sat where ordinary tonal material clears it
comfortably: a correlation over 0.75, a lead over 0.10, a salience over 0.55.
All 48 synthetic progressions pinned all three and scored exactly 1.000, so the
top of the scale was not a rare peak but the default for anything tonal, with no
headroom left to separate a good fit from a certain one.

The second is the real defect. Pearson correlation is scale- and offset-
invariant, which the salience term already accounts for, but it is also blind to
how many pitch classes carry any energy at all. A key is seven pitch classes. A
chroma with three peaks and nine near-empty bins correlates with a key profile
as strongly as a full progression does, and the gap to the runner-up is just as
wide — but it is a gap between two profiles that the evidence cannot separate,
because the evidence for the difference was never in the clip. Salience makes
this worse rather than better: it measures departure from flat, so a two-note
riff scores 1.56 where a four-chord progression scores 1.00.

So confidence now carries a fourth factor, the effective number of pitch classes
carrying tonal energy, and the margin factor is measured relative to the
winner's own correlation and saturates exponentially instead of clamping.

The pedestal subtraction in that fourth factor is the part worth writing down.
Percussion adds roughly equal energy to all twelve classes, so a raw effective
count rises with the drums: the three-note riff measures 3.7 dry and 5.2 over a
beat, which is above where a real progression sits, and the gate that was
supposed to catch it would have passed it. Subtracting the level every class
shares strips that pedestal and leaves the count stable across drum levels —
3.6 to 4.5 for the riff, 6.2 to 6.4 for the progression. Without it the fix
would have worked on a riff in isolation and failed on every real track, which
all have drums.

What this costs and what it buys, on synthetic material: all 24 keys are still
recovered on both profile sets, none of the 48 is flagged uncertain, and their
confidences now spread from 0.54 to 0.98 instead of all reading 1.000. The
under-determined clips fall below the uncertain threshold — power chords to
0.00-0.17, the three-note riff to 0.06-0.47, a drone to 0.00. A genuine
relative-pair coin flip, C major and A minor triads alternating, drops to 0.14.

One consequence of the relative margin, caught in review and worth recording:
dividing the lead by the winner's own correlation turns a small absolute gap
into a large relative one when every profile fits badly. Symmetric harmony makes
this concrete — whole-tone chords and a diminished-seventh stack divide the
octave evenly, so they sit far from every profile while still being loud, varied
and spread across the chroma, clearing the salience and coverage gates on their
own. Whole-tone chords correlate at 0.247 and led by 29% of that, which read as
0.54 and shipped as a confident Bb major; the diminished stack read 0.58, and
had done so before this change too.

The correlation term is the only thing standing between that material and a
confident answer, and it was softened by a square root. Removing the root fixes
every such case (0.31 and 0.39) and changes nothing on real material, since the
term clamps to 1.0 for any winner above the saturation and every clean key
correlates above 0.85. A root on the factor that measures whether anything fits
at all was simply the wrong shape.

The lowest clean scores are the honest ones. Ab minor's i-VI-VII-i is Abm, E,
F# and Abm, and all four chords are diatonic to B major; the estimator gets it
right only because the profile weights a repeated tonic. Reporting that at 0.54
rather than 1.00 is the point of the change.

`ALGORITHM_VERSION` goes to 7, since every cached key confidence is now
incomparable. `SCHEMA_VERSION` stays at 2: no field appeared, moved, or changed
what it promises. The numbers a consumer reads are better, which is what
`ALGORITHM_VERSION` is for.

Both key `source` strings go to `@2` for the same reason. That suffix versions
the estimator that produced a value, and selecta stores it as per-field
provenance — leaving it at `@1` would have let a stored confidence from before
this change and one from after it claim the same origin. `ALGORITHM_VERSION`
does not cover that: it invalidates metrognome's own cache, where `source`
travels with the value into a consumer's database and outlives the run.
`TEMPO_SOURCE` stays at `@1`, since tempo scoring did not move.

Key stays provisional. This makes its confidence worth reading; it does not make
the estimator validated, and entry 31 still needs a larger ground-truth set than
three documented cases. Whether Sandstorm now reads B, or reads E minor with a
confidence low enough to be discarded, is a question for `metrognome validate`
on real audio — the sandbox cannot fetch previews. Either outcome is an
improvement over asserting the wrong one at 1.00.

One observation for selecta, not acted on here: in all 48 synthetic cases the
runner-up is the relative major or a fifth-related neighbour, and a relative
pair shares its Camelot number. A key confusion of that shape costs almost
nothing on the wheel, so a low-confidence key may still carry a usable Camelot
position. Acting on that would mean a separate confidence for the wheel
position, which is not worth inventing until the ground-truth set exists.

## 35. Salience measures arrangement density, so it cannot gate a key (amends 34)

Entry 34 predicted that real audio would settle whether Sandstorm reads B or
reads E minor quietly enough to discard. It did neither, and the answer was more
interesting than either branch.

Ten reference tracks, then thirty-one across house, techno, trance, drum & bass,
breaks, ambient, downtempo and disco, all with the per-factor breakdown
attached. Two results stand out, and both say the gates were fitted to the wrong
material.

`coverage` — the fourth factor entry 34 added — reads 1.000 on thirty of the
thirty-one, and 0.96 on the last. Real tracks measure 5.4 to 9.7 tonal pitch
classes against a saturation of 5.5. It catches the synthetic sparse riff it was
written for and nothing else; on real music it is inert. It stays, because the
riff case is real, but it does no work here.

The salience term — `tonality` until this entry, `structure` after it — is the
one that bit, and it bit indiscriminately: it was the smallest factor on
twenty-four of the thirty-one, and only four tracks survived at all.
Salience runs 0.105 to 0.765 on real music, against a ramp from 0.15 to 0.55, so
almost every real track sits inside it and is marked down by an amount that has
nothing to do with whether its key is determinable.

What salience actually tracks is arrangement density. Brian Eno's *An Ending*
scores 0.765 and Aphex Twin's *Xtal* 0.430; Green Velvet's *La La Land* scores
0.124 and Plastikman's *Spastik* 0.140. That ordering is real and correct — it
is sparseness — but a dense club record states a key perfectly well, and a
gate built on density discards it for being dense. The three tracks in the
reference set with a documented key make the cost concrete: *Hey Boy Hey Girl*
estimated D major, which is right, and was discarded at 0.20.

Percussion and a dense mix are not separable by salience: measured drums sit at
0.20-0.25 and so do *Archangel*, *Blind Faith* and *Right Here, Right Now*.
Correlation separates them cleanly instead — a click track 0.35, four-on-the-
floor 0.40, a breakbeat 0.43, against 0.53-0.91 for real tracks carrying
harmony, with the percussion-led records (*Spastik* 0.304, *Phat Planet* 0.332)
correctly at the bottom. So the correlation term takes over the job, ramped from
0.45 rather than from zero, since all the discrimination lives in that band.

The name goes with the job. A factor called `tonality` reading 1.00 on a dense
club track invites exactly the misreading that produced this entry, so it is
`structure` now: whether the chroma has any shape to correlate against.

Salience keeps one job, which nothing else can do: white noise correlates 0.633
with some profile, because correlation is offset-invariant and cannot see that
the chroma is flat. Noise measures 0.005 salience against 0.105 for the least
tonal real track — two orders of magnitude apart — so the term becomes a floor
at 0.02, not a ramp.

Two dead ends, measured, so they are not tried again. Consistency guards invert:
a drum loop's chroma is a stable biased shape, so independent slices of a click
track agree on a key 100% of the time while a real chord progression agrees 50%,
each slice holding one chord. And alternative amplitude formulas — the fraction
of energy above the floor, peak height above the floor — track the existing
coefficient of variation almost exactly. Neither is worth swapping in.

Sandstorm is unchanged and stays wrong: correlation 0.918 against 0.636 for the
runner-up, with the published B minor third at 0.577. The chroma decisively
believes E minor, and no confidence term reaches that. It is a limit of profile
correlation over a 30-second preview, not a scoring defect, and it should be
read alongside entry 31's standing caveat that published key data is itself the
weaker half of the comparison. E minor and B minor are adjacent on the Camelot
wheel, so the practical cost is one step.

Lowering the salience floor exposed something the old one hid by accident.
Averaging is what flattens noise, so a chroma built from a handful of frames is
not flat, and the 0.15 floor had been rejecting that case for the wrong reason.
Over 300 random draws the worst confidence reaches 0.89 on a fifth of a second
against 0.09 at a second and a half, so key estimation now declines below 1.5
seconds. Duration is the honest gate there: no threshold on a statistic
computed from one frame can tell a real chroma from a lucky one.

Seconds rather than frames, and the duration counted properly, which took two
attempts. `Stft::for_chroma` rounds its window to a power of two, so the frame
rate is 21.5/s at 44.1kHz but 11.7/s at 48kHz: a fixed frame count put the
cutoff at 1.6 seconds against 3.0, and counting hops alone still put it at 1.67
against 1.79, because the first frame costs a whole window rather than a hop
and that window is twice as long at 48kHz. The chroma now records the audio it
covers, window included, and the cutoff lands within 1.533-1.536s at 22.05,
44.1, 48, 88.2 and 96kHz.

Measured by seconds the rates also agree on the risk being guarded against
(0.09 at both at the cutoff); measured by frames they differ by two to four
times, since a 48kHz frame averages twice as many FFT bins into each pitch
class. Duration is both the consistent gate and the fair one.

Key stays provisional, and the ground-truth set entry 31 asked for still does
not exist — thirty-one tracks measured the gates, not the answers.

## 36. Tempo confidence is mostly honest; the discrimination behind it is not

The 2026-09-20 validate run on real previews put all ten reference tempos within
0.1 BPM and reported confidences from 0.09 to 0.95. Two of the ten — Brown Paper
Bag at 0.09 and Born Slippy at 0.34 — sit at or below `UNCERTAIN_AT_OR_BELOW`, so
a consumer discards a correct answer. Sandstorm clears it by 0.045.

The first read of this was wrong, and it is worth writing down why. Ranking the
ten tracks by the best score in their own alternates list reproduces the
confidence order exactly, which looked like proof that confidence tracks an
absolute level and therefore that `clarity` — the one factor measuring level, at
the heaviest exponent — was the cause. It is a real correlation and a false
cause. A second run with the factors instrumented shows `clarity` is not even
monotonic with confidence: Brown Paper Bag scores 0.500 on it against Born
Slippy's 0.386 and still lands a quarter of the confidence.

What the factors actually say, per track:

- Brown Paper Bag, 0.089: clarity 0.500, evenness 0.319, periodic 0.296, and
  `margin` 0.0027. The margin is the whole story. The comb scorer rated a
  97.22 BPM grid within three thousandths of the correct 170.03. Confidence is
  not miscalibrated here — it is honestly reporting that selection was a coin
  flip that happened to land right.
- Born Slippy, 0.341: clarity 0.386 and periodic 0.228, from an autocorrelation
  of 0.114 at the beat lag. No unrelated rival at all, so margin is 1.0.
- Sandstorm, 0.545: clarity 0.664, evenness 0.464, margin 0.433, three mild
  penalties and no single cause.

So there is no one term to retune, and the coverage problem is not in the
confidence formula. Brown Paper Bag's is in candidate discrimination: a dense
break where a grid at 4/7 of the true tempo scores as well as the true one.

The instrumented run did surface one clear defect, in the opposite direction from
the one being hunted. Four of the ten tracks — Born Slippy, Music Sounds Better,
Hey Boy Hey Girl and Tarantula — report no unrelated rival and so take `margin`
1.0 for free, because in each case the only competitor sits at 3/2 or 4/3 of the
chosen tempo and `metrically_related` counts those as the same musical answer.
The filter's stated reason is that a candidate at half the chosen tempo is one
answer seen twice, which is true of an octave and only of an octave — and
candidates are folded into `CANONICAL_LOW_BPM..CANONICAL_HIGH_BPM`, 90 to 180,
exactly one octave, so no two folded candidates can ever be an octave apart and
the case the filter was written for is unreachable. Its entire effect is to
excuse the metric confusions, which `CANDIDATE_RATIOS` expands on purpose and
`Verdict::MetricError` exists as a separate verdict to catch. Confidence is blind
to exactly the error mode the validator checks for, in the direction that invents
certainty.

Narrowing that filter to octaves was built and measured, and is **deliberately
not shipped here**. Its cost on the reference set:

| Track | before | after | newly counted rival |
|---|---:|---:|---|
| Sandstorm | 0.55 | **0.47** | 90.71, gap 0.23 — crosses the discard line |
| Born Slippy | 0.34 | **0.27** | 93.24, gap 0.40 |
| Inner City Life | 0.87 | 0.80 | 103.32, gap 0.72 |
| Music Sounds Better | 0.95 | 0.95 | 165.60, gap 3.78 |
| Hey Boy Hey Girl | 0.91 | 0.91 | 169.30, gap 2.63 |
| Tarantula | 0.82 | 0.82 | 116.04, gap 1.63 |

Sandstorm is what settles it: 136.07 against a published 136, discarded because
the scorer rates a 90.71 grid within 0.23 of it. The number is honest, and the
outcome is that a track which is unambiguously 136 BPM reports no tempo at all.
Every one of these gaps is small for the same reason Brown Paper Bag's is: the
comb scorer cannot separate a tempo from its own 3/2 on sixteenth-dense material.
Jonas's call (2026-09-20) is to fix the scorer first and let the gaps widen on
their own, then narrow the filter against gaps that mean something. Shipping the
filter change first would trade real coverage for a correctness that the scorer
fix is expected to give back for free.

Which tracks would move was also mispredicted, and that is worth recording.
Three of the four reporting no rival at all turned out to have gaps above 1.0, so
counting them changed nothing; the movement came from two tracks whose rival was
already counted, because a *closer* 3/2 neighbour displaced it. The margin only
bites below a gap of 1.0, and which candidate sits nearest is not visible from a
confidence number.

The scorer defect itself has a shape. All four of these rivals sit at an exact
integer number of sixteenth notes per beat-grid step — 6, 7, 6, 6 — and on
material with an event on most sixteenths any such grid finds an onset at every
step it predicts. `recall` across every candidate on those tracks runs 0.054 to
0.175, so `MISS_PENALTY * (1 - recall)` is near-constant and stops separating
anything; `precision` is left to do the work alone, and it cannot tell a beat
from a beat times 2/3.

The measurements a fix is graded against, from the instrumented run. `score` is
the chosen reading's own comb score and `gap` is its lead over the named
neighbour, whether or not the rival filter currently counts it:

| Track | BPM | score | nearest metric neighbour | gap |
|---|---:|---:|---|---:|
| Brown Paper Bag | 170.03 | -3.335 | 97.22, 7/4 below | **0.003** |
| Sandstorm | 136.07 | -2.393 | 90.71, 3/2 below | 0.232 |
| Born Slippy | 140.09 | -2.601 | 93.24, 3/2 below | 0.402 |
| Inner City Life | 154.99 | -0.544 | 103.32, 3/2 below | 0.721 |
| Tarantula | 174.11 | -0.825 | 116.04, 3/2 below | 1.629 |
| Show Me Love | 120.23 | -0.646 | 160.30, 4/3 above | 1.909 |
| Hey Boy Hey Girl | 126.99 | -0.143 | 169.30, 4/3 above | 2.627 |
| Around the World | 121.28 | 1.058 | 161.69, 4/3 above | 3.550 |
| Music Sounds Better | 124.20 | 1.577 | 165.60, 4/3 above | 3.778 |
| Call on Me | 126.30 | 2.409 | 168.41, 4/3 above | 5.410 |

The correlation to work from: the four smallest gaps all belong to tracks whose
winner scores *negative*, and all three tracks scoring above zero have gaps past
3.5. A scorer that cannot explain the true beat well in absolute terms cannot
separate it from its own subdivisions either, so these are one problem and not
two.

A synthetic probe at 170 BPM reproduces it: true beat
-0.505, six sixteenths -0.690, eight -0.717, ten **+0.240 — winning outright**.
That is the follow-up, and it is a selection bug, not a calibration one.

`pulse_strength` was considered as a level factor that might behave better and
rejected: it is `raw_sd / raw_mean` on the un-normalized flux, so a busier clip
lowers it too. Across one clutter sweep it fell from 4.13 to 1.15 while the
envelope's beat-grid mean fell from 5.80 to 1.49. It is the same measurement in
different units.

This change therefore measures and reports, and alters no estimate. The five
factors and their raw inputs travel on the estimate and print per track,
including the chosen reading's own comb score — the alternates list every loser
and never the winner, so a margin could not be checked against the scores printed
beside it, which cost this investigation two rounds. A row whose tempo landed but
is flagged uncertain gets a diagnostics block and a count of its own, because a
correct answer nobody keeps is a failure of the same run. No `ALGORITHM_VERSION`
bump: no DSP behaviour moved. No `SCHEMA_VERSION` bump either — the factors are
`skip_serializing`, so the consumer contract is byte for byte what it was.

Born Slippy's remaining oddity is left open: its autocorrelation at the chosen
beat lag is 0.114, against 0.4 to 0.9 for every other reference track. The tempo
is right and the periodicity the envelope shows at it is nearly absent, which
points at the onset envelope missing most of that preview's beats rather than the
scorer misreading them.

The lesson is the one entry 34 also paid for. On 24 synthetic cases `clarity`,
`margin` and `coverage` were pinned at exactly 1.000 and confidence never left
0.77 to 0.999, against 0.09 to 0.95 on real previews, so the suite could not
distinguish any of these explanations. Reasoning backwards from the two numbers
the output did expose produced a confident, wrong answer; one instrumented run
produced the right one in minutes. Instrument before theorising.

## 37. The comb scorer judges a beat grid's whole metrical lattice

Entry 36 left the tempo scorer unable to separate a tempo from its own 3/2 on
sixteenth-dense material, with the margin fix held back because closing it would
have cost coverage the scorer had not earned. This is that fix.

The defect is sharper than the small margins made it look. A synthetic
`OffbeatTrance` groove at 170 BPM with clutter between the beats is read as
**136 BPM at confidence 0.979** — a confidently wrong answer, not a hesitant
right one. 136 is 4/5 of 170, a grid every five sixteenths.

The mechanism is the one entry 36 predicted. `comb_score` ranks a candidate as
`precision - MISS_PENALTY * (1 - recall)`, and `recall` asks what share of the
onset energy falls on the grid's *beats*. On material with an event on most
sixteenths that share runs under 0.2 for every candidate, so the penalty is
near-constant and cancels out of the comparison. `precision` — `mean - sd` on
the grid's own points — is left to decide alone, and it prefers whichever grid
finds the tidiest set of events, which on dense material is routinely the wrong
one: the true beat alternates kick, snare and a bar of nothing, while a grid at
4/5 of it samples a uniform spread of sixteenths.

**The fix: charge the miss penalty at three metrical levels** — the beat, its
eighths and its sixteenths (`LATTICE_DIVISORS`) — instead of at the beat alone.
A true beat's lattice accounts for where the music puts its events; a grid at
3/2 or 4/5 of it predicts subdivisions that fall *between* the real ones, and
the lower levels say so even when the beat level has gone flat. `MISS_PENALTY`
is unchanged at 3.0 and is now charged once per level.

Seven formulations were measured over 51 synthetic cases (three grooves, four
tempos, three clutter settings), scoring each on how often the true tempo won
and how far ahead it finished:

| scoring | true tempo wins | smallest gap | median gap |
|---|---:|---:|---:|
| beat only (before) | 49/51 | 0.35 | 3.93 |
| sixteenths only | 50/51 | 0.04 | 4.89 |
| beat + sixteenths | 51/51 | 0.05 | 4.94 |
| **beat + eighths + sixteenths** | **51/51** | **1.04** | **5.89** |

Replacing the beat level rather than adding to it is not enough: a lattice at
one depth can be a superset of the material's own grid, which is why 3/4 of a
click track scores a perfect sixteenth-level recall. Three levels together have
no such blind spot in the set measured.

**The margin fix from entry 36 now lands free, so it ships here too.**
`metrically_related` is deleted and the nearest reading at any other tempo
counts against confidence. On the ten reference previews, measured on real
audio:

| Track | before | margin fix alone | both |
|---|---:|---:|---:|
| Brown Paper Bag | 0.089 | 0.089 | **0.392** |
| Born Slippy | 0.341 | 0.27 | 0.341 |
| Sandstorm | 0.545 | 0.47 | **0.673** |
| Inner City Life | 0.870 | 0.80 | 0.870 |
| the other six | 0.88-0.95 | unchanged | 0.88-0.95 |

Every one of the ten now has a gap over its nearest rival above 1.0, so `margin`
saturates for all of them and counting metric neighbours costs nothing — the
smallest gap moved from 0.003 to 1.18. All ten stay within 0.1 BPM, discards
stay at two, and Sandstorm clears the consumer's cutoff with headroom rather
than being binned by the honest accounting.

**What this does not fix, and entry 36 got wrong about it.** Entry 36 said
Brown Paper Bag's `margin` of 0.0027 "is the whole story". It was not. With the
margin at a full 1.0 the track reaches only 0.392, because `clarity` 0.500,
`evenness` 0.319 and `periodic` 0.296 cap it there — a 4.4x improvement and
still discarded. Its beat grid really is weak in the envelope, which is the same
complaint as Born Slippy's autocorrelation of 0.114 and points at the onset
envelope rather than the scorer. That remains open.

**A limit worth naming.** The lattice cannot separate a tempo from its 4/3 when
the material carries an equally loud event on *every eighth*: both readings then
explain the audio, and the gap on a synthetic case built that way falls from
0.284 to 0.020, taking confidence to 0.35. The winner is still correct; the tool
reports that it is unsure, which is the behaviour to want. No reference track
looks like this — it takes clutter synthesized at kick amplitude to reach.

`ALGORITHM_VERSION` 8 to 9 and `TEMPO_SOURCE` to `@2`: every cached tempo
confidence moves, and some estimates do. `SCHEMA_VERSION` stays at 2 — no field
is added, moved or redefined.

## 38. The onset envelope is whitened per band; sparse tracks were not an envelope problem

Entry 37 left Brown Paper Bag (0.39) and Born Slippy (0.34) under the discard
line and blamed a weak onset envelope. That diagnosis was only half right, and
this entry records both halves.

**What was measured.** Around thirty envelope variants were run through the real
tempo estimator on the ten reference previews, fetched once into memory:
flux lag of 2-4 frames, a SuperFlux-style frequency max filter, 128 mel bands,
log-compression constants from 100 to 10000, rectifying after detrending,
median-filter harmonic/percussive separation at four kernel sizes, and
per-band adaptive whitening at several memories, floors and compressions. Only
whitening helped consistently; everything else moved Born Slippy by under 0.01
or made it worse (the max filter and percussive separation cost it margin).

**Why no envelope rescues them.** Instrumented, both tracks are periodic per
bar and not per beat, in every band:

| Track | acf at 1 beat | 2 beats | 4 beats | 8 beats | strongest peak |
|---|---:|---:|---:|---:|---|
| Born Slippy | 0.11 | 0.12 | 0.19 | 0.25 | 1.5 beats, 0.36 |
| Brown Paper Bag | 0.15 | 0.13 | 0.17 | 0.29 | 8 beats, 0.29 |
| Call on Me, for scale | 0.92 | 0.90 | 0.85 | 0.78 | 1 beat |

Split into eight mel band groups, Born Slippy's beat-lag autocorrelation stays
under 0.08 in every one, kick band included; its strongest periodicity is the
dotted-quarter grouping of the synth and vocal. Removing harmonic content does
not change that, so the grouping is carried by percussive events too. The
confidence is reporting the audio honestly: the beat is right, and the preview
barely states it. Reading periodicity at the bar instead of the beat was also
measured and lifts Brown Paper Bag by 0.015 and Born Slippy by 0.05, because
clarity and evenness hold them down as much as periodicity does. Even a perfect
`periodic` would leave Born Slippy at 0.49.

**What ships: per-band adaptive whitening.** Each mel band is divided by a
decaying follower of its own peak (3 s memory, floored at -40 dB under the
clip's loudest value) before log compression, whose constant drops from 1000 to
10 because it now sees a 0-1 ratio. A sustained loud band no longer outweighs
the quieter bands a syncopated beat lands in. The envelope also stops depending
on playback level, which the future live-listening path will need.

Real audio, `validate --no-cache`, before and after:

| Track | BPM before | after | conf before | after | acf before | after |
|---|---:|---:|---:|---:|---:|---:|
| Brown Paper Bag | 170.03 | 170.20 | 0.392 | **0.460** | 0.148 | 0.193 |
| Sandstorm | 136.07 | 136.11 | 0.673 | **0.710** | 0.525 | 0.656 |
| Around the World | 121.28 | 121.27 | 0.925 | 0.955 | 0.694 | 0.806 |
| Tarantula | 174.11 | 174.11 | 0.823 | 0.841 | 0.406 | 0.444 |
| Born Slippy | 140.09 | 140.09 | 0.341 | 0.338 | 0.114 | 0.112 |
| the other five | | within 0.02 BPM | 0.87-0.95 | 0.88-0.95 | | |

Ten of ten stay within 0.2 BPM, discards stay at two. Autocorrelation at the
chosen beat rises on eight of ten. On the synthetic selftest every tempo stays
exact and confidence moves from 0.94-0.96 to 0.90-0.98. Seven extra previews
with unverified tempos were checked in the lab (an earlier variant with an
absolute floor): none changed tempo by more than 0.02 BPM, Aphex Twin's Xtal
rose from 0.74 to 0.92, and the largest drops were Teardrop (0.65 to 0.61) and
Porcelain (0.90 to 0.86).

The alternatives were tuned and rejected on the numbers above rather than on
principle, so the constants carry the measurement: a 1 s memory gave Brown Paper
Bag 0.44 and 6-10 s gave 0.47, a flat region; a compression constant of 1 gave
0.48 and cost Born Slippy margin; the floor made no difference on real previews
between 1e-4 absolute and -40 dB relative, and relative was chosen so a quiet
clip is not whitened into its own noise floor.

**What stays open.** Neither track clears the line, and pushing them over would
mean reading bar-level structure as beat evidence, which would lift a 4/3
reading of a breakbeat just as much. If that is ever worth doing it is a change
to the confidence formula, judged on a breakbeat-heavy reference set, and not
an envelope change.

`ALGORITHM_VERSION` 9 to 10 and `TEMPO_SOURCE` to `@3`: every cached tempo
confidence moves. `SCHEMA_VERSION` stays at 2.

## 39. A key label version of its own, held to the scorer's output by a test

Issue #13 reported that the key label stayed at `@3` across the #5/#7/#8
recalibration. The history says otherwise: `44247e9`, the commit that took
`ALGORITHM_VERSION` 7 to 8, also moved both key labels `@2` to `@3`, and every
later key change in that PR landed before the merge. The pinned outputs below
are identical at the #8 merge (`e787500`) and on `main` after #10 and #15, so every
stored `@3` key comes from one scorer and needs nothing on selecta's side. What
was real is that the bump rests on memory: `@1` to `@2` needed a follow-up
commit (`db9e745`) because the scoring change shipped without it.

**The label gets its own counter, `KEY_SCORER_VERSION`, not a copy of
`ALGORITHM_VERSION`.** The algorithm version moves for tempo changes too, and a
key label that moved with it would have selecta re-measure every key after a
tempo-only fix. The cache still keys on `ALGORITHM_VERSION`, so both bump when
key output moves.

**A test pins the scorer's output on fixed synthetic material** — thirteen
signals, both profiles, key plus confidence and correlation — and fails on any
change with the new table printed and an instruction to bump. A second
assertion ties the pin to the version constant, so bumping one without the
other fails; a re-pin that skips the bump is left to review, where it shows as
a table change with no version change. Tolerance is 0.005: loose enough that platform `libm` differences
can't trip it, and mutating `KEY_MARGIN_SCALE` 0.10 to 0.11 or
`COVERAGE_FLOOR` 3.5 to 3.8 still does. It cannot tell a deliberate change
from an accident; it only makes forgetting the label impossible.

Tempo gets the rule in `CLAUDE.md` but not the test yet: #15 was moving tempo
output while this was written, so its pin is a follow-up.
