//! Quick stem split: a stereo mix into **vocals, drums, bass and other** with classic signal
//! processing (no trained model), so it runs anywhere the editor does, the browser included.
//!
//! How it decides, per short-time spectrum bin:
//! * **drums** take what is *percussive*: energy that is broad across frequency and short in time
//!   (median-filter harmonic/percussive separation, Fitzgerald 2010, with soft masks);
//! * the *harmonic* remainder below ~120 Hz (fading out by 250 Hz) is **bass**;
//! * of the rest, what the left and right channels agree on (a centred source) inside the voice
//!   band is **vocals**;
//! * everything else is **other**.
//!
//! The four masks add up to one in every bin, so **the four stems sum back to the input** (to
//! rounding). That is the property an editor needs: muting one stem removes exactly that part and
//! nothing else. It is a draft split, far short of a trained separator (Demucs and the like): a
//! centred guitar lands in "vocals", reverb tails smear, cymbals bleed. Use it to work a cut, and
//! lay model-made stems over it when they exist.

use crate::fft::Fft;

/// Frame size (power of two) and hop (half a frame: with the sine window, analysis × synthesis
/// overlap-adds to exactly one).
const N: usize = 4096;
const HOP: usize = N / 2;
const BINS: usize = N / 2 + 1;
/// Half-width of the time median (frames) and of the frequency median (bins).
const T_HALF: usize = 4;
const F_HALF: usize = 8;
/// Longest input (samples per channel): 20 minutes at 48 kHz.
pub const MAX_SAMPLES: usize = 57_600_000;

/// Stem order of [`split_stereo`]'s result.
pub const STEM_NAMES: [&str; 4] = ["Vocals", "Drums", "Bass", "Other"];

fn median(buf: &mut [f32]) -> f32 {
    if buf.is_empty() {
        return 0.0;
    }
    let mid = buf.len() / 2;
    let (_, m, _) = buf.select_nth_unstable_by(mid, |a, b| a.total_cmp(b));
    *m
}

/// Raised-cosine step: 1 below `lo`, 0 above `hi`.
fn fade_down(f: f32, lo: f32, hi: f32) -> f32 {
    if f <= lo {
        1.0
    } else if f >= hi {
        0.0
    } else {
        0.5 * (1.0 + (std::f32::consts::PI * (f - lo) / (hi - lo)).cos())
    }
}

/// Windowed spectrum of the frame starting at `pos` (may reach before 0 / past the end: zeros).
fn spectrum(x: &[f32], pos: isize, win: &[f32], fft: &Fft, re: &mut [f32], im: &mut [f32]) {
    for (n, (r, i)) in re.iter_mut().zip(im.iter_mut()).enumerate() {
        let at = pos + n as isize;
        let s = if at >= 0 { x.get(at as usize).copied().unwrap_or(0.0) } else { 0.0 };
        *r = if s.is_finite() { s * win.get(n).copied().unwrap_or(0.0) } else { 0.0 };
        *i = 0.0;
    }
    fft.forward(re, im);
}

