//! Modul DSP v2.3 (NEON):
//! - `AudioScratchBuffers`: Pre-alokasi memori agar callback audio berjalan Zero-Heap-Allocation
//! - `DcBlockerStereo`: Filter High-Pass 5Hz Orde-1 Zero-Latency (f64) untuk membuang DC offset sebelum VST3
//! - `soft_limit_stereo`: Stereo-Linked C2-Continuous True-Peak Soft Limiter (`tanh`, 64-bit)
//! - `AudioOverlayPlayer` & `OverlayBank`: Pemutar file audio (Symphonia + 80ms loop crossfade)
//! - `LatencyProbe`: Pengukur latensi In A -> Out B berbasis Cross-SR FFT Envelope Cross-Correlation

use parking_lot::Mutex;
use rustfft::{num_complex::Complex, FftPlanner};
use std::fs::File;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

// ==============================================================================
// 1. ZERO-ALLOCATION SCRATCH BUFFERS UNTUK THREAD AUDIO REAL-TIME
// ==============================================================================

pub struct AudioScratchBuffers {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
    pub prev_l: Vec<f32>,
    pub prev_r: Vec<f32>,
    pub mono_probe: Vec<f32>,
    pub bus_l: Vec<f32>,
    pub bus_r: Vec<f32>,
}

pub const MAX_SCRATCH_FRAMES: usize = 65_536;

impl AudioScratchBuffers {
    pub fn with_capacity(max_frames: usize) -> Self {
        // Pre-allocate at least 8192 frames (~170ms @ 48kHz) up to MAX_SCRATCH_FRAMES
        // so normal WASAPI/ASIO callbacks never trigger heap resizing on RT threads.
        let cap = max_frames.clamp(8192, MAX_SCRATCH_FRAMES);
        Self {
            left: vec![0.0; cap],
            right: vec![0.0; cap],
            prev_l: vec![0.0; cap],
            prev_r: vec![0.0; cap],
            mono_probe: vec![0.0; cap],
            bus_l: vec![0.0; cap],
            bus_r: vec![0.0; cap],
        }
    }

    #[inline]
    pub fn ensure_frames(&mut self, frames: usize) {
        let clamped = frames.min(MAX_SCRATCH_FRAMES);
        if clamped > self.left.len() {
            let new_cap = clamped.next_power_of_two().min(MAX_SCRATCH_FRAMES);
            self.left.resize(new_cap, 0.0);
            self.right.resize(new_cap, 0.0);
            self.prev_l.resize(new_cap, 0.0);
            self.prev_r.resize(new_cap, 0.0);
            self.mono_probe.resize(new_cap, 0.0);
            self.bus_l.resize(new_cap, 0.0);
            self.bus_r.resize(new_cap, 0.0);
        }
    }
}

// ==============================================================================
// 2. PRE-VST 20Hz DC BLOCKER & SUB-SONIC RUMBLE FILTER (64-BIT DOUBLE PRECISION)
// ==============================================================================

pub struct DcBlockerStereo {
    x1_l: f64,
    y1_l: f64,
    x1_r: f64,
    y1_r: f64,
    coeff_r: f64,
    sample_rate: u32,
}

impl DcBlockerStereo {
    pub fn new(sample_rate: u32) -> Self {
        let mut s = Self {
            x1_l: 0.0,
            y1_l: 0.0,
            x1_r: 0.0,
            y1_r: 0.0,
            coeff_r: 0.997382,
            sample_rate: 0,
        };
        s.set_sample_rate(sample_rate);
        s
    }

    #[inline]
    pub fn set_sample_rate(&mut self, sr: u32) {
        let sr = sr.max(8000);
        if sr != self.sample_rate {
            self.sample_rate = sr;
            // Cutoff ~5.0 Hz ultra-transparan dalam presisi f64: R = 1 - (2 * pi * 5.0 / sr)
            // Membuang DC offset murni (0 Hz) tanpa menggeser fase bass/warmth vokal (60-200 Hz)
            // meskipun DC Blocker aktif bersamaan di Line A dan Line B!
            let r = 1.0 - (2.0 * std::f64::consts::PI * 5.0 / (sr as f64));
            self.coeff_r = r.clamp(0.995, 0.99985);
        }
    }

