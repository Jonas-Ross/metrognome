// A built-in loop so the demo works before anyone drops a file: four-on-the-floor
// at 124 BPM over an F minor i-VI-VII-i progression, the same shape metrognome's
// own tests use.
export const LOOP = { bpm: 124, key: "F minor", camelot: "4A", secs: 24 };

export function synthLoop(sr = 44100) {
  const { bpm, secs } = LOOP;
  const n = Math.floor(secs * sr);
  const out = new Float32Array(n);
  const beat = (60 / bpm) * sr;
  let seed = 0x2545f491;
  const rand = () => ((seed = (seed * 1664525 + 1013904223) >>> 0) / 2 ** 32) * 2 - 1;

  const add = (at, len, fn) => {
    for (let i = 0; i < len && at + i < n; i++) out[at + i] += fn(i / sr);
  };
  const kick = (t) => Math.sin(2 * Math.PI * (50 * t + 90 * (1 - Math.exp(-t * 30)) / 30)) * Math.exp(-t * 9) * 0.9 + rand() * Math.exp(-t * 400) * 0.3;
  const hat = (t) => rand() * Math.exp(-t * 60) * 0.18;
  const clap = (t) => rand() * Math.exp(-t * 22) * 0.3;

  for (let b = 0; b * beat < n; b++) {
    const at = Math.round(b * beat);
    add(at, sr * 0.3, kick);
    add(Math.round(at + beat / 2), sr * 0.08, hat);
    if (b % 2 === 1) add(at, sr * 0.2, clap);
  }

  // F minor, Db major, Eb major, F minor: one chord per bar.
  const chords = [[53, 56, 60], [49, 53, 56], [51, 55, 58], [53, 56, 60]];
  const bar = beat * 4;
  for (let c = 0; c * bar < n; c++) {
    const notes = chords[c % 4];
    const at = Math.round(c * bar);
    const len = Math.min(Math.round(bar), n - at);
    for (const m of notes.concat(notes[0] - 12)) {
      const f = 440 * 2 ** ((m - 69) / 12);
      for (let i = 0; i < len; i++) {
        const t = i / sr;
        const env = Math.min(1, t * 20) * Math.exp(-t * 0.6);
        out[at + i] += (Math.sin(2 * Math.PI * f * t) + 0.3 * Math.sin(4 * Math.PI * f * t)) * env * 0.09;
      }
    }
  }
  return out;
}
