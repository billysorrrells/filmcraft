//! Hit detection for cutting on the beat: where the kicks (or any sharp onsets) land in a signal,
//! and where it is quiet.
//!
//! * [`onsets`]: spectral-flux onset detector. Every hop it sums how much each frequency bin
//!   *grew* since the last frame, inside the chosen band, and keeps the local peaks that stand
//!   clear of the running average. `Band::Kick` listens to 30–160 Hz (kick drums and sub hits),
//!   `Band::All` to the whole spectrum (snares, claps, hard consonants too).
//! * [`quiet_runs`]: the stretches where the level stays under a threshold for at least a given
//!   time (dead air between lines).

use crate::fft::Fft;

const N: usize = 2048;
const HOP: usize = 512;
/// Longest input (samples): 60 minutes at 48 kHz.
pub const MAX_SAMPLES: usize = 172_800_000;

/// What part of the spectrum [`onsets`] listens to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Band {
    /// 30–160 Hz: kick drums and sub hits.
    Kick,
    /// Everything.
    All,
}

/// Sample positions of the onsets in `mono`, in order. Empty for input that is empty, too long,
/// or silent.
pub fn onsets(mono: &[f32], sample_rate: u32, band: Band) -> Vec<usize> {
    if mono.len() < N || mono.len() > MAX_SAMPLES || sample_rate == 0 {
        return Vec::new();
    }
    let fft = Fft::new(N);
    let win: Vec<f32> = (0..N).map(|n| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * n as f32 / N as f32).cos()).collect();
    let hz = sample_rate as f32 / N as f32;
    let (k0, k1) = match band {
        Band::Kick => (((30.0 / hz).floor() as usize).max(1), ((160.0 / hz).ceil() as usize).clamp(2, N / 2)),
        Band::All => (1, N / 2),
    };
    let frames = (mono.len() - N) / HOP + 1;
    let mut flux = Vec::with_capacity(frames);
    let (mut re, mut im) = (vec![0.0f32; N], vec![0.0f32; N]);
    let mut prev = vec![0.0f32; N / 2 + 1];
    let mut full = 0.0f64;
    for f in 0..frames {
        let Some(chunk) = mono.get(f * HOP..f * HOP + N) else { break };
        for ((r, i), (s, w)) in re.iter_mut().zip(im.iter_mut()).zip(chunk.iter().zip(&win)) {
            *r = if s.is_finite() { s * w } else { 0.0 };
            *i = 0.0;
        }
        fft.forward(&mut re, &mut im);
        let mut sum = 0.0f32;
        for k in 1..N / 2 {
            let m = re.get(k).copied().unwrap_or(0.0).hypot(im.get(k).copied().unwrap_or(0.0));
            full += m as f64;
            if k >= k0
                && k < k1
                && let Some(p) = prev.get_mut(k)
            {
                sum += (m - *p).max(0.0);
                *p = m;
            }
        }
        flux.push(if f == 0 { 0.0 } else { sum });
    }
    let peak = flux.iter().copied().fold(0.0f32, f32::max);
    if peak <= 1e-6 {
        return Vec::new();
    }
    // what a frame of this signal carries on average: a hit must be a real share of it, so the
    // leakage of a steady tone into the band never counts
    let floor = (full / flux.len().max(1) as f64) as f32 * 0.02;
    // a hit: a local maximum that clears 1.5x the average of the surrounding ~0.3 s and a tenth
    // of the loudest hit, at least 0.12 s after the last one
    let around = ((0.15 * sample_rate as f32 / HOP as f32).round() as usize).max(2);
    let gap = ((0.12 * sample_rate as f32 / HOP as f32).round() as usize).max(1);
    let mut out: Vec<usize> = Vec::new();
    let mut last: Option<usize> = None;
    for (i, v) in flux.iter().copied().enumerate() {
        let (a, b) = (i.saturating_sub(around), (i + around + 1).min(flux.len()));
        let Some(window) = flux.get(a..b) else { continue };
        let mean = window.iter().sum::<f32>() / window.len().max(1) as f32;
        let (l, r) = (i.saturating_sub(2), (i + 3).min(flux.len()));
        let is_max = flux.get(l..r).is_some_and(|n| n.iter().all(|x| *x <= v));
        if is_max && v > mean * 1.5 && v > peak * 0.1 && v > floor && last.is_none_or(|p| i - p >= gap) {
            // the frame's energy sits around its centre
            out.push(i * HOP + N / 2);
            last = Some(i);
        }
    }
    out
}

/// A rough tempo (beats per minute, 60–200) from the spacing of `onsets`, or `None` with fewer
/// than four of them.
pub fn tempo(onsets: &[usize], sample_rate: u32) -> Option<f32> {
    if onsets.len() < 4 || sample_rate == 0 {
        return None;
    }
    let mut gaps: Vec<f32> =
        onsets.windows(2).filter_map(|w| Some((w.get(1)?.checked_sub(*w.first()?)?) as f32 / sample_rate as f32)).filter(|g| *g > 0.05).collect();
    if gaps.is_empty() {
        return None;
    }
    let mid = gaps.len() / 2;
    let (_, m, _) = gaps.select_nth_unstable_by(mid, |a, b| a.total_cmp(b));
    let mut bpm = 60.0 / *m;
    for _ in 0..8 {
        if bpm < 60.0 {
            bpm *= 2.0;
        } else if bpm > 200.0 {
            bpm /= 2.0;
        }
    }
    bpm.is_finite().then_some(bpm)
}

