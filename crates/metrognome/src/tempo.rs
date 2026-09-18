//! Tempo estimation.
//!
//! Three stages, each doing one job:
//!
//! 1. **Candidates** come from the autocorrelation of the onset envelope. The
//!    ACF is good at spotting *a* periodicity and bad at choosing which of a
//!    tempo's octaves is the real one, so it is used only to propose.
//! 2. **The canonical fold** collapses every candidate into one octave. This is
//!    where the 87-vs-174 problem is handled: within a single octave window an
//!    octave error cannot be expressed.
//! 3. **Scoring** is a phase-aligned comb filter over the whole clip. At the
//!    true tempo every beat slot lands on an onset; a 1% error accumulates into
//!    a visible misalignment over 80+ beats, which makes the score sharp enough
//!    to refine tempo to a fraction of a BPM.

use crate::dsp::{autocorrelation, interp_at, smooth, OnsetEnvelope};
use crate::types::{Alternate, TempoEstimate, UNCERTAIN_AT_OR_BELOW};

/// Identifier recorded on every tempo estimate.
pub const TEMPO_SOURCE: &str = "metrognome/onset-autocorrelation-comb@1";

/// Low edge (inclusive) of the canonical one-octave output window.
///
/// 90-180 is chosen to hold every idiom this is built for in a single octave:
/// house at 120-128, techno and trance at 130-145, drum & bass at 170-176. Any
/// candidate outside it is doubled or halved until it lands inside, which makes
/// an octave error structurally impossible. See DECISIONS.md for the cost.
pub const CANONICAL_LOW_BPM: f32 = 90.0;

/// High edge (exclusive) of the canonical output window.
pub const CANONICAL_HIGH_BPM: f32 = 180.0;

/// Widest tempo the autocorrelation search looks at.
///
/// Wider than the output window on purpose: the true tempo's half and double
/// are usually stronger ACF peaks than the tempo itself, and the fold needs
/// them to be found before it can bring them home.
const SEARCH_MIN_BPM: f32 = 45.0;

/// Fastest tempo the autocorrelation search looks at.
const SEARCH_MAX_BPM: f32 = 280.0;

/// Centre of the log-normal prior applied while picking ACF peaks.
///
/// Only biases which periodicities are *proposed*; no prior is applied once
/// candidates are inside the canonical window, because the window already
/// encodes the genre assumption and stacking a second one on top would quietly
/// pull every estimate toward 128.
const PRIOR_CENTRE_BPM: f32 = 128.0;

/// Width of that prior, in octaves. Wide enough that 174 and 87 are both
/// clearly admissible; narrow enough to suppress 40 and 300.
const PRIOR_SIGMA_OCTAVES: f32 = 0.9;

/// How many ACF peaks to expand into candidates.
const MAX_ACF_PEAKS: usize = 10;

/// Relative span searched around each coarse candidate during refinement.
const REFINE_SPAN: f32 = 0.04;

/// Coarse refinement step, expressed as the beat-drift it allows across the
/// whole clip. A tempo error of `e` puts the last beat `e * n_beats` beats out
/// of place; 1/16 of a beat is comfortably inside the smoothed comb peak.
const REFINE_DRIFT_BEATS: f32 = 1.0 / 16.0;

/// Width of the Hann kernel applied to the envelope before comb scoring.
///
/// 30 ms is roughly a drum transient plus the timing jitter of a
/// hand-programmed groove. Without it the comb score is unsearchable (see
/// [`crate::dsp::smooth`]); much wider and a 16th-note grid at 174 BPM starts
/// to blur into the beat grid it is supposed to be distinguished from.
const SCORING_SMOOTH_SECS: f32 = 0.030;

/// Relative span of the final precision pass, run on the unsmoothed envelope.
///
/// Only has to cover the error the smoothed search can make, which is bounded
/// by the smoothed peak's own width.
const PRECISION_SPAN: f32 = 0.005;

/// Precision-pass step, expressed as end-of-clip drift in envelope frames.
/// A fifth of a frame is finer than the envelope can resolve, so the step is
/// never what limits the reported BPM.
const PRECISION_DRIFT_FRAMES: f32 = 0.2;

/// How many coarse candidates get the precision pass and a final raw-envelope
/// score. The smoothed search ranks roughly and reliably; this is only about
/// giving the genuine contenders a fair, unsmoothed comparison.
const MAX_FINALISTS: usize = 6;

/// Ratios expanded from each ACF peak before folding.
///
/// Octave relatives are handled by the fold. These are the *metric* confusions
/// that survive it: a shuffle or a half-bar pattern can make the strongest
/// periodicity 2/3 or 3/4 of the real beat, and folding a wrong ratio just
/// yields a wrong in-window answer.
const CANDIDATE_RATIOS: [f32; 5] = [1.0, 3.0 / 2.0, 2.0 / 3.0, 4.0 / 3.0, 3.0 / 4.0];