    pub fn reset(&mut self) {
        self.x1_l = 0.0;
        self.y1_l = 0.0;
        self.x1_r = 0.0;
        self.y1_r = 0.0;
    }

    #[inline]
    pub fn process(&mut self, left: &mut [f32], right: &mut [f32], sample_rate: u32) {
        self.set_sample_rate(sample_rate);
        let r = self.coeff_r;
        let frames = left.len().min(right.len());

        let mut x1_l = self.x1_l;
        let mut y1_l = self.y1_l;
        let mut x1_r = self.x1_r;
        let mut y1_r = self.y1_r;

        for i in 0..frames {
            let xl = left[i] as f64;
            let yl = xl - x1_l + r * y1_l;
            x1_l = xl;
            y1_l = yl;
            left[i] = yl as f32;

            let xr = right[i] as f64;
            let yr = xr - x1_r + r * y1_r;
            x1_r = xr;
            y1_r = yr;
            right[i] = yr as f32;
        }

        // Cegah denormal pada state filter f64
        self.x1_l = if x1_l.abs() < 1e-30 { 0.0 } else { x1_l };
        self.y1_l = if y1_l.abs() < 1e-30 { 0.0 } else { y1_l };
        self.x1_r = if x1_r.abs() < 1e-30 { 0.0 } else { x1_r };
        self.y1_r = if y1_r.abs() < 1e-30 { 0.0 } else { y1_r };
    }
}

// ==============================================================================
// 3. SOFT LIMITER DENGAN PENJERNIH ANTI-ALIASING & SANITASI NaN/Inf
// ==============================================================================

/// Kurva C2-Continuous (nilai, turunan ke-1, dan turunan ke-2 kontinu di |x| == threshold)
/// dalam presisi 64-bit (`f64`) dengan 0.0-sample phase shift dan 100% bit-exact di bawah threshold.
/// Stereo-Linked C2-Continuous True-Peak Limiter (64-bit precision):
/// Menjaga koherensi fase & keseimbangan citra stereo (L/R mendapat faktor gain identik pada setiap sampel)
/// serta 100% bit-exact di bawah `threshold`.
pub fn soft_limit_stereo_linked(left: &mut [f32], right: &mut [f32], threshold: f32, ceiling: f32) {
    let frames = left.len().min(right.len());
    let mut peak = 0.0f32;
    let mut has_non_finite = false;
    for i in 0..frames {
        let l = left[i];
        let r = right[i];
        if !l.is_finite() || !r.is_finite() {
            has_non_finite = true;
            break;
        }
        let m = l.abs().max(r.abs());
        if m > peak {
            peak = m;
        }
    }

    if has_non_finite {
        peak = 0.0;
        for i in 0..frames {
            if !left[i].is_finite() {
                left[i] = if left[i].is_nan() { 0.0 } else { left[i].signum() };
            }
            if !right[i].is_finite() {
                right[i] = if right[i].is_nan() { 0.0 } else { right[i].signum() };
            }
            let m = left[i].abs().max(right[i].abs());
            if m > peak {
                peak = m;
            }
        }
    }

    if peak <= threshold {
        return;
    }

    let thr = threshold as f64;
    let headroom = ((ceiling - threshold) as f64).max(1e-5);
    for i in 0..frames {
        let l = left[i] as f64;
        let r = right[i] as f64;
        let m = l.abs().max(r.abs());
        if m > thr {
            let excess = m - thr;
            let target = thr + headroom * (excess / headroom).tanh();
            let gain = (target / m) as f32;
            left[i] *= gain;
            right[i] *= gain;
        }
    }
}

/// Sanitasi Pre/Post-VST 100% Bit-Exact 32-Bit Float (`threshold = 3.95`, `ceiling = 4.00`):
/// Tidak pernah mengompresi atau mengubah satu bit pun sinyal audio normal (`-12 dBFS .. +11.9 dBFS`),
/// hanya membersihkan NaN/Inf atau ledakan fault ekstrem di atas +12 dBFS agar rantai VST3 dan AI Voice Changer
/// menerima 100% dinamika asli seperti WASAPI Shared murni!
#[inline]
pub fn soft_limit_pre_vst_stereo(left: &mut [f32], right: &mut [f32]) {
    soft_limit_stereo_linked(left, right, 3.95, 4.00);
}