/// Stretches of `mono` whose level (RMS over 10 ms) stays under `threshold_db` (dBFS) for at
/// least `min_seconds`, as `(start, end)` sample ranges in order.
pub fn quiet_runs(mono: &[f32], sample_rate: u32, threshold_db: f32, min_seconds: f32) -> Vec<(usize, usize)> {
    if mono.is_empty() || mono.len() > MAX_SAMPLES || sample_rate == 0 {
        return Vec::new();
    }
    let hop = ((sample_rate as f32 * 0.01).round() as usize).max(1);
    let threshold_db = if threshold_db.is_finite() { threshold_db.clamp(-120.0, 0.0) } else { -40.0 };
    let min_seconds = if min_seconds.is_finite() { min_seconds.clamp(0.05, 600.0) } else { 0.7 };
    let limit = 10f32.powf(threshold_db / 20.0);
    let need = ((min_seconds * sample_rate as f32) as usize).max(hop);
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (i, chunk) in mono.chunks(hop).enumerate() {
        let rms = (chunk.iter().map(|s| if s.is_finite() { s * s } else { 0.0 }).sum::<f32>() / chunk.len().max(1) as f32).sqrt();
        let at = i * hop;
        if rms < limit {
            start.get_or_insert(at);
        } else if let Some(s) = start.take()
            && at - s >= need
        {
            out.push((s, at));
        }
    }
    if let Some(s) = start
        && mono.len() - s >= need
    {
        out.push((s, mono.len()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48_000;

    /// A kick-like thump: a 60 Hz burst with a fast decay.
    fn kicks(times: &[f32], seconds: f32) -> Vec<f32> {
        let mut x = vec![0.0f32; (seconds * SR as f32) as usize];
        for t in times {
            let s0 = (t * SR as f32) as usize;
            for n in 0..(SR as usize / 5) {
                if let Some(v) = x.get_mut(s0 + n) {
                    let tt = n as f32 / SR as f32;
                    *v += (2.0 * std::f32::consts::PI * 60.0 * tt).sin() * (-tt * 25.0).exp();
                }
            }
        }
        x
    }

    #[test]
    fn finds_the_kicks_where_they_land() {
        let times = [0.5, 1.0, 1.5, 2.0, 2.5, 3.0];
        let found = onsets(&kicks(&times, 4.0), SR, Band::Kick);
        assert_eq!(found.len(), times.len(), "{found:?}");
        for (f, t) in found.iter().zip(times) {
            let err = (*f as f32 / SR as f32 - t).abs();
            assert!(err < 0.03, "kick at {t} found {err} s off");
        }
        let bpm = tempo(&found, SR).unwrap();
        assert!((bpm - 120.0).abs() < 3.0, "{bpm}");
    }

    #[test]
    fn the_kick_band_ignores_a_steady_high_tone() {
        let n = 3 * SR as usize;
        let tone: Vec<f32> = (0..n).map(|i| 0.5 * (2.0 * std::f32::consts::PI * 2000.0 * i as f32 / SR as f32).sin()).collect();
        assert!(onsets(&tone, SR, Band::Kick).len() <= 1);
        assert!(onsets(&vec![0.0; n], SR, Band::All).is_empty());
    }

    #[test]
    fn quiet_runs_are_the_gaps_between_lines() {
        let mut x = vec![0.0f32; 4 * SR as usize];
        for (i, v) in x.iter_mut().enumerate() {
            let t = i as f32 / SR as f32;
            if !(1.0..2.5).contains(&t) {
                *v = 0.3 * (2.0 * std::f32::consts::PI * 220.0 * t).sin();
            }
        }
        let runs = quiet_runs(&x, SR, -40.0, 0.7);
        assert_eq!(runs.len(), 1, "{runs:?}");
        let (a, b) = (runs[0].0 as f32 / SR as f32, runs[0].1 as f32 / SR as f32);
        assert!((a - 1.0).abs() < 0.03 && (b - 2.5).abs() < 0.03, "{a} {b}");
        assert!(quiet_runs(&x, SR, -40.0, 2.0).is_empty(), "shorter than the minimum");
    }

    #[test]
    fn odd_inputs_never_panic() {
        assert!(onsets(&[], SR, Band::All).is_empty());
        assert!(onsets(&[0.1; 10], SR, Band::Kick).is_empty());
        assert!(onsets(&[f32::NAN; 5000], 1, Band::Kick).is_empty());
        assert!(tempo(&[], SR).is_none());
        assert!(tempo(&[5, 5, 5, 5, 5], SR).is_none());
        assert!(quiet_runs(&[], SR, -40.0, 1.0).is_empty());
        assert_eq!(quiet_runs(&[f32::INFINITY; 48_000], SR, f32::NAN, f32::NAN).len(), 1);
    }
}