/// Fold `bpm` into `[CANONICAL_LOW_BPM, CANONICAL_HIGH_BPM)` by octaves.
pub fn fold_to_canonical(bpm: f32) -> f32 {
    if !bpm.is_finite() || bpm <= 0.0 {
        return 0.0;
    }
    let mut b = bpm;
    while b < CANONICAL_LOW_BPM {
        b *= 2.0;
    }
    while b >= CANONICAL_HIGH_BPM {
        b /= 2.0;
    }
    b
}

fn prior(bpm: f32) -> f32 {
    let z = (bpm / PRIOR_CENTRE_BPM).log2() / PRIOR_SIGMA_OCTAVES;
    (-0.5 * z * z).exp()
}

/// Penalty on the spread of onset strength across beat positions.
///
/// The score is `mean - CONSISTENCY_PENALTY * stddev`, which at 1.0 reads as
/// "the level the weakest beats reach" rather than "the average beat level".
/// That distinction is what separates a real tempo from a metric decoy: a grid
/// at 2/3 or 4/5 of the true tempo can land on *something* every time — a
/// hi-hat instead of a kick — and score a high mean, but the alternation
/// between strong and weak hits shows up as spread. Measured against the
/// synthetic grooves, a mean-only score picks the decoy for a busy breakbeat;
/// at 1.0 every case separates with room to spare.
const CONSISTENCY_PENALTY: f32 = 1.0;

/// Mean and spread of onset strength on the best-aligned beat grid at `bpm`.
///
/// The envelope is zero-mean and unit-variance, so these read directly as
/// standard deviations above background at beat times: a clean
/// four-on-the-floor sits around 5, a wrong tempo near 0.
///
/// No tolerance window is applied around each beat. The target material is
/// sequenced electronic music, which is metronomic to well under a frame; a
/// tolerance window would only blur the discrimination that makes refinement
/// work.
fn comb_score(env: &[f32], fps: f32, bpm: f32) -> CombScore {
    let none = CombScore {
        score: 0.0,
        mean: 0.0,
        sd: 0.0,
        phase_frames: 0.0,
    };
    if bpm <= 0.0 || env.len() < 2 {
        return none;
    }
    let period = 60.0 * fps / bpm;
    if period < 2.0 || period as usize >= env.len() {
        return none;
    }

    // The envelope is z-scored, so it has negatives. Below-average frames are
    // quiet, not negative energy, so they clamp to zero for the recall sums.
    let total: f32 = env.iter().map(|v| v.max(0.0)).sum();

    // Half-frame phase steps: finer than the envelope's own resolution, so the
    // phase search never limits the score.
    // Phase is chosen by mean alone: the best phase is the one where the grid
    // sits on the most onset energy, which is the definition of beat alignment.
    // The consistency penalty then judges *that* alignment. Folding the penalty
    // into the phase search instead lets a candidate shop for a phase where its
    // beats happen to be uniformly mediocre, which scores well and means
    // nothing.
    let steps = (period * 2.0).ceil() as usize;
    let mut best = none;
    let mut best_mean = f32::MIN;
    for s in 0..steps {
        let phase = s as f32 * 0.5;
        // Count only beats that actually fall inside the envelope. Letting the
        // last one run off the end reads as a missing beat and penalizes fast
        // tempos for nothing.
        let n_beats = (((env.len() - 1) as f32 - phase) / period).floor() as usize + 1;
        if n_beats < 4 {
            continue;
        }
        let mut sum = 0.0f32;
        let mut sum_sq = 0.0f32;
        for k in 0..n_beats {
            let v = interp_at(env, phase + k as f32 * period);
            sum += v;
            sum_sq += v * v;
        }
        let n = n_beats as f32;
        let mean = sum / n;
        if mean > best_mean {
            best_mean = mean;
            let sd = (sum_sq / n - mean * mean).max(0.0).sqrt();
            // How strong this grid's own beats are, and how evenly. On real
            // music this is routinely negative — the spread between a strong
            // downbeat and a weak one exceeds the level itself — which is why
            // the miss penalty below is subtracted rather than multiplied in.
            // A multiplicative form has to clamp the negative case away, and
            // clamping collapses every candidate on a real track to the same
            // zero and leaves the ranking to sort order.
            let precision = mean - CONSISTENCY_PENALTY * sd;
            let missed = 1.0 - recall(env, fps, period, phase, total);
            let score = precision - MISS_PENALTY * missed;
            best = CombScore {
                score,
                mean,
                sd,
                phase_frames: phase,
            };
        }
    }
    best
}

