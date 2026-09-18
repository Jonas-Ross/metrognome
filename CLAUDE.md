# metrognome

A Rust CLI that estimates BPM and musical key from short audio clips —
in practice, the 30-second iTunes preview for a track. It exists because
[selecta](https://github.com/Jonas-Ross/selecta) needs audio features for a
library of DRM'd streaming tracks and AcousticBrainz is dead post-2022. selecta
calls this binary as a subprocess, so the machine interface comes first.

`README.md` is the user-facing overview. `DECISIONS.md` records design forks.

## Architecture

Two crates, split so that analysis has no I/O assumptions baked in:

- **`crates/metrognome`** — the library. DSP (`dsp`, `tempo`, `key`), resolution
  (`resolve`), fetching (`fetch`), cache (`cache`), shared types (`types`).
  Analysis entry points take PCM samples plus a sample rate, nothing more. The
  eventual goal is a background process that live-listens on macOS, so no
  analysis function may assume audio came from a preview URL.
- **`crates/metrognome-cli`** — the binary, named `metrognome`. Argument
  parsing, JSON serialization, logging setup. Thin: if it contains logic worth
  testing, that logic belongs in the library.

## Hard rules

- **stdout is the JSON channel.** One object per `analyze`, one per line for
  `batch`. All logging, progress, and diagnostics go to stderr. A stray
  `println!` is a broken interface, not a cosmetic bug.
- **Audio never touches disk.** Previews are fetched into memory, decoded, and
  dropped. No temp files, no cache of audio bytes — only of results.
- **Errors: `thiserror` in the library, `anyhow` in the binary.** Library errors
  carry a stable `ErrorKind` discriminator that consumers branch on; the strings
  are interface, so don't rename them casually.
- **One bad track never kills a batch.** Failures are result objects with an
  `error` field, emitted in input order alongside successes.
- **No taste.** metrognome reports measurements — tempo, key, confidence — and
  never ranks, recommends, or picks tracks. That is selecta's model's job.
- **Rate limits are respected, not probed.** The iTunes Search API allows
  roughly 20 requests/minute; the limiter is not optional and is not tuned
  upward to go faster.

## Engineering defaults

- **Comments explain why, not what.** This matters most for DSP constants. A
  window size, hop divisor, prior range, or fold window with no rationale next
  to it is an unreviewable magic number — write the reason down.
- **Tests land with the code.** DSP is tested against synthetic signals from
  `testsig` (click tracks at known BPM, chord progressions in known keys) with
  explicit tolerances. Network resolution is tested against recorded fixture
  JSON in `crates/metrognome/tests/fixtures/`. Nothing in the test suite touches
  the network.
- **Small, minimal-dependency code wins.** Adding a dependency needs a sentence
  in the commit message saying why.
- **Bump `ALGORITHM_VERSION`** whenever a DSP change makes old cached results
  incomparable. The cache treats a version mismatch as a miss.

## Commands

| Command | Use |
|---|---|
| `cargo build --release` | Build the binary |
| `cargo test --workspace` | Full test suite, no network |
| `cargo fmt --all` | Format |
| `cargo clippy --workspace --all-targets -- -D warnings` | Lint, warnings denied |
| `cargo run -p metrognome-cli -- <args>` | Run the CLI in place |

CI gates on fmt, clippy with warnings denied, and tests.

## Git workflow

- Feature branches off `main`; never commit directly to `main`.
- [Conventional Commits](https://www.conventionalcommits.org/):
  `<type>(<scope>): <subject>`, imperative, lowercase, no trailing period.
- One concern per commit. Keep the build green.
