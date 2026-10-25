# Resolution fixtures

Recorded-shape responses for the iTunes Search API, replayed by the tests in
`src/resolve.rs` so that matching is covered without network access.

These were hand-authored against the documented response schema rather than
captured from a live call — the environment this was built in has no route to
`itunes.apple.com`. Field names, nesting and types match what the API returns;
the values are chosen to exercise specific matching cases (a neutral qualifier,
a remix competing with its original, an entry with no `previewUrl`, an empty
result set). If you capture real responses later, replacing these files should
require no code change.

# Decode fixtures

`hot_noise.mp3` is one second of mono white noise boosted 10 dB past full
scale, encoded by ffmpeg with no tags. MP3 synthesis overshoots [-1, 1] on it,
so `src/decode.rs` tests that the decoder clamps as symphonia 0.5 did.