/// Share of the envelope's onset energy that falls on this grid.
///
/// The other half of the trade that `precision` makes. Precision alone punishes
/// a grid that is too *fast*, because half its points land on nothing. Nothing
/// punished a grid that was too *slow*: a sparse grid is a subset of a dense
/// one's structure, so sampling every third beat of a real groove posts a high
/// mean and a tight spread precisely by skipping the beats that would have cost
/// it. Recall charges for those skipped beats, and the product is an ordinary
/// precision-recall balance.
///
/// Measured need: Sandstorm's preview reads 90.71 against a true 136.07 —
/// exactly 2/3 — with the true tempo already on the shortlist and losing.
fn recall(env: &[f32], fps: f32, period: f32, phase: f32, total: f32) -> f32 {
    if total <= 0.0 {
        return 0.0;
    }
    let half_width = EXPLAIN_HALF_WIDTH_SECS * fps;
    let mut captured = 0.0f32;
    for (i, v) in env.iter().enumerate() {
        let v = v.max(0.0);
        if v <= 0.0 {
            continue;
        }
        // Distance to the nearest grid line, in frames.
        let beats = (i as f32 - phase) / period;
        let dist = (beats - beats.round()).abs() * period;
        if dist <= half_width {
            captured += v;
        }
    }
    (captured / total).clamp(0.0, 1.0)
}

/// A beat grid's fit at one tempo.
#[derive(Debug, Clone, Copy)]
struct CombScore {
    /// Ranking score: precision less the miss penalty. Only ever compared
    /// against other candidates on the same audio.
    score: f32,
    /// Mean onset strength at this grid's beat positions, in standard
    /// deviations above background.
    ///
    /// Confidence reads `mean` and `sd` rather than `score`, because recall is
    /// structurally genre-dependent — a breakbeat with sixteenth hats genuinely
    /// carries much of its onset energy off the beat grid, and letting that
    /// drag its confidence down would have selecta discarding drum & bass for
    /// being drum & bass. Recall decides *which* grid wins; it should not
    /// decide how sure we are of the winner.
    ///
    /// They are carried separately rather than as `mean - sd`: that difference
    /// is a sound way to *rank* grids on one track, where every candidate
    /// shares an envelope, and a useless way to judge one grid against a fixed
    /// threshold, where it is routinely negative on perfectly good readings.
    mean: f32,
    /// Spread of onset strength across those beats, same units.
    sd: f32,
    phase_frames: f32,
}

/// Locate the precise tempo near `coarse_bpm` using the unsmoothed envelope.
///
/// Selection is already settled by this point; this only sharpens the number.
/// Alignment is scored by mean alone — the consistency penalty is a tiebreaker
/// between different tempi, not a better measure of where one tempo sits.
fn precision_pass(env: &[f32], fps: f32, coarse_bpm: f32) -> f32 {
    let total_frames = env.len() as f32;
    if total_frames < 8.0 {
        return coarse_bpm;
    }
    let rel_step = PRECISION_DRIFT_FRAMES / total_frames;
    let half = (PRECISION_SPAN / rel_step).ceil() as i32;
    let mut best = (coarse_bpm, f32::MIN);
    for i in -half..=half {
        let bpm = coarse_bpm * (1.0 + i as f32 * rel_step);
        let period = 60.0 * fps / bpm;
        if period < 2.0 {
            continue;
        }
        let steps = (period * 2.0).ceil() as usize;
        let mut mean_best = f32::MIN;
        for s in 0..steps {
            let phase = s as f32 * 0.5;
            let n = (((env.len() - 1) as f32 - phase) / period).floor() as usize + 1;
            if n < 4 {
                continue;
            }
            let sum: f32 = (0..n)
                .map(|k| interp_at(env, phase + k as f32 * period))
                .sum();
            mean_best = mean_best.max(sum / n as f32);
        }
        if mean_best > best.1 {
            best = (bpm, mean_best);
        }
    }
    best.0
}

/// Sharpen a coarse candidate by scanning a fine tempo grid around it.
fn refine(env: &[f32], fps: f32, coarse_bpm: f32) -> Candidate {
    let clip_beats = (env.len() as f32 / fps) * coarse_bpm / 60.0;
    let c = comb_score(env, fps, coarse_bpm);
    let mut best = Candidate {
        bpm: coarse_bpm,
        score: c.score,
        mean: c.mean,
        sd: c.sd,
        phase_frames: c.phase_frames,
    };
    if clip_beats < 4.0 {
        return best;
    }
    let rel_step = REFINE_DRIFT_BEATS / clip_beats;
    let half = (REFINE_SPAN / rel_step).ceil() as i32;

    for i in -half..=half {
        let bpm = coarse_bpm * (1.0 + i as f32 * rel_step);
        if !(CANONICAL_LOW_BPM..CANONICAL_HIGH_BPM).contains(&bpm) {
            continue;
        }
        let c = comb_score(env, fps, bpm);
        if c.score > best.score {
            best = Candidate {
                bpm,
                score: c.score,
                mean: c.mean,
                sd: c.sd,
                phase_frames: c.phase_frames,
            };
        }
    }
    best
}

