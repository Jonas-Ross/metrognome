# metrognome

BPM and musical-key estimation for tracks you can't analyze directly, by way of
their 30-second iTunes preview clips.

Built to be driven by [selecta](https://github.com/Jonas-Ross/selecta), which
automates Apple Music and needs audio features for a library of DRM'd streaming
tracks. Its usual upstream, AcousticBrainz, has no data for anything released
after 2022. Preview clips are plain unencrypted AAC, so they can be analyzed.

## Interface

```
metrognome analyze --artist "Daft Punk" --title "Around the World"
metrognome analyze --track-id 1440857781
metrognome batch < tracks.jsonl > results.jsonl
metrognome probe --url https://.../preview.m4a
```

- `analyze` prints exactly one JSON object on stdout.
- `batch` reads one JSON object per line on stdin and writes one result per
  input line, in input order. A track that fails produces an error result, not a
  crash — one bad row never kills the run.
- **stdout carries nothing but JSON.** Logs go to stderr, always.
- Results are cached on disk by resolved iTunes store track ID, so a track is
  analyzed once.

## Install

```
cargo build --release
./target/release/metrognome --help
```

## Layout

| Path | What lives there |
|---|---|
| `crates/metrognome` | Library: DSP, resolution, cache. No CLI assumptions. |
| `crates/metrognome-cli` | Thin binary: argument parsing, JSON out, logging. |
| `DECISIONS.md` | Design forks and why each was called the way it was. |
| `CLAUDE.md` | Conventions for agents (and humans) working in here. |

The analysis API takes PCM samples plus a sample rate and nothing else, so the
same code can serve a live capture tap later without unpicking a preview-URL
assumption.

## Accuracy, honestly

Tempo is reported in a canonical one-octave window (see `DECISIONS.md`), which
makes the classic 87-vs-174 drum & bass error impossible at the cost of
reporting genuinely slow music at double time. Alternates are always included so
a consumer can override.

Every estimate carries a 0-1 confidence. Previews are sometimes a beatless intro
or a breakdown, and a low confidence score is the estimator telling you so.

## License

MIT
