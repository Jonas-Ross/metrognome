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

metrognome selftest   # accuracy check against synthesized audio, no network
metrognome validate   # accuracy check against known tracks, needs network
```

- `analyze` prints exactly one JSON object on stdout.
- `batch` reads one JSON object per line on stdin and writes one result per
  input line, in input order. A track that fails produces an error result, not a
  crash — one bad row never kills the run.
- **stdout carries nothing but JSON.** Logs go to stderr, always.
- Results are cached on disk by resolved iTunes store track ID, so a track is
  analyzed once. Resolutions are cached too — that is the rate-limited step.
  `--cache-path` moves the database, `--no-cache` bypasses it.

## Output

Every object carries `schema_version`, and every feature carries its own
`source` and `confidence` plus an `uncertain` flag, so a consumer can tell a
measurement from a guess. It also carries `maturity`, either `"validated"` or
`"provisional"`: confidence is how sure the estimator is about this clip,
maturity is whether the estimator itself has been checked against real
recordings. Read both — a provisional feature can be confidently wrong. Features
live under `features`, which grows by adding optional fields rather than by
moving existing ones.

```json
{
  "schema_version": 2,
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
    "key": {
      "key": "A minor",
      "tonic": "A",
      "mode": "minor",
      "camelot": "8A",
      "confidence": 0.79,
      "uncertain": false,
      "maturity": "provisional",
      "source": "metrognome/chroma-correlation-edm@1",
      "alternates": [
        { "value": 8.0, "label": "C major (8B)", "relation": "relative_major", "score": 0.681 }
      ]
    },
    "tempo": {
      "bpm": 121.0,
      "confidence": 0.86,
      "uncertain": false,
      "maturity": "validated",
      "source": "metrognome/onset-autocorrelation-comb@1",
      "beat_offset_secs": 0.496,
      "canonical_window_bpm": [90.0, 180.0],
      "alternates": [{ "value": 60.5, "relation": "half", "score": 5.109 }]
    }
  },
  "audio": { "duration_secs": 30.0, "sample_rate": 44100, "source_channels": 2, "silent_fraction": 0.01 },
  "cached": false
}
```

A failure is the same object with `status: "error"` and an `error` field
carrying a stable `kind` — never a crash, and never a missing row. That holds
for every line `batch` emits, including one whose input was not valid JSON: it
comes back as a full result object with an empty `query`, so a consumer can
deserialize every line into one type without special-casing the bad row.

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

**Tempo is validated; key is provisional.** They are not equally trustworthy and
the output should not be read as though they were. Tempo is checked against
published figures that agree across sources, and every case with a verified
reference passes. Key has no comparable reference: published key data
contradicts itself — the same track is listed in different keys by the same
source — so there is nothing to measure against. What is verified is that key
estimation recovers all twenty-four keys from unambiguous synthetic material on
both profile sets, which rules out a systematic rotation but says nothing about
real recordings.

That split is in the output, not just here: tempo carries
`"maturity": "validated"` and key carries `"maturity": "provisional"`, so a
consumer branches on the field rather than hardcoding which feature to trust.
Consume key through its `confidence` and `uncertain` fields rather than as a
fact. `metrognome validate` reflects this: a key disagreement is printed and
diagnosed, but only tempo decides the exit status.

Every estimate carries a 0-1 confidence. Previews are sometimes a beatless intro
or a breakdown, and a low confidence score is the estimator telling you so.

For key, confidence measures how determined the key is, not how neatly a profile
fits. A clip has to state enough distinct pitch classes to choose between keys:
a riff on two or three, or a run of fifths with no third in them, scores near
zero however cleanly it correlates. Confidence does not reach 1.0 — chroma
correlation over a 30-second clip cannot earn certainty.

Two commands check accuracy, and both print a markdown table to **stderr** with
a machine-readable report on stdout:

- `metrognome selftest` runs against synthesized audio carrying the traps each
  idiom actually has — offbeat hats that read as double time, a breakbeat snare
  period that reads as half time. No network, and it gates CI.
- `metrognome validate` runs the same check against ten well-known electronic
  releases with widely documented tempos, spanning house, techno and drum &
  bass. This is the one that tells you whether octave handling survives contact
  with real recordings.

```
$ metrognome selftest
| Track | Genre | Expected BPM | Estimated BPM | Verdict | Tempo conf. | ... |
| synthetic four-on-the-floor 124 | house       | 124 | 124.00 | ok |  1.00 | ... |
| synthetic breakbeat 174         | drum & bass | 174 | 174.00 | ok |  0.98 | ... |
```

## License

MIT