/// A scored tempo candidate.
#[derive(Debug, Clone, Copy)]
struct Candidate {
    bpm: f32,
    score: f32,
    mean: f32,
    sd: f32,
    phase_frames: f32,
}

/// Coarse candidates from the weighted autocorrelation, already folded, plus
/// the autocorrelation itself so confidence scoring can reuse it.
fn candidates(env: &OnsetEnvelope) -> (Vec<f32>, Vec<f32>) {
    let fps = env.fps;
    let min_lag = ((60.0 * fps / SEARCH_MAX_BPM).floor() as usize).max(2);
    let max_lag = (60.0 * fps / SEARCH_MIN_BPM).ceil() as usize;
    let acf = autocorrelation(&env.values, max_lag);
    if acf.len() <= min_lag + 2 {
        return (Vec::new(), acf);
    }
    let hi = (max_lag).min(acf.len() - 2);

    let mut peaks: Vec<(f32, f32)> = Vec::new();
    for lag in (min_lag + 1)..hi {
        let bpm = 60.0 * fps / lag as f32;
        let w = acf[lag] * prior(bpm);
        let prev = acf[lag - 1] * prior(60.0 * fps / (lag - 1) as f32);
        let next = acf[lag + 1] * prior(60.0 * fps / (lag + 1) as f32);
        if w > prev && w >= next && w > 0.0 {
            peaks.push((w, bpm));
        }
    }
    peaks.sort_by(|a, b| b.0.total_cmp(&a.0));
    peaks.truncate(MAX_ACF_PEAKS);

    let mut out: Vec<f32> = Vec::new();
    for (_, bpm) in peaks {
        for ratio in CANDIDATE_RATIOS {
            let folded = fold_to_canonical(bpm * ratio);
            // 0.5 BPM is well inside what refinement will move a candidate, so
            // anything closer than that is the same candidate twice.
            if folded > 0.0 && !out.iter().any(|b| (b - folded).abs() < 0.5) {
                out.push(folded);
            }
        }
    }
    (out, acf)
}

/// True when `a` and `b` are related by a simple metric ratio.
///
/// Used to decide what counts as a *competing* reading when scoring confidence:
/// a candidate at half the chosen tempo is the same musical answer seen
/// differently, while one at an unrelated tempo means the estimator genuinely
/// could not tell.
fn metrically_related(a: f32, b: f32) -> bool {
    const RATIOS: [f32; 7] = [0.5, 2.0, 2.0 / 3.0, 3.0 / 2.0, 3.0 / 4.0, 4.0 / 3.0, 1.0];
    RATIOS.iter().any(|r| (a / b - r).abs() < 0.03 * r.max(1.0))
}

/// What failing to explain *all* the onset energy costs, in units of envelope
/// standard deviation.
///
/// The envelope is z-scored, so this is directly comparable to the precision
/// term: a grid that explains nothing gives up [`MISS_PENALTY`] sd of beat
/// strength, and the 2/3 grid that skips a third of a groove gives up about a
/// third of that. 3.0 puts a third of the energy at roughly one sd, which is
/// the same order as the consistency penalty and so trades against it rather
/// than swamping it.
const MISS_PENALTY: f32 = 3.0;

/// Half-width of the window in which a beat counts as explaining an onset.
///
/// Fixed in time, not as a fraction of the beat period. A fraction would hand a
/// slow grid a proportionally wider window and let it claim the same onsets a
/// fast grid has to hit precisely, which is the exact bias `recall` exists to
/// remove. 30 ms is roughly the perceptual tolerance for "on the beat" and
/// comfortably wider than the onset envelope's own smearing.
const EXPLAIN_HALF_WIDTH_SECS: f32 = 0.030;

