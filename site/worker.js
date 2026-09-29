// Runs metrognome off the main thread. The wasm module sees mono PCM and a
// sample rate, nothing else; this file only moves samples in and JSON out.
const ready = fetch(new URL("metrognome.wasm", self.location.href))
  .then((r) => {
    if (!r.ok) throw new Error(`metrognome.wasm: HTTP ${r.status}`);
    return r.arrayBuffer();
  })
  .then((bytes) => WebAssembly.instantiate(bytes, {}))
  .then(({ instance }) => instance.exports);

self.onmessage = async ({ data }) => {
  const { id, samples, sampleRate, krumhansl } = data;
  try {
    const x = await ready;
    const ptr = x.mg_alloc(samples.length);
    new Float32Array(x.memory.buffer, ptr, samples.length).set(samples);
    const t0 = performance.now();
    const len = x.mg_analyze(ptr, samples.length, sampleRate, krumhansl ? 1 : 0);
    const ms = performance.now() - t0;
    const json = new TextDecoder().decode(new Uint8Array(x.memory.buffer, x.mg_result_ptr(), len).slice());
    x.mg_free(ptr, samples.length);
    self.postMessage({ id, ok: true, ms, report: JSON.parse(json) });
  } catch (err) {
    self.postMessage({ id, ok: false, error: String(err && err.message ? err.message : err) });
  }
};
