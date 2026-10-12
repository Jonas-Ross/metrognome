"""Write the key_bench manifest from the two GiantSteps dataset checkouts.

usage: manifest.py <giantsteps-key-dataset> <giantsteps-mtg-key-dataset> > manifest.tsv
"""
import os
import re
import sys

PC = {"C": 0, "C#": 1, "Db": 1, "D": 2, "D#": 3, "Eb": 3, "E": 4, "F": 5, "F#": 6,
      "Gb": 6, "G": 7, "G#": 8, "Ab": 8, "A": 9, "A#": 10, "Bb": 10, "B": 11}


def parse(label):
    m = re.fullmatch(r"\s*([A-G][#b]?)\s+(major|minor)\s*", label)
    return (PC[m.group(1)], m.group(2)) if m else None


gs, mtg = sys.argv[1:3]
keys = os.path.join(gs, "annotations", "key")
for name in sorted(os.listdir(keys)):
    key = parse(open(os.path.join(keys, name)).read())
    if key:
        track = name.split(".")[0]
        url = f"https://www.cp.jku.at/datasets/giantsteps/backup/{track}.LOFI.mp3"
        print("gs", track, url, *key, sep="\t")
# Only single-key tracks the annotators marked confident (column C = 2).
for line in open(os.path.join(mtg, "annotations", "annotations.txt")).read().splitlines()[1:]:
    cols = line.split("\t")
    key = parse(cols[1]) if len(cols) >= 3 and cols[2].strip() == "2" else None
    if key:
        url = f"https://www.cp.jku.at/datasets/giantsteps/mtg_key_backup/{cols[0]}.LOFI.mp3"
        print("mtg", cols[0], url, *key, sep="\t")