/// Estimate tempo from an onset strength envelope.
pub fn estimate_tempo(env: &OnsetEnvelope) -> Option<TempoEstimate> {
    if env.values.len() < 32 {
        return None;
    }
    let fps = env.fps;
    let (coarse, acf) = candidates(env);
    let scoring_env = smooth(&env.values, (SCORING_SMOOTH_SECS * fps).round() as usize);
    let mut scored: Vec<Candidate> = coarse
        .into_iter()
        .map(|bpm| refine(&scoring_env, fps, bpm))
        .filter(|c| c.score.is_finite())
        .collect();
    if scored.is_empty() {
        return None;
    }
    scored.sort_by(|a, b| b.score.total_cmp(&a.score));
    scored.truncate(MAX_FINALISTS);

    // Rescore the finalists on the unsmoothed envelope. Smoothing is what makes
    // the search tractable, but it also lifts a wrong grid that lands on quiet
    // events towards the right grid that lands on loud ones — exactly the
    // distinction that separates a real tempo from a 2/3 metric decoy. So the
    // final comparison is made without it.
    for c in scored.iter_mut() {
        c.bpm = precision_pass(&env.values, fps, c.bpm);
        let raw = comb_score(&env.values, fps, c.bpm);
        c.score = raw.score;
        c.mean = raw.mean;
        c.sd = raw.sd;
        c.phase_frames = raw.phase_frames;
    }
    scored.sort_by(|a, b| b.score.total_cmp(&a.score));
    for c in &scored {
        tracing::debug!(bpm = c.bpm, score = c.score, "tempo candidate");
    }
    let best = scored[0];

    // The strongest reading that is not just the chosen tempo re-expressed.
    // `None` when every other finalist is a metric restatement of the winner,
    // which is the *most* confident case, not the least: nothing else on the
    // shortlist is competing for a different answer. Defaulting to a score of
    // zero instead would read as a rival that beat the winner, since comb
    // scores on real music are routinely negative.
    let rival = scored[1..]
        .iter()
        .find(|c| !metrically_related(c.bpm, best.bpm))
        .map(|c| c.score);
    let periodicity = interp_at(&acf, 60.0 * fps / best.bpm);
    let observed_beats = (env.values.len() as f32 / fps) * best.bpm / 60.0;
    let confidence = confidence(
        best.mean,
        best.sd,
        rival.map(|r| best.score - r),
        periodicity,
        observed_beats,
    );

    let mut alternates: Vec<Alternate> = Vec::new();
    // Always offer the fold's two neighbours, because the fold is an opinion
    // and a consumer analyzing downtempo material will want to overrule it.
    alternates.push(Alternate {
        value: f64::from(round2(best.bpm / 2.0)),
        label: None,
        relation: "half".into(),
        score: round3(comb_score(&env.values, fps, best.bpm / 2.0).score),
    });
    alternates.push(Alternate {
        value: f64::from(round2(best.bpm * 2.0)),
        label: None,
        relation: "double".into(),
        score: round3(comb_score(&env.values, fps, best.bpm * 2.0).score),
    });
    // The finalists converge from several seeds onto the same few tempos, so
    // report distinct readings rather than the same number three times.
    for c in scored.iter().skip(1) {
        if alternates.len() >= 5 {
            break;
        }
        let bpm = round2(c.bpm);
        if alternates
            .iter()
            .any(|a| (a.value - f64::from(bpm)).abs() < 0.5)
        {
            continue;
        }
        alternates.push(Alternate {
            value: f64::from(bpm),
            label: None,
            relation: "runner_up".into(),
            score: round3(c.score),
        });
    }

    // The envelope reacts to a transient before that transient is centred in
    // the analysis window, so the raw phase runs early by a fixed latency.
    let beat_offset = (best.phase_frames / fps + env.latency_secs).max(0.0);

    Some(TempoEstimate {
        bpm: round2(best.bpm),
        confidence: round3(confidence),
        uncertain: confidence <= UNCERTAIN_AT_OR_BELOW,
        source: TEMPO_SOURCE.into(),
        beat_offset_secs: round3(beat_offset),
        canonical_window_bpm: [CANONICAL_LOW_BPM, CANONICAL_HIGH_BPM],
        alternates,
    })
}

/// Autocorrelation at the chosen period at which the beat is considered
/// unambiguously present. A sequenced four-on-the-floor reaches 0.8; noise
/// with no periodic structure sits near 0.
const PERIODICITY_SATURATION: f32 = 0.5;

/// Mean beat strength at which the grid is considered unambiguously strong.
///
/// Measured on the winner's beat positions in standard deviations above
/// background, where a clean four-on-the-floor reaches 5 and a wrong grid sits
/// near 0. Half of clean is already past any real doubt that a beat is there.
const CLARITY_SATURATION: f32 = 2.5;

/// Score gap over the best unrelated reading at which the winner is considered
/// clearly ahead.
///
/// An absolute difference, not a ratio: comb scores are z-scored units that go
/// negative on real music, so dividing by the winner inverts the meaning
/// exactly where it matters. A full standard deviation is the same order as
/// [`CONSISTENCY_PENALTY`], the discrimination the scorer itself makes, so a
/// gap that size means the two readings were never really in contention.
const MARGIN_SATURATION: f32 = 1.0;

/// Beats of audio at which the estimate is considered fully supported.
///
/// 32 beats is about 15 seconds of house. A 30-second preview clears this
/// comfortably; a short live-capture buffer might not, and should say so rather
/// than reporting the same confidence as a full clip.
const COVERAGE_SATURATION: f32 = 32.0;

