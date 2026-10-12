# keyfit

How the band key scorer's weights (`crates/metrognome/src/key/weights.rs`) are
measured and fitted. Not part of the build or the test suite: it needs the
network and about 3 GB of downloads, none of it kept.

The ground truth is the two GiantSteps key datasets, 2-minute Beatport previews
of electronic dance music with keys labelled by people:

- [giantsteps-key-dataset](https://github.com/GiantSteps/giantsteps-key-dataset), 604 tracks
- [giantsteps-mtg-key-dataset](https://github.com/GiantSteps/giantsteps-mtg-key-dataset),
  1,159 tracks with one confidently labelled key

```sh
git clone --depth 1 https://github.com/GiantSteps/giantsteps-key-dataset /tmp/gs
git clone --depth 1 https://github.com/GiantSteps/giantsteps-mtg-key-dataset /tmp/gsm
python3 tools/keyfit/manifest.py /tmp/gs /tmp/gsm > /tmp/manifest.tsv
cargo build --release --example key_bench
target/release/examples/key_bench < /tmp/manifest.tsv > /tmp/bench.jsonl   # ~10 min, progress on stderr
python3 tools/keyfit/fit.py /tmp/bench.jsonl                                # held-out numbers
python3 tools/keyfit/fit.py /tmp/bench.jsonl --write crates/metrognome/src/key/weights.rs
```

`key_bench` streams each clip into memory, keeps the middle 30 seconds as a
stand-in for a store preview, and prints the band features plus every scorer's
estimate. `fit.py` trains on one dataset and tests on the other, both ways, so
the numbers it prints are on tracks the weights never saw; `--write` then fits
on both. A refit moves key output, so it bumps `KEY_SCORER_VERSION` and
`ALGORITHM_VERSION` like any other key change.
