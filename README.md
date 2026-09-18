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

## Output

Every object carries `schema_version`, and every feature carries its own
`source` and `confidence` plus an `uncertain` flag, so a consumer can tell a
measurement from a guess. Features live under `features`, which grows by adding
optional fields rather than by moving existing ones.

```json
{
  "schema_version": 1,
  "algorithm_version": 1,
  "status": "ok",
  "query": { "artist": "Daft Punk", "title": "Around the World" },
  "track": {
    "track_id": 1440857781,
    "artist": "Daft Punk",
    "title": "Around the World",
    "album": "Homework",
    "preview_url": "https://.../preview.m4a",
    "match_score": 1.0,
    "uncertain": false
  },
  "features": {
    "tempo": {
      "bpm": 121.31,
      "confidence": 0.86,
      "uncertain": false,
      "source": "metrognome/onset-autocorrelation-comb@1",
      "beat_offset_secs": 0.104,
      "canonical_window_bpm": [90.0, 180.0],
      "alternates": [{ "value": 60.66, "relation": "half", "score": 1.9 }]
    }
  },
  "audio": { "duration_secs": 30.0, "sample_rate": 44100, "source_channels": 2, "silent_fraction": 0.01 },
  "cached": false
}
```

A failure is the same object with `status: "error"` and an `error` field
carrying a stable `kind` — never a crash, and never a missing row.

Batch input lines take the same shape as `query`, including an optional
`client_ref` that is echoed back untouched: selecta keys its library on
Music.app persistent IDs, which mean nothing to the store, so that field is how
a result gets matched back to a row.

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

Key comes out in both standard notation and Camelot (`"F minor"`, `"4A"`),
correlated against EDM-weighted profiles by default;
`--key-profile krumhansl` switches to the classical Krumhansl-Schmuckler set.

Every estimate carries a 0-1 confidence. Previews are sometimes a beatless intro
or a breakdown, and a low confidence score is the estimator telling you so.

## License

MIT