/// Fold five independent signals into a single 0-1 score.
///
/// Multiplicative rather than averaged: each factor can veto on its own, which
/// is the behaviour we want. A clip with no beat at all must not score well
/// just because whatever it found was unrivalled.
///
/// Every factor is either scale-free or measured in the envelope's own z-scored
/// units, so the same number means the same thing on a sparse house loop and on
/// a dense breakbeat. An earlier version read the winner's `mean - sd`
/// directly, which is neither: that quantity is negative on most real music,
/// so it clamped to zero and reported no confidence in answers that were
/// correct.
///
/// `gap` is the winner's score over the best unrelated reading, or `None` when
/// there is no unrelated reading to beat.
fn confidence(mean: f32, sd: f32, gap: Option<f32>, periodicity: f32, observed_beats: f32) -> f32 {
    // How far above background this grid's beats sit on average.
    let clarity = (mean / CLARITY_SATURATION).clamp(0.0, 1.0);
    // Whether they are alike. `mean / (mean + sd)` is 1 for beats of identical
    // strength and 0.5 where the spread equals the level, with no dependence on
    // how loud the track is. A grid landing on a kick, then a hat, then nothing
    // scores a decent mean and fails here, which is the metric decoy it exists
    // to catch.
    let evenness = if mean > 0.0 {
        (mean / (mean + sd)).clamp(0.0, 1.0)
    } else {
        0.0
    };
    // How much better the winner is than the best unrelated reading.
    let margin = match gap {
        Some(g) => (g / MARGIN_SATURATION).clamp(0.0, 1.0),
        None => 1.0,
    };
    // Whether the clip is periodic at this rate at all. This is the factor that
    // collapses on a beatless intro, where the flux is noise and the comb score
    // can still find a lucky alignment.
    let periodic = (periodicity / PERIODICITY_SATURATION).clamp(0.0, 1.0);
    // How much audio the estimate is standing on.
    let coverage = (observed_beats / COVERAGE_SATURATION).clamp(0.0, 1.0);

    // Exponents weight clarity hardest: it is the only factor that is low for
    // both of the two real failure modes (no beat, and a beat we missed).
    clarity.powf(0.5)
        * evenness.powf(0.25)
        * margin.powf(0.25)
        * periodic.powf(0.25)
        * coverage.powf(0.25)
}

fn round2(v: f32) -> f32 {
    (v * 100.0).round() / 100.0
}

