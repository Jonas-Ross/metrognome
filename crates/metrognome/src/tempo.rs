//! Tempo estimation: the onset envelope's autocorrelation proposes candidates,
//! the canonical fold collapses them into one octave so an octave error cannot
//! be expressed, and a phase-aligned comb filter scores and refines them.

use crate::dsp::{autocorrelation, interp_at, smooth, OnsetEnvelope};
use crate::types::{
    Alternate, Maturity, TempoConfidenceFactors, TempoEstimate, UNCERTAIN_AT_OR_BELOW,
};

/// Identifier recorded on every tempo estimate.
pub const TEMPO_SOURCE: &str = "metrognome/onset-autocorrelation-comb@1";

/// Tempo is checked against published references that agree across sources and
/// passes every verified case. DECISIONS.md entries 31 and 32.
pub const TEMPO_MATURITY: Maturity = Maturity::Validated;

/// Low edge (inclusive) of the canonical one-octave output window.
///
/// 90-180 holds house, techno and drum & bass in one octave, so an octave error
/// cannot be expressed. See DECISIONS.md for the cost.
pub const CANONICAL_LOW_BPM: f32 = 90.0;

/// High edge (exclusive) of the canonical output window.
pub const CANONICAL_HIGH_BPM: f32 = 180.0;

/// Widest tempo the autocorrelation search looks at.
///
/// Wider than the output window: a tempo's half and double are usually stronger
/// ACF peaks than the tempo itself, and the fold needs them found first.
const SEARCH_MIN_BPM: f32 = 45.0;

/// Fastest tempo the autocorrelation search looks at.
const SEARCH_MAX_BPM: f32 = 280.0;

/// Centre of the log-normal prior applied while picking ACF peaks.
///
/// Biases only which periodicities are proposed. The canonical window already
/// encodes the genre assumption; a second prior would pull everything to 128.
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
/// 30 ms is a drum transient plus programmed jitter. Narrower and the comb
/// score is unsearchable; wider and a 16th grid at 174 blurs into the beat.
const SCORING_SMOOTH_SECS: f32 = 0.030;

/// Relative span of the final precision pass, run on the unsmoothed envelope.
///
/// Only has to cover the error the smoothed search can make.
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
/// The fold handles octaves. These are the metric confusions that survive it: a
/// shuffle or half-bar pattern can peak at 2/3 or 3/4 of the real beat.
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
/// At 1.0 the score reads as the level the weakest beats reach rather than the
/// average. A grid at 2/3 of the true tempo lands on *something* every time — a
/// hat instead of a kick — and that alternation shows up as spread.
const CONSISTENCY_PENALTY: f32 = 1.0;

/// Mean and spread of onset strength on the best-aligned beat grid at `bpm`.
///
/// The envelope is z-scored, so these read as standard deviations above
/// background: a clean four-on-the-floor sits near 5, a wrong tempo near 0. No
/// tolerance window — sequenced material is metronomic to under a frame, and a
/// window would blur the discrimination refinement depends on.
fn comb_score(env: &[f32], fps: f32, bpm: f32) -> Option<CombScore> {
    if bpm <= 0.0 || env.len() < 2 {
        return None;
    }
    let period = 60.0 * fps / bpm;
    if period < 2.0 || period as usize >= env.len() {
        return None;
    }

    // The envelope is z-scored, so it has negatives. Below-average frames are
    // quiet, not negative energy, so they clamp to zero for the recall sums.
    let total: f32 = env.iter().map(|v| v.max(0.0)).sum();

    // Half-frame steps, so the phase search never limits the score. Phase is
    // chosen by mean alone and the consistency penalty then judges that
    // alignment; folding it in here lets a candidate shop for a phase where its
    // beats are uniformly mediocre.
    let steps = (period * 2.0).ceil() as usize;
    let mut best = None;
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
            // Routinely negative on real music — the spread between a strong
            // downbeat and a weak one exceeds the level itself — which is why
            // the miss penalty is subtracted rather than multiplied in.
            let precision = mean - CONSISTENCY_PENALTY * sd;
            let missed = 1.0 - recall(env, fps, period, phase, total);
            let score = precision - MISS_PENALTY * missed;
            best = Some(CombScore {
                score,
                mean,
                sd,
                phase_frames: phase,
            });
        }
    }
    best
}