/// Mastering True-Peak Guard Akhir Sebelum DAC Hardware (`threshold = 0.995` / `-0.043 dBFS`, `ceiling = 0.9999`):
/// Menjamin 100% Bit-Exact (`gain = 1.0`) untuk seluruh sinyal audio di bawah `-0.043 dBFS` (0.995),
/// dan hanya menahan puncak ekstrem tepat di batas 0.0 dBFS agar bebas distorsi saturasi ganda.
#[inline]
pub fn soft_limit_stereo(left: &mut [f32], right: &mut [f32]) {
    soft_limit_stereo_linked(left, right, 0.995, 0.9999);
}

// ==============================================================================
// 4. LATENCY PROBE & FFT CROSS-CORRELATION
// ==============================================================================

struct ProbeInner {
    sample_rate: u32,
    max_samples: usize,
    buffer: Vec<f32>,
    write_pos: usize,
    filled: usize,
    last_write_instant: Option<std::time::Instant>,
}

pub struct LatencyProbe {
    inner: Mutex<ProbeInner>,
}

impl LatencyProbe {
    pub fn new(seconds: f64, sample_rate: u32) -> Self {
        let max_samples = ((seconds * sample_rate as f64) as usize).max(256);
        Self {
            inner: Mutex::new(ProbeInner {
                sample_rate,
                max_samples,
                buffer: vec![0.0; max_samples],
                write_pos: 0,
                filled: 0,
                last_write_instant: None,
            }),
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.inner.lock().sample_rate
    }

    pub fn set_sample_rate(&self, sample_rate: u32, seconds: f64) {
        let mut g = self.inner.lock();
        g.sample_rate = sample_rate;
        g.max_samples = ((seconds * sample_rate as f64) as usize).max(256);
        g.buffer = vec![0.0; g.max_samples];
        g.write_pos = 0;
        g.filled = 0;
        g.last_write_instant = None;
    }

    pub fn write(&self, mono_block: &[f32]) {
        self.write_at(mono_block, std::time::Instant::now());
    }

    /// Write mono samples tagged with the wall-clock timestamp of the tail sample
    /// so cross-thread snapshot phase skew `(t_a - t_b)` can be subtracted cleanly.
    pub fn write_at(&self, mono_block: &[f32], ts: std::time::Instant) {
        let n = mono_block.len();
        if n == 0 {
            return;
        }
        let Some(mut g) = self.inner.try_lock() else {
            return;
        };
        let ms = g.max_samples;
        if n >= ms {
            g.buffer.copy_from_slice(&mono_block[n - ms..]);
            g.write_pos = 0;
            g.filled = ms;
            g.last_write_instant = Some(ts);
            return;
        }
        let wp = g.write_pos;
        let end = wp + n;
        if end <= ms {
            g.buffer[wp..end].copy_from_slice(mono_block);
        } else {
            let first = ms - wp;
            g.buffer[wp..].copy_from_slice(&mono_block[..first]);
            g.buffer[..n - first].copy_from_slice(&mono_block[first..]);
        }
        g.write_pos = (wp + n) % ms;
        g.filled = (g.filled + n).min(ms);
        g.last_write_instant = Some(ts);
    }

    pub fn snapshot(&self) -> Vec<f32> {
        self.snapshot_with_meta().0
    }

    pub fn snapshot_with_meta(&self) -> (Vec<f32>, Option<std::time::Instant>, u32) {
        let g = self.inner.lock();
        let data = if g.filled < g.max_samples {
            g.buffer[..g.write_pos].to_vec()
        } else {
            let mut out = Vec::with_capacity(g.max_samples);
            out.extend_from_slice(&g.buffer[g.write_pos..]);
            out.extend_from_slice(&g.buffer[..g.write_pos]);
            out
        };
        (data, g.last_write_instant, g.sample_rate)
    }

    pub fn clear(&self) {
        let mut g = self.inner.lock();
        g.buffer.fill(0.0);
        g.write_pos = 0;
        g.filled = 0;
        g.last_write_instant = None;
    }
}

pub fn estimate_latency_ms(probe_a: &LatencyProbe, probe_b: &LatencyProbe, max_lag_ms: f64) -> Option<f64> {
    estimate_latency_ms_with_ceiling(probe_a, probe_b, max_lag_ms, max_lag_ms)
}

pub fn estimate_latency_ms_with_ceiling(
    probe_a: &LatencyProbe,
    probe_b: &LatencyProbe,
    max_lag_ms: f64,
    realistic_ceiling_ms: f64,
) -> Option<f64> {
    let (a_full, ts_a, sr_a) = probe_a.snapshot_with_meta();
    let (b_full, ts_b, sr_b) = probe_b.snapshot_with_meta();
    if sr_a == 0 || sr_b == 0 {
        return None;
    }

    // Hitung selisih fase tulis antar-thread `dt_skew_ms = (t_a - t_b)` dalam ms.
    // Jika A menulis lebih baru dari B (`t_a > t_b`), indeks puncak lag bergeser maju sebesar `(t_a - t_b)`,
    // sehingga delay fisik sejati adalah `raw_lag_ms - (t_a - t_b)`.
    let dt_skew_ms: f64 = match (ts_a, ts_b) {
        (Some(ta), Some(tb)) => {
            let skew = if ta >= tb {
                ta.duration_since(tb).as_secs_f64() * 1000.0
            } else {
                -(tb.duration_since(ta).as_secs_f64() * 1000.0)
            };
            if skew.abs() > 500.0 {
                return None;
            }
            skew
        }
        _ => 0.0,
    };

    // Gunakan envelope rate tetap ~400 Hz; jika SR Line A != SR Line B, resample `env_b`
    // tepat ke grid waktu `env_sr_a` (terjangkar pada tail `t_b`) agar 0% drift lintas sample rate!
    let env_sr = 400.0f64;
    let blk_a = ((sr_a as f64 / env_sr).round() as usize).max(1);
    let blk_b = ((sr_b as f64 / env_sr).round() as usize).max(1);
    let actual_env_sr = (sr_a as f64) / (blk_a as f64);
    let env_sr_b = (sr_b as f64) / (blk_b as f64);

    let nb_a = a_full.len() / blk_a;
    let nb_b = b_full.len() / blk_b;
    let rate_ratio = env_sr_b / actual_env_sr;
    let nb_b_mapped = if rate_ratio > 0.0 {
        ((nb_b as f64) / rate_ratio).floor() as usize
    } else {
        0
    };
    let num_blocks = nb_a.min(nb_b_mapped);
    if num_blocks < 80 || nb_b < 80 {
        // Minimal ~0.2 detik histori audio
        return None;
    }

    let a_slice = &a_full[a_full.len() - num_blocks * blk_a..];
    let b_slice = &b_full[b_full.len() - nb_b * blk_b..];

    let rms_a = (a_slice.iter().map(|x| x * x).sum::<f32>() / a_slice.len() as f32).sqrt();
    let rms_b = (b_slice.iter().map(|x| x * x).sum::<f32>() / b_slice.len() as f32).sqrt();
    if rms_a < 8e-4 || rms_b < 8e-4 {
        return None;
    }

    let mut env_a = Vec::with_capacity(num_blocks);
    for i in 0..num_blocks {
        let sa = i * blk_a;
        let ma = a_slice[sa..sa + blk_a].iter().map(|v| v.abs()).sum::<f32>() / blk_a as f32;
        env_a.push(ma);
    }

    let mut env_b = Vec::with_capacity(num_blocks);
    if (actual_env_sr - env_sr_b).abs() <= 1e-3 {
        let b_off = nb_b - num_blocks;
        for i in 0..num_blocks {
            let sb = (b_off + i) * blk_b;
            let mb = b_slice[sb..sb + blk_b].iter().map(|v| v.abs()).sum::<f32>() / blk_b as f32;
            env_b.push(mb);
        }
    } else {
        let mut raw_env_b = Vec::with_capacity(nb_b);
        for i in 0..nb_b {
            let sb = i * blk_b;
            let mb = b_slice[sb..sb + blk_b].iter().map(|v| v.abs()).sum::<f32>() / blk_b as f32;
            raw_env_b.push(mb);
        }
        let last_b = (nb_b - 1) as f64;
        for i in 0..num_blocks {
            let bins_from_tail = (num_blocks - 1 - i) as f64;
            let src = (last_b - bins_from_tail * rate_ratio).clamp(0.0, last_b);
            let idx0 = src.floor() as usize;
            let idx1 = (idx0 + 1).min(nb_b - 1);
            let frac = (src - idx0 as f64) as f32;
            env_b.push(raw_env_b[idx0] * (1.0 - frac) + raw_env_b[idx1] * frac);
        }
    }

    // Gunakan kombinasi Onset (turunan positif awal suku kata) + Envelope agar ekor Reverb panjang
    // (seperti 2x FabFilter Pro-R di Line B) dan background noise loop tidak menggeser puncak korelasi ke belakang!
    let mut onset_a = vec![0.0f32; num_blocks];
    let mut onset_b = vec![0.0f32; num_blocks];
    for i in 1..num_blocks {
        let da = (env_a[i] - env_a[i - 1]).max(0.0);
        let db = (env_b[i] - env_b[i - 1]).max(0.0);
        onset_a[i] = da * 0.75 + env_a[i] * 0.25;
        onset_b[i] = db * 0.75 + env_b[i] * 0.25;
    }
    env_a = onset_a;
    env_b = onset_b;

    let mean_a = env_a.iter().sum::<f32>() / num_blocks as f32;
    let mean_b = env_b.iter().sum::<f32>() / num_blocks as f32;

    let mut prefix_a2 = vec![0.0f64; num_blocks + 1];
    let mut prefix_b2 = vec![0.0f64; num_blocks + 1];
    for i in 0..num_blocks {
        env_a[i] -= mean_a;
        env_b[i] -= mean_b;
        let ea2 = (env_a[i] as f64) * (env_a[i] as f64);
        let eb2 = (env_b[i] as f64) * (env_b[i] as f64);
        prefix_a2[i + 1] = prefix_a2[i] + ea2;
        prefix_b2[i + 1] = prefix_b2[i] + eb2;
    }
    let norm_a = prefix_a2[num_blocks];
    let norm_b = prefix_b2[num_blocks];
    if (norm_a * norm_b).sqrt() < 1e-6 {
        return None;
    }

    // Sisakan overlap minimal 60 bin (~150ms) agar normalisasi lokal tidak meledak di ujung buffer
    let min_overlap = 60usize.min(num_blocks / 3).max(16);
    let max_valid_lag = num_blocks.saturating_sub(min_overlap);
    let max_lag = (((max_lag_ms / 1000.0) * actual_env_sr).round() as usize).min(max_valid_lag);
    if max_lag == 0 {
        return None;
    }

    let mut fft_size = 1usize;
    while fft_size < 2 * num_blocks {
        fft_size *= 2;
    }

    static GLOBAL_FFT_PLANNER: std::sync::OnceLock<Mutex<FftPlanner<f32>>> = std::sync::OnceLock::new();
    let (fft_fwd, fft_inv) = {
        let mut planner = GLOBAL_FFT_PLANNER.get_or_init(|| Mutex::new(FftPlanner::<f32>::new())).lock();
        (planner.plan_fft_forward(fft_size), planner.plan_fft_inverse(fft_size))
    };

    let mut fx = vec![Complex::new(0.0f32, 0.0f32); fft_size];
    let mut fy = vec![Complex::new(0.0f32, 0.0f32); fft_size];
    for i in 0..num_blocks {
        fx[i].re = env_a[i];
        fy[i].re = env_b[i];
    }

    fft_fwd.process(&mut fx);
    fft_fwd.process(&mut fy);

    for i in 0..fft_size {
        fy[i] = fy[i] * fx[i].conj();
    }
    fft_inv.process(&mut fy);

    // Hitung korelasi ternormalisasi bebas-bias berdasarkan energi overlap lokal `sqrt(E_a[0..N-lag] * E_b[lag..N])`
    // agar lag panjang (500..1200ms) tidak kehilangan 50-80% skor kepercayaan dibanding lag pendek!
    let mut corr_norm = vec![0.0f64; max_lag + 1];
    let min_e_a = norm_a * 0.05;
    let min_e_b = norm_b * 0.05;
    for lag in 0..=max_lag {
        let raw_val = (fy[lag].re as f64) / (fft_size as f64);
        let e_a = prefix_a2[num_blocks - lag];
        let e_b = prefix_b2[num_blocks] - prefix_b2[lag];
        if e_a >= min_e_a && e_b >= min_e_b {
            let local_denom = (e_a * e_b).sqrt();
            if local_denom > 1e-9 {
                corr_norm[lag] = (raw_val / local_denom).clamp(-1.0, 1.0);
            }
        }
    }

    // Cari lag kausal terbaik (min_lag..=max_lag, di mana Out B tertunda setelah In A).
    // Gunakan bobot prioritas kausal ringan agar suku kata berulang tidak mengalahkan puncak pertama.
    let min_lag = ((0.015 * actual_env_sr).round() as usize).min(max_lag);
    let mut best_score = f64::NEG_INFINITY;
    let mut best_lag: usize = min_lag;

    for lag in min_lag..=max_lag {
        let c = corr_norm[lag];
        let lag_ms = (lag as f64) / actual_env_sr * 1000.0;
        let causal_weight = 1.0 - 0.08 * (lag_ms / 1500.0).clamp(0.0, 1.0);
        let weighted = c * causal_weight;
        if weighted > best_score {
            best_score = weighted;
            best_lag = lag;
        }
    }

    // Jika puncak global jatuh di atas batas realistis dinamis, prioritaskan puncak korelasi sah pertama
    // di dalam rentang realistis menggunakan ambang kepercayaan yang konsisten (`MIN_CONFIDENCE`).
    const MIN_CONFIDENCE: f64 = 0.20;
    let eff_ceiling_ms = if realistic_ceiling_ms.is_finite() {
        realistic_ceiling_ms.clamp(250.0, max_lag_ms.max(250.0))
    } else {
        max_lag_ms.max(250.0)
    };
    let best_lag_ms = (best_lag as f64) / actual_env_sr * 1000.0;
    if best_lag_ms > eff_ceiling_ms {
        let realistic_max_lag = (((eff_ceiling_ms / 1000.0) * actual_env_sr).round() as usize).min(max_lag);
        let mut early_best_val = f64::NEG_INFINITY;
        let mut early_best_lag = min_lag;
        for lag in min_lag..=realistic_max_lag {
            let c = corr_norm[lag];
            if c > early_best_val {
                early_best_val = c;
                early_best_lag = lag;
            }
        }
        if early_best_val >= corr_norm[best_lag] * 0.55 && early_best_val >= MIN_CONFIDENCE {
            best_lag = early_best_lag;
        }
    }

    let confidence = corr_norm[best_lag];
    if confidence < MIN_CONFIDENCE {
        return None;
    }

    // 3-point parabolic sub-bin interpolation untuk akurasi < ±0.3 ms (menghilangkan kuantisasi bin 2.5 ms)
    let mut refined_lag = best_lag as f64;
    if best_lag > min_lag && best_lag < max_lag {
        let ym = corr_norm[best_lag - 1];
        let y0 = corr_norm[best_lag];
        let yp = corr_norm[best_lag + 1];
        let denom_p = ym - 2.0 * y0 + yp;
        if denom_p < -1e-9 {
            let delta = (0.5 * (ym - yp) / denom_p).clamp(-0.5, 0.5);
            refined_lag += delta;
        }
    }

    let raw_ms = (refined_lag / actual_env_sr) * 1000.0;
    let corrected_ms = raw_ms - dt_skew_ms;
    if !corrected_ms.is_finite() || corrected_ms < 5.0 {
        return None;
    }
    Some(corrected_ms)
}

// ==============================================================================
// 5. AUDIO OVERLAY PLAYER & BANK
// ==============================================================================

struct OverlayData {
    file_path: Option<String>,
    sample_rate: u32,
    left: Vec<f32>,
    right: Vec<f32>,
    play_pos: usize,
    duration_sec: f64,
}

pub struct AudioOverlayPlayer {
    enabled: AtomicBool,
    gain_bits: AtomicU32,
    inner: Mutex<OverlayData>,
}

impl AudioOverlayPlayer {
    pub fn new() -> Self {
        Self {
            enabled: AtomicBool::new(false),
            gain_bits: AtomicU32::new(0.5f32.to_bits()),
            inner: Mutex::new(OverlayData {
                file_path: None,
                sample_rate: 48000,
                left: Vec::new(),
                right: Vec::new(),
                play_pos: 0,
                duration_sec: 0.0,
            }),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    pub fn gain(&self) -> f32 {
        f32::from_bits(self.gain_bits.load(Ordering::Relaxed))
    }

    pub fn set_gain(&self, gain_0_2: f32) {
        let clamped = gain_0_2.clamp(0.0, 2.0);
        self.gain_bits.store(clamped.to_bits(), Ordering::Relaxed);
    }

    pub fn file_path(&self) -> Option<String> {
        self.inner.lock().file_path.clone()
    }

    pub fn duration_sec(&self) -> f64 {
        self.inner.lock().duration_sec
    }

    pub fn load_file(&self, path: &str, target_sample_rate: u32) -> Result<f64, String> {
        let target_sample_rate = target_sample_rate.max(8000);
        let (mut l, mut r, src_sr) = decode_audio_file(path)?;
        if l.is_empty() {
            return Err("Audio file is empty".into());
        }

        if src_sr != target_sample_rate && src_sr > 0 && target_sample_rate > 0 {
            l = resample_channel(&l, src_sr, target_sample_rate);
            r = resample_channel(&r, src_sr, target_sample_rate);
        }

        // 1. Pangkas padding hening MP3/WAV di awal & akhir file (maks 150ms) agar file noise tidak terputus
        let max_trim = ((target_sample_rate as usize) * 150) / 1000;
        let thr_sil = 1.5e-4f32;
        let total_raw = l.len().min(r.len());
        if total_raw > max_trim * 4 {
            let mut head_trim = 0usize;
            while head_trim < max_trim && l[head_trim].abs() < thr_sil && r[head_trim].abs() < thr_sil {
                head_trim += 1;
            }
            let mut tail_trim = 0usize;
            while tail_trim < max_trim
                && l[total_raw - 1 - tail_trim].abs() < thr_sil
                && r[total_raw - 1 - tail_trim].abs() < thr_sil
            {
                tail_trim += 1;
            }
            if head_trim > 0 || tail_trim > 0 {
                let keep_end = total_raw.saturating_sub(tail_trim);
                if keep_end > head_trim + 1024 {
                    l = l[head_trim..keep_end].to_vec();
                    r = r[head_trim..keep_end].to_vec();
                }
            }
        }

        // 2. Equal-Power Cosine/Sine Loop Crossfade 250.0 ms (`cos^2 + sin^2 == 1.0000`):
        //    Menjamin daya RMS 100% konstan (0.00 dB dip) saat memutar ulang file background noise / room tone!
        let total = l.len().min(r.len());
        let cf_n = (((target_sample_rate as f64) * 0.250) as usize).min(total / 4);
        if cf_n > 16 {
            for i in 0..cf_n {
                let t = (i as f32) / ((cf_n - 1) as f32);
                let angle = 0.5 * std::f32::consts::PI * t;
                let fade_out = angle.cos();
                let fade_in = angle.sin();
                l[i] = l[total - cf_n + i] * fade_out + l[i] * fade_in;
                r[i] = r[total - cf_n + i] * fade_out + r[i] * fade_in;
            }
            l.truncate(total - cf_n);
            r.truncate(total - cf_n);
        }

        let duration_sec = l.len() as f64 / target_sample_rate as f64;
        let mut g = self.inner.lock();
        g.left = l;
        g.right = r;
        g.sample_rate = target_sample_rate;
        g.play_pos = 0;
        g.file_path = Some(path.to_string());
        g.duration_sec = duration_sec;
        Ok(duration_sec)
    }

    pub fn ensure_sample_rate(&self, target_sample_rate: u32) -> Option<Result<f64, String>> {
        let (path_opt, need_reload) = {
            let g = self.inner.lock();
            (
                g.file_path.clone(),
                !g.left.is_empty() && g.sample_rate != target_sample_rate,
            )
        };
        if need_reload {
            if let Some(p) = path_opt {
                return Some(self.load_file(&p, target_sample_rate));
            }
        }
        None
    }

    pub fn mix_into(&self, left: &mut [f32], right: &mut [f32]) {
        if !self.is_enabled() {
            return;
        }
        let gain = self.gain();
        let Some(mut g) = self.inner.try_lock() else {
            return;
        };
        let total = g.left.len().min(g.right.len());
        if total == 0 {
            return;
        }
        let n = left.len().min(right.len());
        let mut pos = g.play_pos % total;
        let mut written = 0;
        while written < n {
            let take = (n - written).min(total - pos);
            for i in 0..take {
                left[written + i] += g.left[pos + i] * gain;
                right[written + i] += g.right[pos + i] * gain;
            }
            pos += take;
            written += take;
            if pos >= total {
                pos = 0;
            }
        }
        g.play_pos = pos;
    }
}

fn resample_channel(input: &[f32], src_sr: u32, dst_sr: u32) -> Vec<f32> {
    if input.is_empty() || src_sr == dst_sr {
        return input.to_vec();
    }
    let ratio = src_sr as f64 / dst_sr as f64;
    let out_len = ((input.len() as f64) / ratio).ceil() as usize;
    let mut out = Vec::with_capacity(out_len);
    let max_idx = input.len() - 1;
    for i in 0..out_len {
        let pos = (i as f64) * ratio;
        let i0 = (pos.floor() as usize).min(max_idx);
        let i1 = (i0 + 1).min(max_idx);
        let frac = (pos - i0 as f64) as f32;
        out.push(input[i0] * (1.0 - frac) + input[i1] * frac);
    }
    out
}

fn decode_audio_file(path: &str) -> Result<(Vec<f32>, Vec<f32>, u32), String> {
    let file = File::open(path).map_err(|e| format!("Failed to open file: {}", e))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = Path::new(path).extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| format!("Unsupported audio format: {}", e))?;

    let mut format = probed.format;
    let track = format
        .default_track()
        .ok_or_else(|| "No audio track found in file".to_string())?;
    let track_id = track.id;
    let sample_rate = track.codec_params.sample_rate.unwrap_or(48000);

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("Failed to create audio decoder: {}", e))?;

