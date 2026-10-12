"""Fit the band scorer's weights from key_bench output and measure them.

usage: fit.py bench.jsonl [--write path/to/weights.rs]

Evaluates train-on-one-dataset, test-on-the-other both ways, then fits on both
and, with --write, emits the Rust table. numpy only.
"""
import json
import sys

import numpy as np

L2 = 1e-2  # Measured flat from 1e-4 to 1e-2 on held-out tracks; the larger one calibrates best.
ITERS = 2000
LR = 0.05

rows = [json.loads(line) for line in open(sys.argv[1])]
rows = [r for r in rows if "error" not in r]
F = np.array([r["bands"] for r in rows], dtype=np.float64)  # N x 5 x 12
y = np.array([2 * r["tonic"] + (r["mode"] == "minor") for r in rows])
ds = np.array([r["dataset"] for r in rows])
gates = np.array([
    (e["scoring"]["structure"] * e["scoring"]["coverage"]) if (e := r["estimates"]["Bands"]) else 0.0
    for r in rows
])
N, B, _ = F.shape

# Z[n, k] is the design row for key k = 2*tonic + minor: the clip rotated so
# the tonic sits at index 0, in the slot for that mode.
Z = np.zeros((N, 24, 2, B, 12))
for t in range(12):
    r = np.roll(F, -t, axis=2)
    Z[:, 2 * t, 0] = r
    Z[:, 2 * t + 1, 1] = r
Z = Z.reshape(N, 24, -1)
MODE = np.tile([0, 1], 12)


def probs(Zs, w, b):
    s = Zs @ w + b[MODE]
    s -= s.max(1, keepdims=True)
    p = np.exp(s)
    return p / p.sum(1, keepdims=True)


def fit(mask):
    Zs, ys, n = Z[mask], y[mask], mask.sum()
    th = np.zeros(Z.shape[2] + 2)
    m1 = np.zeros_like(th)
    m2 = np.zeros_like(th)
    for it in range(1, ITERS + 1):
        w, b = th[:-2], th[-2:]
        g = probs(Zs, w, b)
        g[np.arange(n), ys] -= 1
        grad = np.concatenate([
            np.einsum("nk,nkp->p", g, Zs) / n + L2 * w,
            [g[:, 0::2].sum() / n, g[:, 1::2].sum() / n],
        ])
        m1 = 0.9 * m1 + 0.1 * grad
        m2 = 0.999 * m2 + 0.001 * grad * grad
        th -= LR * (m1 / (1 - 0.9**it)) / (np.sqrt(m2 / (1 - 0.999**it)) + 1e-8)
    return th[:-2], th[-2:]


def relation(est, truth):
    if est == truth:
        return "exact"
    iv, em, tm = (est // 2 - truth // 2) % 12, est % 2, truth % 2
    if em == tm and iv in (5, 7):
        return "fifth"
    if (tm, em, iv) in ((0, 1, 9), (1, 0, 3)):
        return "relative"
    if em != tm and iv == 0:
        return "parallel"
    return "other"


MIREX = {"exact": 1, "fifth": 0.5, "relative": 0.3, "parallel": 0.2, "other": 0}


def line(name, est, conf, truth):
    rel = [relation(e, t) for e, t in zip(est, truth)]
    exact = np.array([x == "exact" for x in rel])
    keep = conf > 0.5
    return (f"  {name:10s} exact {exact.mean():.3f}  mirex {np.mean([MIREX[x] for x in rel]):.3f}"
            f"  | confident: {keep.mean():.3f} of tracks, {exact[keep].mean() if keep.any() else 0:.3f} right")


def parse_key(e):
    names = ["C", "Db", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B"]
    if not e:
        return -1, 0.0
    t, m = e["key"].split()
    return 2 * names.index(t) + (m == "minor"), e["confidence"]


for train, test in (("mtg", "gs"), ("gs", "mtg")):
    w, b = fit(ds == train)
    sel = ds == test
    p = probs(Z[sel], w, b)
    est, top = p.argmax(1), p.max(1)
    conf = top * gates[sel]
    print(f"train {train} ({(ds == train).sum()}), test {test} ({sel.sum()}):")
    print(line("bands", est, conf, y[sel]))
    for prof in ("Edm", "Krumhansl"):
        pk = [parse_key(r["estimates"][prof]) for r, s in zip(rows, sel) if s]
        print(line(prof.lower(), np.array([k for k, _ in pk]), np.array([c for _, c in pk]), y[sel]))
    ok = est == y[sel]
    print("  calibration (confidence band: share right):",
          ", ".join(f"{lo:.1f}-{lo + .2:.1f}: {ok[(conf > lo) & (conf <= lo + .2)].mean():.2f}"
                    for lo in np.arange(0, 1, .2) if ((conf > lo) & (conf <= lo + .2)).sum() >= 10))

if "--write" in sys.argv:
    w, b = fit(np.ones(N, bool))
    w = w.reshape(2, B, 12)
    fmt = lambda v: f"{v:.4f}"
    body = ",\n".join(
        "    [\n" + ",\n".join("        [" + ", ".join(fmt(x) for x in band) + "]" for band in mode) + ",\n    ]"
        for mode in w)
    out = f'''//! Learned weights for the band scorer, fitted on {N} labelled tracks by
//! `tools/keyfit/fit.py`. Refit there rather than editing by hand.

/// Per-mode additive prior, major then minor.
pub const MODE_BIAS: [f32; 2] = [{fmt(b[0])}, {fmt(b[1])}];

/// Per mode, per register, a weight for each scale degree above the tonic.
// Fitted numbers that land near a math constant are not that constant.
#[allow(clippy::approx_constant)]
#[rustfmt::skip]
pub const PROFILES: [[[f32; 12]; {B}]; 2] = [
{body},
];
'''
    open(sys.argv[sys.argv.index("--write") + 1], "w").write(out)
