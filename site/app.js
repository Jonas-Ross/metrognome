import { LOOP, synthLoop } from "./synth.js";

const SR = 44100;
// metrognome is tuned and validated on 30-second preview clips.
const WINDOW_SECS = 30;
// A trailing window shorter than this holds too few bars to say much.
const MIN_TAIL_SECS = 12;
const NOTES = ["C", "Db", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B"];

// The loop's real output from this same build, so the page reads complete
// before the engine has loaded. It is re-run live on load.
const EXAMPLE = {"algorithm_version":10,"features":{"tempo":{"bpm":124.01,"confidence":0.958,"uncertain":false,"maturity":"validated","source":"metrognome/onset-autocorrelation-comb@3","beat_offset_secs":0.491,"canonical_window_bpm":[90.0,180.0],"alternates":[{"value":62.0,"relation":"half","score":2.043},{"value":248.01,"relation":"double","score":3.147},{"value":165.37,"relation":"runner_up","score":-5.228}]},"key":{"key":"F minor","tonic":"F","mode":"minor","camelot":"4A","confidence":0.858,"uncertain":false,"maturity":"provisional","source":"metrognome/chroma-correlation-edm@3","alternates":[{"value":7.0,"label":"F major (7B)","relation":"parallel","score":0.681},{"value":4.0,"label":"Ab major (4B)","relation":"relative_major","score":0.536},{"value":5.0,"label":"C minor (5A)","relation":"dominant","score":0.532}],"scoring":{"correlation":0.8486,"runner_up":0.6811,"salience":0.4955,"tonal_pitch_classes":6.488,"strength":0.9966,"margin":0.8611,"structure":1.0,"coverage":1.0}}},"chroma":[1.0,0.413,0.338,0.354,0.298,0.896,0.224,0.35,0.676,0.356,0.578,0.318]};

const $ = (id) => document.getElementById(id);
const el = (tag, props = {}, ...kids) => {
  const n = Object.assign(document.createElement(tag), props);
  n.append(...kids);
  return n;
};

// ---- engine ---------------------------------------------------------------

let worker = null;
let nextId = 0;
const waiting = new Map();
function engine() {
  if (!worker) {
    worker = new Worker(new URL("worker.js", import.meta.url));
    worker.onmessage = ({ data }) => {
      const w = waiting.get(data.id);
      waiting.delete(data.id);
      data.ok ? w.resolve(data) : w.reject(new Error(data.error));
    };
    worker.onerror = (e) => {
      for (const w of waiting.values()) w.reject(new Error(e.message || "The analysis engine failed to load."));
      waiting.clear();
    };
  }
  return worker;
}
function analyze(samples, krumhansl) {
  const id = nextId++;
  const copy = samples.slice();
  return new Promise((resolve, reject) => {
    waiting.set(id, { resolve, reject });
    engine().postMessage({ id, samples: copy, sampleRate: SR, krumhansl }, [copy.buffer]);
  });
}

async function decodeFile(file) {
  const bytes = await file.arrayBuffer();
  const Ctx = window.OfflineAudioContext || window.webkitOfflineAudioContext;
  // decodeAudioData resamples to the context's rate, so everything reaches the
  // engine at 44.1 kHz whatever the file was.
  const buf = await new Ctx(1, 1, SR).decodeAudioData(bytes);
  const mono = new Float32Array(buf.length);
  for (let c = 0; c < buf.numberOfChannels; c++) {
    const ch = buf.getChannelData(c);
    for (let i = 0; i < ch.length; i++) mono[i] += ch[i] / buf.numberOfChannels;
  }
  return mono;
}

function windows(len) {
  const w = WINDOW_SECS * SR;
  if (len <= w) return [{ start: 0, end: len }];
  const out = [];
  for (let s = 0; s < len; s += w) {
    const end = Math.min(s + w, len);
    if (end - s >= MIN_TAIL_SECS * SR || out.length === 0) out.push({ start: s, end });
  }
  return out;
}
function headline(len) {
  const w = WINDOW_SECS * SR;
  if (len <= w) return { start: 0, end: len };
  const start = Math.floor((len - w) / 2);
  return { start, end: start + w };
}

// ---- state ----------------------------------------------------------------

const state = { samples: null, name: "", head: null, report: null, run: 0, selected: null };
const readout = document.querySelector(".readout");

async function load(samples, name) {
  stop();
  const run = ++state.run;
  state.samples = samples;
  state.name = name;
  state.head = headline(samples.length);
  $("now").textContent = `${name} · ${fmtTime(samples.length / SR)}`;
  readout.classList.add("busy");
  $("play").disabled = true;
  const krumhansl = $("profile").value === "krumhansl";

  status(`Listening to ${state.head.end - state.head.start < samples.length ? "the middle 30 seconds" : "the clip"}…`);
  try {
    const { report, ms } = await analyze(samples.subarray(state.head.start, state.head.end), krumhansl);
    if (run !== state.run) return;
    state.report = report;
    render(report);
    readout.classList.remove("busy");
    $("play").disabled = false;
    $("build").textContent = `metrognome algorithm ${report.algorithm_version} · WebAssembly · last read took ${Math.round(ms)} ms in this tab`;

    const wins = windows(samples.length);
    const items = timelineSkeleton(wins);
    for (let i = 0; i < wins.length; i++) {
      if (run !== state.run) return;
      if (wins.length > 1) status(`Reading the rest of the track`, (i + 1) / wins.length);
      const r = await analyze(samples.subarray(wins[i].start, wins[i].end), krumhansl);
      if (run !== state.run) return;
      fillWindow(items[i], wins[i], r.report);
    }
    status(`Done. ${fmtTime(samples.length / SR)} analyzed, 0 bytes uploaded.`);
  } catch (err) {
    if (run !== state.run) return;
    readout.classList.remove("busy");
    status(`Could not analyze this: ${err.message}`, null, true);
  }
}

function status(text, frac = null, err = false) {
  const s = $("status");
  s.classList.toggle("err", err);
  s.textContent = text;
  if (frac != null) {
    const p = el("span", { className: "prog" }, el("i"));
    p.firstChild.style.width = `${Math.round(frac * 100)}%`;
    s.append(p);
  }
}

// ---- rendering --------------------------------------------------------------

const fmtTime = (s) => `${Math.floor(s / 60)}:${String(Math.floor(s % 60)).padStart(2, "0")}`;
const pct = (v) => `${Math.round(Math.max(0, Math.min(1, v)) * 100)}%`;

function render(report) {
  const { tempo, key } = report.features;

  $("tempo-card").classList.toggle("guess", !tempo || tempo.uncertain);
  $("tempo-flag").hidden = !!tempo && !tempo.uncertain;
  $("tempo-flag").textContent = tempo ? "Hint only. Confidence is at or below 0.5, so treat this as a guess." : "No steady beat found in this audio.";
  $("bpm").textContent = tempo ? tempo.bpm.toFixed(1) : "—";
  $("tempo-conf").textContent = tempo ? tempo.confidence.toFixed(2) : "—";
  $("tempo-conf-bar").style.width = pct(tempo ? tempo.confidence : 0);
  $("tempo-maturity").textContent = tempo?.maturity === "validated" ? "Validated" : "Provisional";
  $("platter-bpm").textContent = tempo ? Math.round(tempo.bpm) : "?";
  document.querySelector(".platter").style.setProperty("--spin", `${tempo ? 60 / tempo.bpm : 0.5}s`);
  const relName = { half: "half", double: "double", runner_up: "next best" };
  $("tempo-alts").replaceChildren(
    ...(tempo?.alternates ?? []).map((a) => el("li", {}, el("b", { textContent: relName[a.relation] ?? a.relation.replace("_", " ") }), ` ${a.value.toFixed(1)}`)),
  );

  $("key-card").classList.toggle("guess", !key || key.uncertain);
  $("key-flag").hidden = !!key && !key.uncertain;
  $("key-flag").textContent = key ? "Hint only. Confidence is at or below 0.5, so treat this as a guess." : "Not enough tonal material to name a key.";
  $("keyname").textContent = key ? key.key : "—";
  $("camelot").textContent = key ? key.camelot : "";
  $("key-conf").textContent = key ? key.confidence.toFixed(2) : "—";
  $("key-conf-bar").style.width = pct(key ? key.confidence : 0);
  const relKey = { relative_major: "relative", relative_minor: "relative", parallel: "parallel", dominant: "fifth up", subdominant: "fifth down" };
  $("key-alts").replaceChildren(
    ...(key?.alternates ?? []).map((a) => el("li", {}, el("b", { textContent: relKey[a.relation] ?? a.relation.replace("_", " ") }), ` ${a.label ?? a.value}`)),
  );
  renderFactors(key?.scoring);

  state.selected = key?.camelot ?? null;
  drawWheel(key);
  renderChroma(report.chroma, key);
}

const FACTORS = [
  ["strength", "Fit", "How well the notes match any key at all."],
  ["margin", "Lead", "How far the winner is ahead of the next best key."],
  ["structure", "Shape", "Whether the notes have a tonal shape, not flat noise."],
  ["coverage", "Range", "Whether enough different notes are present to choose."],
];
function renderFactors(s) {
  const dl = $("factors");
  if (!s) return dl.replaceChildren();
  dl.replaceChildren(
    ...FACTORS.flatMap(([k, name, why]) => {
      const bar = el("span", { className: "bar" }, el("i"));
      bar.firstChild.style.width = pct(s[k]);
      return [el("dt", { textContent: name }), el("dd", {}, bar, el("span", { className: "v", textContent: s[k].toFixed(2) })), el("dd", {}, el("span", { className: "why", textContent: why }))];
    }),
    el("dd", {}, el("span", { className: "why", textContent: "Confidence is these four multiplied, so the smallest one explains a low score." })),
  );
}

// ---- Camelot wheel ----------------------------------------------------------

const majorPc = (n) => (((n - 8) * 7) % 12 + 12) % 12;
const camelotName = (n, ring) => (ring === "B" ? `${NOTES[majorPc(n)]} major` : `${NOTES[(majorPc(n) + 9) % 12]} minor`);
const parseCamelot = (c) => (c ? { n: parseInt(c, 10), ring: c.slice(-1) } : null);
const wheelStep = (n, d) => ((n - 1 + d + 12) % 12) + 1;
function neighbours(c) {
  const { n, ring } = parseCamelot(c);
  return [`${wheelStep(n, -1)}${ring}`, `${wheelStep(n, 1)}${ring}`, `${n}${ring === "A" ? "B" : "A"}`];
}

function arc(r0, r1, a0, a1) {
  const p = (r, a) => `${(r * Math.sin(a)).toFixed(2)} ${(-r * Math.cos(a)).toFixed(2)}`;
  return `M${p(r1, a0)} A${r1} ${r1} 0 0 1 ${p(r1, a1)} L${p(r0, a1)} A${r0} ${r0} 0 0 0 ${p(r0, a0)}Z`;
}

function drawWheel(key) {
  const svg = $("wheel");
  const NS = "http://www.w3.org/2000/svg";
  const detected = key?.camelot;
  const sel = state.selected;
  const near = sel ? neighbours(sel) : [];
  svg.querySelectorAll("g").forEach((g) => g.remove());
  const g = document.createElementNS(NS, "g");
  for (const ring of ["B", "A"]) {
    const [r0, r1] = ring === "B" ? [104, 154] : [54, 102];
    for (let n = 1; n <= 12; n++) {
      const c = `${n}${ring}`;
      const a = (n * Math.PI) / 6;
      const path = document.createElementNS(NS, "path");
      path.setAttribute("d", arc(r0, r1, a - Math.PI / 12, a + Math.PI / 12));
      let fill = "var(--paper)", ink = "var(--ink)";
      if (c === sel) [fill, ink] = [c === detected && key.uncertain ? "var(--warn)" : "var(--pink)", "#fff"];
      else if (near.includes(c)) [fill, ink] = ["var(--blue)", "#fff"];
      path.setAttribute("style", `fill:${fill}`);
      path.setAttribute("tabindex", "0");
      path.setAttribute("role", "button");
      path.setAttribute("aria-label", `${c}, ${camelotName(n, ring)}`);
      const pick = () => { state.selected = c; drawWheel(key); };
      path.addEventListener("click", pick);
      path.addEventListener("keydown", (e) => (e.key === "Enter" || e.key === " ") && (e.preventDefault(), pick()));
      g.append(path);
      const rm = (r0 + r1) / 2;
      const t = document.createElementNS(NS, "text");
      t.setAttribute("x", (rm * Math.sin(a)).toFixed(1));
      t.setAttribute("y", (-rm * Math.cos(a) + 4).toFixed(1));
      t.setAttribute("text-anchor", "middle");
      t.setAttribute("class", "num");
      t.setAttribute("style", `fill:${ink}`);
      t.textContent = c + (c === detected ? "•" : "");
      g.append(t);
    }
  }
  svg.append(g);

  if (!sel) {
    $("wheel-note").textContent = "No key to place. Tap any key to see what it mixes with.";
    return;
  }
  const { n, ring } = parseCamelot(sel);
  const names = near.map((c) => { const p = parseCamelot(c); return `${c} ${camelotName(p.n, p.ring)}`; });
  const who = sel === detected ? `This track, ${sel} ${camelotName(n, ring)},` : `${sel} ${camelotName(n, ring)}`;
  $("wheel-note").textContent = `DJs number keys like a clock. ${who} blends smoothly into ${names[0]}, ${names[1]} or ${names[2]}: they share six of their seven notes, so nothing clashes during a mix.`;
}

function renderChroma(chroma, key) {
  let inScale = new Set();
  if (key) {
    const t = NOTES.indexOf(key.tonic);
    const steps = key.mode === "minor" ? [0, 2, 3, 5, 7, 8, 10] : [0, 2, 4, 5, 7, 9, 11];
    inScale = new Set(steps.map((s) => (t + s) % 12));
  }
  $("chroma").replaceChildren(
    ...chroma.map((v, i) => {
      const bar = el("i");
      bar.style.height = `${Math.max(1, v * 100)}%`;
      return el("div", { className: inScale.has(i) ? "in" : "", title: `${NOTES[i]}: ${v.toFixed(2)}` }, bar, el("span", { textContent: NOTES[i] }));
    }),
  );
}

// ---- timeline -------------------------------------------------------------

function timelineSkeleton(wins) {
  const list = $("timeline");
  $("timeline-hint").textContent = wins.length > 1 ? `${wins.length} windows of 30 seconds` : "one window";
  const items = wins.map((w) => el("li", { className: "pending" }, el("span", { className: "t", textContent: `${fmtTime(w.start / SR)}–${fmtTime(w.end / SR)}` }), el("span", { className: "b", textContent: "···" }), el("span", { className: "k", textContent: " " })));
  list.replaceChildren(...items);
  return items;
}
function fillWindow(li, w, report) {
  const { tempo, key } = report.features;
  li.classList.remove("pending");
  const h = state.head;
  li.classList.toggle("head", w.start <= h.start + (h.end - h.start) / 2 && h.start + (h.end - h.start) / 2 < w.end);
  const b = li.querySelector(".b"), k = li.querySelector(".k");
  b.textContent = tempo ? Math.round(tempo.bpm) : "—";
  b.classList.toggle("lo", !tempo || tempo.uncertain);
  k.textContent = key ? `${key.camelot} ${key.tonic}${key.mode === "minor" ? "m" : ""}` : "no key";
  k.classList.toggle("lo", !key || key.uncertain);
}

// ---- playback with a click on the detected grid ----------------------------

let playing = null;
function stop() {
  if (!playing) return;
  playing.ctx.close();
  cancelAnimationFrame(playing.raf);
  playing = null;
  $("play").textContent = "Play with click";
  document.querySelector(".platter").classList.remove("spinning");
}
function play() {
  if (playing) return stop();
  const tempo = state.report?.features.tempo;
  if (!state.samples) return;
  const ctx = new (window.AudioContext || window.webkitAudioContext)();
  const { start, end } = state.head;
  const buf = ctx.createBuffer(1, end - start, SR);
  buf.copyToChannel(state.samples.subarray(start, end), 0);
  const src = ctx.createBufferSource();
  src.buffer = buf;
  src.connect(ctx.destination);
  const t0 = ctx.currentTime + 0.1;
  src.start(t0);
  src.onended = stop;

  const beats = [];
  if (tempo) {
    const period = 60 / tempo.bpm;
    for (let t = tempo.beat_offset_secs % period; t < buf.duration; t += period) {
      beats.push(t0 + t);
      const o = ctx.createOscillator(), g = ctx.createGain();
      o.frequency.value = 1760;
      g.gain.setValueAtTime(0.0001, t0 + t);
      g.gain.exponentialRampToValueAtTime(0.35, t0 + t + 0.002);
      g.gain.exponentialRampToValueAtTime(0.0001, t0 + t + 0.05);
      o.connect(g).connect(ctx.destination);
      o.start(t0 + t);
      o.stop(t0 + t + 0.06);
    }
  }
  const platter = document.querySelector(".platter");
  platter.classList.add("spinning");
  let i = 0;
  const tick = () => {
    while (i < beats.length && ctx.currentTime >= beats[i]) {
      i++;
      platter.classList.remove("beat");
      void platter.offsetWidth;
      platter.classList.add("beat");
    }
    playing.raf = requestAnimationFrame(tick);
  };
  playing = { ctx, raf: 0 };
  playing.raf = requestAnimationFrame(tick);
  $("play").textContent = "Stop";
}

// ---- wiring ----------------------------------------------------------------

async function takeFile(file) {
  if (!file) return;
  stop();
  const run = ++state.run;
  status(`Decoding ${file.name}…`);
  try {
    const samples = await decodeFile(file);
    // Another file or the loop was picked while this one decoded.
    if (run !== state.run) return;
    if (samples.length < SR * 5) throw new Error("it is shorter than five seconds");
    await load(samples, file.name);
  } catch (err) {
    if (run !== state.run) return;
    status(`Could not read ${file.name}: ${err.message || "your browser cannot decode this format"}. Try an MP3, AAC or WAV file.`, null, true);
  }
}

const drop = $("drop");
$("file").addEventListener("change", (e) => takeFile(e.target.files[0]));
drop.addEventListener("dragover", (e) => { e.preventDefault(); drop.classList.add("over"); });
drop.addEventListener("dragleave", () => drop.classList.remove("over"));
drop.addEventListener("drop", (e) => { e.preventDefault(); drop.classList.remove("over"); takeFile(e.dataTransfer.files[0]); });
$("play").addEventListener("click", play);
$("loop").addEventListener("click", () => load(synthLoop(SR), `Example: built-in ${LOOP.bpm} BPM loop in ${LOOP.key}`));
$("profile").addEventListener("change", () => state.samples && load(state.samples, state.name));

state.samples = synthLoop(SR);
state.name = `Example: built-in ${LOOP.bpm} BPM loop in ${LOOP.key}`;
state.head = headline(state.samples.length);
state.report = EXAMPLE;
render(EXAMPLE);
load(state.samples, state.name);