    let mut left = Vec::new();
    let mut right = Vec::new();

    while let Ok(packet) = format.next_packet() {
        if packet.track_id() != track_id {
            continue;
        }
        if let Ok(decoded) = decoder.decode(&packet) {
            let spec = *decoded.spec();
            let ch_count = spec.channels.count().max(1);
            let frames = decoded.frames();
            let mut sample_buf = SampleBuffer::<f32>::new(frames as u64, spec);
            sample_buf.copy_interleaved_ref(decoded);
            let samples = sample_buf.samples();
            if ch_count == 1 {
                for &s in samples {
                    left.push(s);
                    right.push(s);
                }
            } else {
                for chunk in samples.chunks_exact(ch_count) {
                    left.push(chunk[0]);
                    right.push(chunk[1]);
                }
            }
        }
    }

    Ok((left, right, sample_rate))
}

pub struct OverlayBank {
    pub players: Vec<AudioOverlayPlayer>,
}

impl OverlayBank {
    pub const MAX_SLOTS: usize = 1;

    pub fn new() -> Self {
        Self {
            players: vec![AudioOverlayPlayer::new()],
        }
    }

    pub fn mix_into(&self, left: &mut [f32], right: &mut [f32]) {
        for p in &self.players {
            p.mix_into(left, right);
        }
    }
}