fn round3(v: f32) -> f32 {
    (v * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::{onset_envelope, Stft};
    use crate::testsig::{self, Groove};

    const SR: u32 = 44_100;

    pub(super) fn tempo_of(signal: &[f32]) -> TempoEstimate {
        let stft = Stft::for_onsets(SR);
        let env = onset_envelope(&stft.magnitudes(signal, SR));
        estimate_tempo(&env).expect("tempo estimate")
    }

    #[test]
    fn fold_collapses_octaves_into_one_window() {
        assert!((fold_to_canonical(87.0) - 174.0).abs() < 1e-4);
        assert!((fold_to_canonical(174.0) - 174.0).abs() < 1e-4);
        assert!((fold_to_canonical(62.0) - 124.0).abs() < 1e-4);
        assert!((fold_to_canonical(248.0) - 124.0).abs() < 1e-4);
        assert!((fold_to_canonical(45.0) - 180.0 / 2.0).abs() < 1e-4);
        for bpm in [30.0, 61.0, 90.0, 128.0, 179.9, 200.0, 400.0] {
            let f = fold_to_canonical(bpm);
            assert!(
                (CANONICAL_LOW_BPM..CANONICAL_HIGH_BPM).contains(&f),
                "{bpm} folded to {f}"
            );
        }
        assert_eq!(fold_to_canonical(0.0), 0.0);
        assert_eq!(fold_to_canonical(f32::NAN), 0.0);
    }

    #[test]
    fn click_tracks_are_recovered_within_half_a_bpm() {
        for expected in [100.0f32, 118.0, 124.0, 128.0, 140.0, 174.0] {
            let sig = testsig::click_track(expected, 30.0, SR);
            let est = tempo_of(&sig);
            assert!(
                (est.bpm - expected).abs() < 0.5,
                "expected {expected}, got {} (conf {})",
                est.bpm,
                est.confidence
            );
            assert!(
                est.confidence > 0.6,
                "click track at {expected} should be confident, got {}",
                est.confidence
            );
        }
    }

    #[test]
    fn four_on_the_floor_is_not_read_at_double_time() {
        // Offbeat hats give a strong periodicity at 2x the beat. The fold puts
        // 248 back to 124, and the comb score has to prefer 124 over 165 (4/3).
        for expected in [122.0f32, 124.0, 128.0, 138.0] {
            let sig = testsig::groove(expected, 30.0, SR, Groove::FourOnFloor);
            let est = tempo_of(&sig);
            assert!(
                (est.bpm - expected).abs() < 1.0,
                "four-on-floor {expected} -> {} (conf {})",
                est.bpm,
                est.confidence
            );
        }
    }

    #[test]
    fn breakbeat_is_not_read_at_half_time() {
        // This is the drum & bass failure: the snare period is 2 beats, so the
        // strongest ACF peak sits at half the real tempo.
        for expected in [170.0f32, 174.0, 176.0] {
            let sig = testsig::groove(expected, 30.0, SR, Groove::Breakbeat);
            let est = tempo_of(&sig);
            assert!(
                (est.bpm - expected).abs() < 1.0,
                "breakbeat {expected} -> {} (conf {})",
                est.bpm,
                est.confidence
            );
        }
    }

    #[test]
    fn slow_material_is_reported_folded_with_half_offered_as_an_alternate() {
        // The documented cost of the canonical window: 87 BPM reads as 174.
        let sig = testsig::click_track(87.0, 30.0, SR);
        let est = tempo_of(&sig);
        assert!((est.bpm - 174.0).abs() < 1.0, "got {}", est.bpm);
        let half = est
            .alternates
            .iter()
            .find(|a| a.relation == "half")
            .expect("half alternate");
        assert!((half.value - 87.0).abs() < 1.0, "got {}", half.value);
        assert_eq!(est.canonical_window_bpm, [90.0, 180.0]);

        // Several candidate seeds converge on the same tempo; the list the
        // consumer sees must not repeat it.
        let mut values: Vec<i64> = est
            .alternates
            .iter()
            .map(|a| (a.value * 2.0) as i64)
            .collect();
        values.sort_unstable();
        let before = values.len();
        values.dedup();
        assert_eq!(
            values.len(),
            before,
            "duplicate alternates: {:?}",
            est.alternates
        );
    }

    #[test]
    fn beat_offset_points_at_the_first_beat() {
        let sr = SR;
        let mut sig = vec![0.0f32; (0.25 * sr as f32) as usize];
        sig.extend(testsig::click_track(128.0, 20.0, sr));
        let est = tempo_of(&sig);
        assert!((est.bpm - 128.0).abs() < 0.5);
        // Any beat in the grid is a valid phase, so compare modulo one beat.
        // Tolerance is one analysis window: onset latency depends on how loud
        // the audio around the transient is, so this is approximate by nature.
        let period = 60.0 / 128.0;
        let err = (est.beat_offset_secs - 0.25).rem_euclid(period);
        assert!(
            err < 0.02 || period - err < 0.02,
            "offset {} (period {period})",
            est.beat_offset_secs
        );
    }

    #[test]
    fn beatless_audio_is_flagged_uncertain() {
        let sig = testsig::sine(220.0, 30.0, SR);
        let stft = Stft::for_onsets(SR);
        let env = onset_envelope(&stft.magnitudes(&sig, SR));
        match estimate_tempo(&env) {
            None => {}
            Some(est) => assert!(
                est.uncertain && est.confidence <= UNCERTAIN_AT_OR_BELOW,
                "a pure tone must not read as a confident tempo: {est:?}"
            ),
        }
    }

    #[test]
    fn a_preview_that_opens_with_a_beatless_intro_still_reports_its_tempo() {
        // Previews often start on an intro or a breakdown. The estimate should
        // survive on the half that has a beat, and say it is less sure.
        let beats = testsig::groove(128.0, 15.0, SR, Groove::FourOnFloor);
        let mut sig = testsig::sine(220.0, 15.0, SR);
        sig.extend(beats);
        let est = tempo_of(&sig);
        assert!((est.bpm - 128.0).abs() < 1.0, "got {}", est.bpm);

        let clean = tempo_of(&testsig::groove(128.0, 30.0, SR, Groove::FourOnFloor));
        assert!(
            est.confidence < clean.confidence,
            "half-intro {} should be less certain than clean {}",
            est.confidence,
            clean.confidence
        );
    }

    #[test]
    fn a_short_clip_is_less_confident_than_a_long_one() {
        let long = tempo_of(&testsig::click_track(128.0, 30.0, SR));
        let short = tempo_of(&testsig::click_track(128.0, 6.0, SR));
        assert!((short.bpm - 128.0).abs() < 1.0, "got {}", short.bpm);
        assert!(
            short.confidence < long.confidence,
            "short {} vs long {}",
            short.confidence,
            long.confidence
        );
    }

    #[test]
    fn too_short_input_yields_nothing() {
        let env = OnsetEnvelope {
            values: vec![0.0; 8],
            fps: 172.0,
            latency_secs: 0.0,
            pulse_strength: 0.0,
        };
        assert!(estimate_tempo(&env).is_none());
    }

    #[test]
    fn a_strong_but_uneven_grid_still_reports_confidence() {
        // The shape every real track in the validation set has: beats plainly
        // above background, but a spread wider than the level itself, because
        // the clip contains a breakdown as well as a drop. `mean - sd` is
        // negative here, which is what used to zero the confidence on answers
        // that were correct.
        let (mean, sd) = (4.0, 5.0);
        assert!(mean - CONSISTENCY_PENALTY * sd < 0.0, "not the bug's shape");
        let c = confidence(mean, sd, Some(1.5), 0.6, 64.0);
        assert!(c > 0.3, "got {c}");

        // Evenness still separates it from a grid whose beats are alike.
        let even = confidence(mean, 0.5, Some(1.5), 0.6, 64.0);
        assert!(even > c, "even {even} should beat uneven {c}");
    }

    #[test]
    fn a_grid_with_no_beat_under_it_reports_nothing() {
        // Clarity has to veto on its own: no amount of periodicity, coverage or
        // absent competition may lift a grid that sits at background level.
        assert_eq!(confidence(0.0, 0.1, None, 1.0, 1000.0), 0.0);
        assert!(confidence(0.05, 0.1, None, 1.0, 1000.0) < 0.2);
    }

    #[test]
    fn an_uncontested_winner_is_not_penalized_for_having_no_rival() {
        // Every finalist being a metric restatement of the winner is the most
        // confident case there is. Scoring it as though a rival had tied would
        // invert that, and did: the old default rival score of zero beat a
        // winner whose own score was negative.
        let contested = confidence(4.0, 1.0, Some(0.1), 0.6, 64.0);
        let uncontested = confidence(4.0, 1.0, None, 0.6, 64.0);
        assert!(
            uncontested > contested,
            "uncontested {uncontested} vs narrowly contested {contested}"
        );
    }

    #[test]
    fn metric_relations_are_recognized() {
        assert!(metrically_related(174.0, 87.0));
        assert!(metrically_related(124.0, 124.0));
        assert!(metrically_related(120.0, 180.0));
        assert!(!metrically_related(124.0, 140.0));
    }
}

#[cfg(test)]
mod repro_tests {
    use super::tests::tempo_of;
    use super::*;
    use crate::testsig::{self, Groove};

    /// A grid that lands on every onset scores above one that lands on two
    /// thirds of them, even when the sparse grid's own points are just as
    /// strong. This is the Sandstorm failure in miniature: 2/3 of 136 has
    /// period 1.5 beats, so it alternates between a kick and an offbeat stab
    /// and its points look excellent — while a third of the pattern goes
    /// unexplained.
    #[test]
    fn a_sparse_grid_cannot_win_on_tidiness_alone() {
        let fps = 100.0;
        let bpm = 136.0;
        let period = 60.0 * fps / bpm;
        // Impulses on every beat and every offbeat, all the same height.
        let mut env = vec![0.0f32; 3000];
        let mut t = 0.0;
        while (t as usize) < env.len() {
            env[t as usize] = 1.0;
            t += period / 2.0;
        }
        let truth = comb_score(&env, fps, bpm);
        let sparse = comb_score(&env, fps, bpm * 2.0 / 3.0);
        assert!(
            truth.score > sparse.score,
            "true {} vs 2/3 {}",
            truth.score,
            sparse.score
        );
    }

    /// Real music does not give a grid whose mean exceeds its own spread, so
    /// any form that clamps a negative precision away collapses every
    /// candidate to the same value and hands the ranking to sort order. That
    /// shipped once: a live run came back with almost every candidate scoring
    /// 0.000, including the correct one.
    #[test]
    fn scores_stay_ordered_on_an_envelope_with_uneven_beats() {
        let fps = 100.0;
        let bpm = 136.0;
        let period = 60.0 * fps / bpm;
        let mut env = vec![0.0f32; 3000];
        // Alternating strong and weak beats plus offbeats, so the spread across
        // the grid comfortably exceeds its mean — the real-music case.
        let mut k = 0;
        let mut t = 0.0;
        while (t as usize) < env.len() {
            env[t as usize] = if k % 4 == 0 { 6.0 } else { 0.5 };
            t += period / 2.0;
            k += 1;
        }
        // Everything else sits below average, as a z-scored envelope does.
        for v in env.iter_mut() {
            *v -= 0.4;
        }

        let truth = comb_score(&env, fps, bpm);
        let sparse = comb_score(&env, fps, bpm * 2.0 / 3.0);
        let unrelated = comb_score(&env, fps, 103.0);
        assert!(
            truth.score > sparse.score,
            "true {} vs 2/3 {}",
            truth.score,
            sparse.score
        );
        assert!(
            truth.score > unrelated.score,
            "true {} vs unrelated {}",
            truth.score,
            unrelated.score
        );
        // The distinctions must survive as real numbers, not collapse to a
        // shared floor.
        assert!(
            (truth.score - sparse.score).abs() > 1e-6
                && (sparse.score - unrelated.score).abs() > 1e-6,
            "scores collapsed: {} {} {}",
            truth.score,
            sparse.score,
            unrelated.score
        );
    }

    #[test]
    fn a_loud_offbeat_does_not_drag_the_grid_to_two_thirds() {
        for bpm in [136.0f32, 140.0, 128.0] {
            let sig = testsig::groove(bpm, 30.0, 44_100, Groove::OffbeatTrance);
            let est = tempo_of(&sig);
            assert!(
                (est.bpm - bpm).abs() < 2.0,
                "expected {bpm}, got {} (alternates {:?})",
                est.bpm,
                est.alternates.iter().map(|a| a.value).collect::<Vec<_>>()
            );
        }
    }
}