/// Split a stereo signal into four stems, each returned as **interleaved stereo** of the input's
/// length, in [`STEM_NAMES`] order. `None` when the input is empty, longer than [`MAX_SAMPLES`] or
/// the rate is zero. The channels are read to the shorter of the two.
pub fn split_stereo(left: &[f32], right: &[f32], sample_rate: u32) -> Option<[Vec<f32>; 4]> {
    let len = left.len().min(right.len());
    if len == 0 || len > MAX_SAMPLES || sample_rate == 0 {
        return None;
    }
    let (left, right) = (left.get(..len)?, right.get(..len)?);
    let fft = Fft::new(N);
    let win: Vec<f32> = (0..N).map(|n| (std::f32::consts::PI * (n as f32 + 0.5) / N as f32).sin()).collect();
    let frames = len.div_ceil(HOP) + 1;
    let frame_pos = |i: usize| (i as isize) * HOP as isize - HOP as isize;

    // pass 1: mono magnitude of every frame
    let mut mags = vec![0.0f32; frames.checked_mul(BINS)?];
    let (mut lr, mut li, mut rr, mut ri) = (vec![0.0f32; N], vec![0.0f32; N], vec![0.0f32; N], vec![0.0f32; N]);
    for (i, row) in mags.chunks_exact_mut(BINS).enumerate() {
        spectrum(left, frame_pos(i), &win, &fft, &mut lr, &mut li);
        spectrum(right, frame_pos(i), &win, &fft, &mut rr, &mut ri);
        for (k, m) in row.iter_mut().enumerate() {
            let a = (lr.get(k).copied().unwrap_or(0.0)).hypot(li.get(k).copied().unwrap_or(0.0));
            let b = (rr.get(k).copied().unwrap_or(0.0)).hypot(ri.get(k).copied().unwrap_or(0.0));
            *m = 0.5 * (a + b);
        }
    }

    // per-bin frequency weights
    let hz = sample_rate as f32 / N as f32;
    let bass_w: Vec<f32> = (0..BINS).map(|k| fade_down(k as f32 * hz, 120.0, 250.0)).collect();
    let voice_w: Vec<f32> = (0..BINS)
        .map(|k| {
            let f = k as f32 * hz;
            (1.0 - fade_down(f, 100.0, 180.0)) * fade_down(f, 9_000.0, 14_000.0)
        })
        .collect();

    // pass 2: masks, resynthesis, overlap-add
    let out_len = len.checked_mul(2)?;
    let mut out: [Vec<f32>; 4] = [vec![0.0; out_len], vec![0.0; out_len], vec![0.0; out_len], vec![0.0; out_len]];
    let mut masks = vec![[0.0f32; 4]; BINS];
    let (mut tbuf, mut fbuf) = ([0.0f32; 2 * T_HALF + 1], [0.0f32; 2 * F_HALF + 1]);
    let (mut sr_, mut si_) = (vec![0.0f32; N], vec![0.0f32; N]);
    let scale = 1.0 / N as f32;
    for i in 0..frames {
        spectrum(left, frame_pos(i), &win, &fft, &mut lr, &mut li);
        spectrum(right, frame_pos(i), &win, &fft, &mut rr, &mut ri);
        let row = mags.get(i * BINS..(i + 1) * BINS)?;
        for (k, mask) in masks.iter_mut().enumerate() {
            // harmonic estimate: median over time at this bin
            let (t0, t1) = (i.saturating_sub(T_HALF), (i + T_HALF + 1).min(frames));
            let mut nt = 0;
            for (slot, t) in tbuf.iter_mut().zip(t0..t1) {
                *slot = mags.get(t * BINS + k).copied().unwrap_or(0.0);
                nt += 1;
            }
            let h = median(tbuf.get_mut(..nt)?);
            // percussive estimate: median over frequency in this frame
            let (k0, k1) = (k.saturating_sub(F_HALF), (k + F_HALF + 1).min(BINS));
            let mut nf = 0;
            for (slot, v) in fbuf.iter_mut().zip(row.get(k0..k1)?) {
                *slot = *v;
                nf += 1;
            }
            let p = median(fbuf.get_mut(..nf)?);
            let (h2, p2) = (h * h, p * p);
            let harm = if h2 + p2 > 1e-20 { h2 / (h2 + p2) } else { 0.5 };
            // how much left and right agree (1 = identical and in phase)
            let (a, b, c, d) =
                (lr.get(k).copied().unwrap_or(0.0), li.get(k).copied().unwrap_or(0.0), rr.get(k).copied().unwrap_or(0.0), ri.get(k).copied().unwrap_or(0.0));
            let power = a * a + b * b + c * c + d * d;
            let agree = if power > 1e-20 { (2.0 * (a * c + b * d) / power).clamp(0.0, 1.0) } else { 0.0 };
            let centre = agree * agree * agree;
            let bw = bass_w.get(k).copied().unwrap_or(0.0);
            let vw = voice_w.get(k).copied().unwrap_or(0.0) * centre;
            let rest = harm * (1.0 - bw);
            *mask = [rest * vw, 1.0 - harm, harm * bw, rest * (1.0 - vw)];
        }
        let pos = frame_pos(i);
        for (stem, dst) in out.iter_mut().enumerate() {
            for (ch, (xr, xi)) in [(&lr, &li), (&rr, &ri)].into_iter().enumerate() {
                for k in 0..N {
                    let m = masks.get(k.min(N - k)).map_or(0.0, |m| m.get(stem).copied().unwrap_or(0.0));
                    if let (Some(o), Some(q)) = (sr_.get_mut(k), si_.get_mut(k)) {
                        *o = xr.get(k).copied().unwrap_or(0.0) * m;
                        *q = xi.get(k).copied().unwrap_or(0.0) * m;
                    }
                }
                fft.inverse(&mut sr_, &mut si_);
                for (n, v) in sr_.iter().enumerate() {
                    let at = pos + n as isize;
                    if at < 0 || at as usize >= len {
                        continue;
                    }
                    if let Some(o) = dst.get_mut(at as usize * 2 + ch) {
                        *o += v * scale * win.get(n).copied().unwrap_or(0.0);
                    }
                }
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48_000;

    fn sine(f: f32, n: usize, amp: f32) -> Vec<f32> {
        (0..n).map(|i| amp * (2.0 * std::f32::consts::PI * f * i as f32 / SR as f32).sin()).collect()
    }

    /// Share of the total output energy each stem carries.
    fn shares(stems: &[Vec<f32>; 4]) -> [f32; 4] {
        let e: Vec<f32> = stems.iter().map(|s| s.iter().map(|v| v * v).sum::<f32>()).collect();
        let total: f32 = e.iter().sum::<f32>().max(1e-12);
        [e[0] / total, e[1] / total, e[2] / total, e[3] / total]
    }

    #[test]
    fn stems_sum_back_to_the_mix() {
        let n = SR as usize;
        let mut seed = 12345u32;
        let mut noise = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / 8_388_608.0 - 1.0
        };
        let l: Vec<f32> = sine(440.0, n, 0.3).iter().zip(sine(70.0, n, 0.3)).map(|(a, b)| a + b + 0.1 * noise()).collect();
        let r: Vec<f32> = sine(660.0, n, 0.3).iter().zip(sine(70.0, n, 0.3)).map(|(a, b)| a + b + 0.1 * noise()).collect();
        let stems = split_stereo(&l, &r, SR).unwrap();
        let mut worst = 0.0f32;
        for i in 0..n {
            let sl: f32 = stems.iter().map(|s| s[2 * i]).sum();
            let sr: f32 = stems.iter().map(|s| s[2 * i + 1]).sum();
            worst = worst.max((sl - l[i]).abs()).max((sr - r[i]).abs());
        }
        assert!(worst < 2e-3, "stems do not sum to the mix: {worst}");
    }

    #[test]
    fn a_centred_tone_is_vocals_and_a_panned_one_is_other() {
        let n = SR as usize;
        let tone = sine(1_000.0, n, 0.5);
        let centred = shares(&split_stereo(&tone, &tone, SR).unwrap());
        assert!(centred[0] > 0.8, "{centred:?}");
        let silent = vec![0.0; n];
        let panned = shares(&split_stereo(&tone, &silent, SR).unwrap());
        assert!(panned[3] > 0.8, "{panned:?}");
    }

    #[test]
    fn a_low_tone_is_bass_and_clicks_are_drums() {
        let n = SR as usize;
        let low = sine(60.0, n, 0.5);
        let bass = shares(&split_stereo(&low, &low, SR).unwrap());
        assert!(bass[2] > 0.8, "{bass:?}");
        let mut clicks = vec![0.0f32; n];
        for i in (0..n).step_by(12_000) {
            clicks[i] = 1.0;
        }
        let drums = shares(&split_stereo(&clicks, &clicks, SR).unwrap());
        assert!(drums[1] > 0.6, "{drums:?}");
    }

    #[test]
    fn odd_inputs_never_panic() {
        assert!(split_stereo(&[], &[], SR).is_none());
        assert!(split_stereo(&[0.1], &[0.1], 0).is_none());
        let short = split_stereo(&[0.5, -0.5, 0.25], &[0.5, -0.5], SR).unwrap();
        assert_eq!(short[0].len(), 4);
        let bad = [f32::NAN, f32::INFINITY, 0.1, -0.1];
        let stems = split_stereo(&bad, &bad, SR).unwrap();
        assert!(stems.iter().all(|s| s.iter().all(|v| v.is_finite())));
        let quiet = split_stereo(&[0.0; 5000], &[0.0; 5000], SR).unwrap();
        assert!(quiet.iter().all(|s| s.iter().all(|v| *v == 0.0)));
    }
}