/// Share of the envelope's onset energy that falls on this grid.
///
/// Precision punishes a grid that is too fast but not one that is too slow:
/// sampling every third beat of a groove posts a high mean and tight spread
/// precisely by skipping the beats that would have cost it.
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
    /// Confidence reads `mean` and `sd` rather than `score`: recall is
    /// genre-dependent, so it decides which grid wins but not how sure we are.
    /// They stay separate because `mean - sd` is routinely negative, and so
    /// useless against a fixed threshold.
    mean: f32,
    /// Spread of onset strength across those beats, same units.
    sd: f32,
    phase_frames: f32,
}

/// Locate the precise tempo near `coarse_bpm` using the unsmoothed envelope.
///
/// Selection is settled by now, so this only sharpens the number and scores
/// alignment by mean alone.
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
fn refine(env: &[f32], fps: f32, coarse_bpm: f32) -> Option<Candidate> {
    let clip_beats = (env.len() as f32 / fps) * coarse_bpm / 60.0;
    let mut best = comb_score(env, fps, coarse_bpm).map(|c| Candidate::at(coarse_bpm, c));
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
        if let Some(c) = comb_score(env, fps, bpm) {
            if best.as_ref().is_none_or(|b| c.score > b.score) {
                best = Some(Candidate::at(bpm, c));
            }
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

impl Candidate {
    fn at(bpm: f32, c: CombScore) -> Self {
        Candidate {
            bpm,
            score: c.score,
            mean: c.mean,
            sd: c.sd,
            phase_frames: c.phase_frames,
        }
    }
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
/// A candidate at half the chosen tempo is the same musical answer seen
/// differently, so it is not a competing reading for confidence.
fn metrically_related(a: f32, b: f32) -> bool {
    const RATIOS: [f32; 7] = [0.5, 2.0, 2.0 / 3.0, 3.0 / 2.0, 3.0 / 4.0, 4.0 / 3.0, 1.0];
    // Compared in log space, where a tolerance means the same thing in both
    // directions. The ratio set is closed under reciprocal, so this makes the
    // relation symmetric; an absolute window on `a / b` does not, and made a
    // tempo's own double read as an unrelated rival from one side only.
    if a <= 0.0 || b <= 0.0 {
        return false;
    }
    let d = (a / b).ln();
    RATIOS.iter().any(|r| (d - r.ln()).abs() < 0.03)
}

/// What failing to explain all the onset energy costs, in envelope standard
/// deviations.
///
/// At 3.0 a grid skipping a third of a groove gives up about one sd, the same
/// order as the consistency penalty, so the two trade rather than one swamping
/// the other.
const MISS_PENALTY: f32 = 3.0;

/// Half-width of the window in which a beat counts as explaining an onset.
///
/// Fixed in time, not a fraction of the period: a fraction would hand a slow
/// grid a wider window, the exact bias `recall` exists to remove. 30 ms is
/// about the perceptual tolerance for "on the beat".
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
        .filter_map(|bpm| refine(&scoring_env, fps, bpm))
        .filter(|c| c.score.is_finite())
        .collect();
    if scored.is_empty() {
        return None;
    }
    scored.sort_by(|a, b| b.score.total_cmp(&a.score));
    scored.truncate(MAX_FINALISTS);

    // Rescore the finalists unsmoothed. Smoothing makes the search tractable
    // but lifts a wrong grid landing on quiet events toward the right one, so
    // the final comparison goes without it.
    // Folded again: the precision pass runs after refine's window check and can
    // walk a candidate back out. The rescore runs on the folded tempo, so score
    // and phase stay consistent with it. A candidate that is unscorable on the
    // raw envelope is dropped rather than carried at a sentinel value.
    let mut scored: Vec<Candidate> = scored
        .into_iter()
        .filter_map(|c| {
            let bpm = fold_to_canonical(precision_pass(&env.values, fps, c.bpm));
            comb_score(&env.values, fps, bpm).map(|raw| Candidate::at(bpm, raw))
        })
        .collect();
    if scored.is_empty() {
        return None;
    }
    scored.sort_by(|a, b| b.score.total_cmp(&a.score));
    for c in &scored {
        tracing::debug!(bpm = c.bpm, score = c.score, "tempo candidate");
    }
    let best = scored[0];

    // The strongest reading that is not just the chosen tempo re-expressed.
    // `None` is the most confident case, not the least; a default of zero would
    // read as a rival beating a winner whose own score is negative.
    let rival = scored[1..]
        .iter()
        .find(|c| !metrically_related(c.bpm, best.bpm))
        .map(|c| c.score);
    let periodicity = interp_at(&acf, 60.0 * fps / best.bpm);
    let observed_beats = (env.values.len() as f32 / fps) * best.bpm / 60.0;
    let (raw_confidence, confidence_factors) = confidence(
        best.mean,
        best.sd,
        rival.map(|r| best.score - r),
        periodicity,
        observed_beats,
    );
    let confidence = crate::types::normalize_confidence(raw_confidence);

    let mut alternates: Vec<Alternate> = Vec::new();
    // Always offer the fold's two neighbours, because the fold is an opinion
    // and a consumer analyzing downtempo material will want to overrule it.
    for (bpm, relation) in [(best.bpm / 2.0, "half"), (best.bpm * 2.0, "double")] {
        // Skipped rather than reported at a made-up score when the clip is too
        // short to carry the grid: scores here are routinely negative, so any
        // stand-in value reads as a real and mediocre reading.
        if let Some(c) = comb_score(&env.values, fps, bpm) {
            alternates.push(Alternate {
                value: f64::from(round2(bpm)),
                label: None,
                relation: relation.into(),
                score: round3(c.score),
            });
        }
    }
    // The finalists converge from several seeds onto the same few tempos, so
    // report distinct readings rather than the same number three times.
    for c in scored.iter().skip(1) {
        if alternates.len() >= 5 {
            break;
        }
        let bpm = round2(c.bpm);
        // Several seeds converge on the winner itself. Reporting one as a
        // runner-up reads as a rival at the tempo that already won.
        if (bpm - round2(best.bpm)).abs() < 0.5 {
            continue;
        }
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

    // Rounding to two places can push a tempo just under the top edge over it,
    // so the reported number is clamped to the window rather than the reverse.
    let bpm = round2(best.bpm).clamp(CANONICAL_LOW_BPM, CANONICAL_HIGH_BPM - 0.01);

    Some(TempoEstimate {
        bpm,
        confidence,
        uncertain: confidence <= UNCERTAIN_AT_OR_BELOW,
        maturity: TEMPO_MATURITY,
        source: TEMPO_SOURCE.into(),
        beat_offset_secs: round3(beat_offset),
        canonical_window_bpm: [CANONICAL_LOW_BPM, CANONICAL_HIGH_BPM],
        alternates,
        confidence_factors,
    })
}

/// Autocorrelation at the chosen period at which the beat is considered
/// unambiguously present. A sequenced four-on-the-floor reaches 0.8; noise
/// with no periodic structure sits near 0.
const PERIODICITY_SATURATION: f32 = 0.5;

/// Mean beat strength at which the grid is considered unambiguously strong.
///
/// A clean four-on-the-floor reaches 5 sd above background; half of that is
/// already past any real doubt that a beat is there.
const CLARITY_SATURATION: f32 = 2.5;

/// Score gap over the best unrelated reading at which the winner is considered
/// clearly ahead.
///
/// Absolute, not a ratio: comb scores go negative on real music, so dividing by
/// the winner inverts the meaning. One sd matches [`CONSISTENCY_PENALTY`], the
/// discrimination the scorer itself makes.
const MARGIN_SATURATION: f32 = 1.0;

/// Beats of audio at which the estimate is considered fully supported.
///
/// 32 beats is about 15 seconds of house. A preview clears it; a short
/// live-capture buffer might not, and should say so.
const COVERAGE_SATURATION: f32 = 32.0;

/// Fold five signals into a single 0-1 score, and report what went into it.
///
/// Multiplicative, so each factor can veto alone: a clip with no beat must not
/// score well because whatever it found was unrivalled. `gap` is the winner's
/// score over the best unrelated reading, `None` when there is none.
///
/// `clarity` is not scale-free despite its z-scored units: the envelope is
/// normalized by the clip's own activity, so a busy mix sinks its own beats.
/// See DECISIONS.md entry 35.
fn confidence(
    mean: f32,
    sd: f32,
    gap: Option<f32>,
    periodicity: f32,
    observed_beats: f32,
) -> (f32, TempoConfidenceFactors) {
    // How far above background this grid's beats sit on average.
    let clarity = (mean / CLARITY_SATURATION).clamp(0.0, 1.0);
    // Whether they are alike, independent of how loud the track is. A grid
    // landing on a kick, then a hat, then nothing posts a decent mean and
    // fails here.
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
    let score = clarity.powf(0.5)
        * evenness.powf(0.25)
        * margin.powf(0.25)
        * periodic.powf(0.25)
        * coverage.powf(0.25);
    (
        score,
        TempoConfidenceFactors {
            clarity,
            evenness,
            margin,
            periodic,
            coverage,
            beat_mean: mean,
            beat_sd: sd,
            rival_gap: gap,
            periodicity,
            observed_beats,
        },
    )
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

    /// Confidence alone; the factor breakdown has its own tests.
    fn conf(mean: f32, sd: f32, gap: Option<f32>, periodicity: f32, beats: f32) -> f32 {
        confidence(mean, sd, gap, periodicity, beats).0
    }

    pub(super) fn tempo_of(signal: &[f32]) -> TempoEstimate {
        let stft = Stft::for_onsets(SR);
        let env = onset_envelope(&stft.magnitudes(signal, SR));
        estimate_tempo(&env).expect("tempo estimate")
    }

    #[test]
    fn a_tempo_estimate_declares_itself_validated() {
        let sig = testsig::click_track(128.0, 30.0, SR);
        assert_eq!(tempo_of(&sig).maturity, Maturity::Validated);
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
        // The shape every real track has: beats plainly above background, but
        // a spread wider than the level, because the clip holds a breakdown as
        // well as a drop.
        let (mean, sd) = (4.0, 5.0);
        assert!(mean - CONSISTENCY_PENALTY * sd < 0.0, "not the bug's shape");
        let c = conf(mean, sd, Some(1.5), 0.6, 64.0);
        assert!(c > 0.3, "got {c}");

        // Evenness still separates it from a grid whose beats are alike.
        let even = conf(mean, 0.5, Some(1.5), 0.6, 64.0);
        assert!(even > c, "even {even} should beat uneven {c}");
    }

    #[test]
    fn the_factors_reported_multiply_back_to_the_confidence() {
        let (score, f) = confidence(1.8, 0.6, Some(0.4), 0.35, 48.0);
        let replayed = f.clarity.powf(0.5)
            * f.evenness.powf(0.25)
            * f.margin.powf(0.25)
            * f.periodic.powf(0.25)
            * f.coverage.powf(0.25);
        assert!((score - replayed).abs() < 1e-6, "{score} vs {replayed}");
        // The raw inputs travel too: the alternates carry every loser's score
        // and never the winner's, so the gap is unrecoverable without them.
        assert_eq!(f.beat_mean, 1.8);
        assert_eq!(f.beat_sd, 0.6);
        assert_eq!(f.rival_gap, Some(0.4));
        assert_eq!(f.periodicity, 0.35);
    }

    #[test]
    fn a_busier_mix_costs_confidence_at_an_unchanged_tempo() {
        // Recorded defect, not desired behaviour: the clutter never lands on a
        // beat, so the grid is identical and only the clip's activity rises.
        // A recalibration is expected to move this; DECISIONS.md entry 35.
        let clean = testsig::groove(120.0, 30.0, SR, Groove::FourOnFloor);
        let mut busy = clean.clone();
        testsig::add_offgrid_clutter(&mut busy, 120.0, SR, 7, 0.6);

        let (a, b) = (tempo_of(&clean), tempo_of(&busy));
        assert!((a.bpm - 120.0).abs() < 0.5, "clean read {}", a.bpm);
        assert!((b.bpm - 120.0).abs() < 0.5, "busy read {}", b.bpm);
        assert!(
            b.confidence < a.confidence - 0.05,
            "clutter should cost confidence today: clean {} busy {}",
            a.confidence,
            b.confidence
        );
        // And it is the level, not the competition, that carries the loss.
        assert!(
            b.confidence_factors.beat_mean < a.confidence_factors.beat_mean - 1.0,
            "clean mean {} busy mean {}",
            a.confidence_factors.beat_mean,
            b.confidence_factors.beat_mean
        );
    }

    #[test]
    fn a_grid_with_no_beat_under_it_reports_nothing() {
        // Clarity has to veto on its own: no amount of periodicity, coverage or
        // absent competition may lift a grid that sits at background level.
        assert_eq!(conf(0.0, 0.1, None, 1.0, 1000.0), 0.0);
        assert!(conf(0.05, 0.1, None, 1.0, 1000.0) < 0.2);
    }

    #[test]
    fn an_uncontested_winner_is_not_penalized_for_having_no_rival() {
        // Every finalist being a metric restatement of the winner is the most
        // confident case there is, not the least.
        let contested = conf(4.0, 1.0, Some(0.1), 0.6, 64.0);
        let uncontested = conf(4.0, 1.0, None, 0.6, 64.0);
        assert!(
            uncontested > contested,
            "uncontested {uncontested} vs narrowly contested {contested}"
        );
    }

    #[test]
    fn the_winner_is_not_offered_as_its_own_alternate() {
        let est = tempo_of(&testsig::click_track(128.0, 30.0, SR));
        for a in &est.alternates {
            if a.relation == "runner_up" {
                assert!(
                    (a.value - f64::from(est.bpm)).abs() >= 0.5,
                    "runner-up {} duplicates the chosen {}",
                    a.value,
                    est.bpm
                );
            }
        }
    }

    #[test]
    fn the_reported_tempo_never_leaves_the_window_it_declares() {
        // The precision pass runs after refine's clamp, so it can walk a
        // candidate back out of the window the payload still advertises. A
        // consumer folding on the declared window would double-fold it.
        for bpm in [90.0, 90.2, 179.0, 179.8] {
            let est = tempo_of(&testsig::click_track(bpm, 20.0, SR));
            assert!(
                est.bpm >= CANONICAL_LOW_BPM && est.bpm < CANONICAL_HIGH_BPM,
                "{bpm} reported {} outside [{CANONICAL_LOW_BPM}, {CANONICAL_HIGH_BPM})",
                est.bpm
            );
        }
    }

    #[test]
    fn metric_relations_are_symmetric_across_the_window() {
        // Asymmetry here does not change which tempo wins, only how sure the
        // tool says it is: a winner near the bottom of the window whose double
        // reads as unrelated gets its margin — and so its confidence — cut on
        // an answer with no real competitor.
        // A full grid, not exact multiples: on an exact ratio both the old
        // form and this one agree, so a sweep of doubles and halves would pass
        // against the bug. The disagreement lives just off the ratios.
        let mut a = CANONICAL_LOW_BPM;
        while a < CANONICAL_HIGH_BPM {
            let mut b = SEARCH_MIN_BPM;
            while b < 200.0 {
                assert_eq!(
                    metrically_related(a, b),
                    metrically_related(b, a),
                    "{a} vs {b}"
                );
                b += 0.5;
            }
            a += 0.5;
        }
        assert!(!metrically_related(174.0, 91.0));
        assert!(!metrically_related(91.0, 174.0));
    }

    #[test]
    fn an_unscorable_grid_never_outranks_a_scorable_one() {
        // Scores are routinely negative on real material, so a zero sentinel
        // for "could not score this" outranked every genuine reading. Only
        // reachable on very short buffers today — which is exactly the
        // live-capture case.
        let fps = 100.0;
        let env = vec![0.5f32; 60];
        // Period under two frames, and period longer than the buffer.
        assert!(comb_score(&env, fps, 4000.0).is_none());
        assert!(comb_score(&env, fps, 50.0).is_none());

        // And a real reading on weak material scores below zero, so the old
        // sentinel outranked it.
        let env = vec![0.5f32; 300];
        let scorable = comb_score(&env, fps, 128.0).expect("scorable");
        assert!(
            scorable.score < 0.0,
            "expected a negative score to sit under the old sentinel, got {}",
            scorable.score
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

    /// A grid landing on every onset beats one landing on two thirds of them,
    /// even when the sparse grid's own points are just as strong.
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
        let truth = comb_score(&env, fps, bpm).expect("scorable");
        let sparse = comb_score(&env, fps, bpm * 2.0 / 3.0).expect("scorable");
        assert!(
            truth.score > sparse.score,
            "true {} vs 2/3 {}",
            truth.score,
            sparse.score
        );
    }

    /// Real music does not give a grid whose mean exceeds its own spread, so
    /// clamping a negative precision collapses every candidate to the same
    /// value and hands the ranking to sort order.
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

        let truth = comb_score(&env, fps, bpm).expect("scorable");
        let sparse = comb_score(&env, fps, bpm * 2.0 / 3.0).expect("scorable");
        let unrelated = comb_score(&env, fps, 103.0).expect("scorable");
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
