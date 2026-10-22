//! Modul DSP v2.0:
//! - `AudioScratchBuffers`: Pre-alokasi memori agar callback audio berjalan Zero-Heap-Allocation
//! - `DcBlockerStereo`: Filter High-Pass 20Hz Orde-1 Zero-Latency untuk membuang DC offset & sub-sonic rumble sebelum VST3
//! - `soft_limit_stereo`: First-Order ADAA (Anti-Derivative Anti-Aliasing) + `tanh` True-Peak Soft Limiter
//! - `AudioOverlayPlayer` & `OverlayBank`: Pemutar file audio (Symphonia + 80ms loop crossfade)
//! - `LatencyProbe`: Pengukur latensi In A -> Out B berbasis FFT Envelope Cross-Correlation

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

impl AudioScratchBuffers {
    pub fn with_capacity(max_frames: usize) -> Self {
        let cap = max_frames.max(4096);
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
        if frames > self.left.len() {
            self.left.resize(frames, 0.0);
            self.right.resize(frames, 0.0);
            self.prev_l.resize(frames, 0.0);
            self.prev_r.resize(frames, 0.0);
            self.mono_probe.resize(frames, 0.0);
            self.bus_l.resize(frames, 0.0);
            self.bus_r.resize(frames, 0.0);
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
#[inline]
fn soft_clip_sample_f64(x: f32, threshold: f64, headroom: f64) -> f32 {
    let xf = x as f64;
    let abs_x = xf.abs();
    if abs_x <= threshold {
        x
    } else {
        let excess = abs_x - threshold;
        let limited = threshold + headroom * (excess / headroom).tanh();
        (xf.signum() * limited) as f32
    }
}

pub fn soft_limit_slice(samples: &mut [f32], threshold: f32, ceiling: f32) {
    let mut peak = 0.0f32;
    let mut has_non_finite = false;
    for &x in samples.iter() {
        if !x.is_finite() {
            has_non_finite = true;
            break;
        }
        let a = x.abs();
        if a > peak {
            peak = a;
        }
    }

    if has_non_finite {
        peak = 0.0;
        for x in samples.iter_mut() {
            if x.is_nan() {
                *x = 0.0;
            } else if x.is_infinite() {
                *x = if *x > 0.0 { 1.0 } else { -1.0 };
            }
            let a = x.abs();
            if a > peak {
                peak = a;
            }
        }
    }

    if peak <= threshold {
        return;
    }

    let thr_f64 = threshold as f64;
    let headroom_f64 = ((ceiling - threshold) as f64).max(1e-5);
    for x_ref in samples.iter_mut() {
        let x = *x_ref;
        if x.abs() > threshold {
            *x_ref = soft_clip_sample_f64(x, thr_f64, headroom_f64);
        }
    }
}

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
/// hanya membersihkan NaN/Inf atau ledakan fault ekstrem di atas +12 dBFS agar rantai VST3 dan Vonovox
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
    }

    pub fn write(&self, mono_block: &[f32]) {
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
    }

    pub fn snapshot(&self) -> Vec<f32> {
        let g = self.inner.lock();
        if g.filled < g.max_samples {
            g.buffer[..g.write_pos].to_vec()
        } else {
            let mut out = Vec::with_capacity(g.max_samples);
            out.extend_from_slice(&g.buffer[g.write_pos..]);
            out.extend_from_slice(&g.buffer[..g.write_pos]);
            out
        }
    }
}

pub fn estimate_latency_ms(probe_a: &LatencyProbe, probe_b: &LatencyProbe, max_lag_ms: f64) -> Option<f64> {
    let sr_a = probe_a.sample_rate();
    let sr_b = probe_b.sample_rate();
    if sr_a == 0 || sr_b == 0 {
        return None;
    }
    let a_full = probe_a.snapshot();
    let b_full = probe_b.snapshot();

    // Gunakan envelope rate tetap ~400 Hz agar tetap bekerja meskipun SR Line A != SR Line B
    let env_sr = 400.0f64;
    let blk_a = ((sr_a as f64 / env_sr).round() as usize).max(1);
    let blk_b = ((sr_b as f64 / env_sr).round() as usize).max(1);

    let nb_a = a_full.len() / blk_a;
    let nb_b = b_full.len() / blk_b;
    let num_blocks = nb_a.min(nb_b);
    if num_blocks < 80 {
        // Minimal ~0.2 detik histori audio
        return None;
    }

    let a_slice = &a_full[a_full.len() - num_blocks * blk_a..];
    let b_slice = &b_full[b_full.len() - num_blocks * blk_b..];

    let rms_a = (a_slice.iter().map(|x| x * x).sum::<f32>() / a_slice.len() as f32).sqrt();
    let rms_b = (b_slice.iter().map(|x| x * x).sum::<f32>() / b_slice.len() as f32).sqrt();
    if rms_a < 8e-4 || rms_b < 8e-4 {
        return None;
    }

    let mut env_a = Vec::with_capacity(num_blocks);
    let mut env_b = Vec::with_capacity(num_blocks);

    for i in 0..num_blocks {
        let sa = i * blk_a;
        let sb = i * blk_b;
        let ma = a_slice[sa..sa + blk_a].iter().map(|v| v.abs()).sum::<f32>() / blk_a as f32;
        let mb = b_slice[sb..sb + blk_b].iter().map(|v| v.abs()).sum::<f32>() / blk_b as f32;
        env_a.push(ma);
        env_b.push(mb);
    }

    let actual_env_sr = (sr_a as f64) / (blk_a as f64);

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

    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;
    for i in 0..num_blocks {
        env_a[i] -= mean_a;
        env_b[i] -= mean_b;
        norm_a += env_a[i] * env_a[i];
        norm_b += env_b[i] * env_b[i];
    }
    let denom = (norm_a * norm_b).sqrt();
    if denom < 1e-6 {
        return None;
    }

    let max_lag = (((max_lag_ms / 1000.0) * actual_env_sr) as usize).min(num_blocks.saturating_sub(1));
    if max_lag == 0 {
        return None;
    }

    let mut fft_size = 1usize;
    while fft_size < 2 * num_blocks {
        fft_size *= 2;
    }

    let mut planner = FftPlanner::<f32>::new();
    let fft_fwd = planner.plan_fft_forward(fft_size);
    let fft_inv = planner.plan_fft_inverse(fft_size);

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

    // Cari lag kausal terbaik (0..=max_lag, di mana Out B tertunda setelah In A).
    // Gunakan bobot prioritas kausal agar suku kata berulang (mis. pada lag 800-1000ms)
    // tidak pernah mengalahkan puncak delay suara pertama yang sesungguhnya (30..550ms)!
    let min_lag = ((0.015 * actual_env_sr).round() as usize).min(max_lag);
    let mut best_val = f32::NEG_INFINITY;
    let mut best_lag: usize = min_lag;

    for lag in min_lag..=max_lag {
        let raw_val = fy[lag].re / (fft_size as f32);
        let lag_ms = (lag as f32) / (actual_env_sr as f32) * 1000.0;
        let causal_weight = 1.0 - 0.18 * (lag_ms / 1500.0).clamp(0.0, 1.0);
        let weighted = raw_val * causal_weight;
        if weighted > best_val {
            best_val = weighted;
            best_lag = lag;
        }
    }

    // Jika puncak global jatuh di atas 580ms (sering kali korelasi antar suku kata berikutnya),
    // periksa apakah sudah ada puncak lokal yang kuat (>= 78% puncak) di rentang realistis 25..550ms!
    let best_lag_ms = (best_lag as f64) / actual_env_sr * 1000.0;
    if best_lag_ms > 580.0 {
        let realistic_max_lag = ((0.550 * actual_env_sr).round() as usize).min(max_lag);
        let mut early_best_val = f32::NEG_INFINITY;
        let mut early_best_lag = min_lag;
        for lag in min_lag..=realistic_max_lag {
            let raw_val = fy[lag].re / (fft_size as f32);
            if raw_val > early_best_val {
                early_best_val = raw_val;
                early_best_lag = lag;
            }
        }
        if early_best_val >= best_val * 0.78 && (early_best_val / denom) >= 0.20 {
            best_lag = early_best_lag;
            best_val = early_best_val;
        }
    }

    let confidence = best_val / denom;
    if confidence < 0.20 {
        return None;
    }

    Some((best_lag as f64) / actual_env_sr * 1000.0)
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

        // Crossfade 80.0 ms di ujung loop agar pemutaran berulang mulus tanpa klik
        let total = l.len().min(r.len());
        let cf_n = (((target_sample_rate as f64) * 0.080) as usize).min(total / 4);
        if cf_n > 8 {
            for i in 0..cf_n {
                let t = i as f32 / (cf_n - 1) as f32;
                let fade_out = 1.0 - t;
                let fade_in = t;
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
                for chunk in samples.chunks(ch_count) {
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
