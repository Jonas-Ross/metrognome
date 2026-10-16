# Review instructions

CLAUDE.md holds the project's rules; this file only tunes how reviews weigh them.
selecta stores what metrognome reports, so wrong output is worse than none.

## What Important means here

Important is anything that would change what a consumer receives or trusts:
incorrect DSP or resolution logic, a broken hard rule from CLAUDE.md, a change
to the JSON contract or an `ErrorKind` string, a confident result where the
estimate should be flagged `uncertain`, a matcher that can attach another
song's preview, or documentation that now contradicts the code. A test
weakened, loosened or removed without cause is Important too.

Style, naming and refactoring suggestions are Nit at most. Report at most five
Nits per review.

## Always check

- A DSP change that can move cached results bumps `ALGORITHM_VERSION`; one that
  can move key output bumps `KEY_SCORER_VERSION`; one that can move tempo bumps
  the `@N` in `TEMPO_SOURCE`.
- A consumer-visible JSON change bumps `SCHEMA_VERSION` (a field that may be
  absent does not).
- Every new DSP constant has a one-line reason next to it.
- Nothing prints to stdout except the JSON result; logs go to stderr.
- No audio bytes reach disk; only results are cached.
- The iTunes rate limiter is not bypassed or tuned upward.
- Analysis functions take PCM plus a sample rate and nothing about where audio
  came from.
- No test touches the network.

## Do not report

- Anything CI already enforces: rustfmt, clippy, the wasm build.
- `Cargo.lock`.
- Recorded fixture JSON under `crates/metrognome/tests/fixtures/`.
