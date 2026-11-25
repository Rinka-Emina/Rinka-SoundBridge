use rinka_soundbridge_core::asio_host::{
    OrionAutoResetSyncPacket, SharedAsioBus, AUTORESET_SYNC_MAGIC, SHM_FLAG_COORD_RESET_FROM_ORION,
    SHM_FLAG_COORD_RESET_FROM_RSB,
};
use rinka_soundbridge_core::dsp::{
    estimate_latency_ms_with_ceiling, AudioScratchBuffers, LatencyProbe, MAX_SCRATCH_FRAMES,
};
use rinka_soundbridge_core::fifo::{AudioFIFO, AudioFIFOClassic, FADE_N};
use rinka_soundbridge_core::pipeline::{
    normalize_latency_ms, AudioPipeline, AutoResetCoordinator, AutoResetTransport,
};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[test]
fn test_audio_fifo_classic_overflow_wrap_preserves_chronological_order() {
    // AudioFIFOClassic enforces minimum ring capacity of 16384 samples.
    let cap = 16_384usize;
    let fifo = AudioFIFOClassic::new(cap);

    // Case 1: max_lead_samples > max_samples (32768 > 16384) and write 20000 samples (> cap).
    // Chronological order of the newest 16384 samples (3616..20000) must be preserved without wrap corruption.
    let total_write = 20_000usize;
    let in_l: Vec<f32> = (0..total_write).map(|i| i as f32).collect();
    let in_r: Vec<f32> = (0..total_write).map(|i| -(i as f32)).collect();
    fifo.write(&in_l, &in_r, 32_768);

    let mut out_l = vec![0.0f32; cap];
    let mut out_r = vec![0.0f32; cap];
    fifo.read_into(&mut out_l, &mut out_r);

    let expected_start = (total_write - cap) as f32;
    for i in 0..cap {
        assert_eq!(
            out_l[i],
            expected_start + i as f32,
            "left sample mismatch at index {i}"
        );
        assert_eq!(
            out_r[i],
            -(expected_start + i as f32),
            "right sample mismatch at index {i}"
        );
    }

    // Case 2: Incremental overflow across ring wrap boundary (10000 + 12000 = 22000 > 16384).
    fifo.reset();
    let chunk1: Vec<f32> = (0..10_000).map(|i| (i + 1) as f32).collect();
    let chunk2: Vec<f32> = (10_000..22_000).map(|i| (i + 1) as f32).collect();
    fifo.write(&chunk1, &chunk1, cap);
    fifo.write(&chunk2, &chunk2, cap);

    let mut out2_l = vec![0.0f32; cap];
    let mut out2_r = vec![0.0f32; cap];
    fifo.read_into(&mut out2_l, &mut out2_r);
    for i in 0..cap {
        let exp = (22_000 - cap + i + 1) as f32;
        assert_eq!(out2_l[i], exp, "incremental wrap mismatch at {i}");
        assert_eq!(out2_r[i], exp, "incremental wrap R mismatch at {i}");
    }
}

#[test]
fn test_audio_fifo_initial_cushion_rewind_zero_fills_prefix() {
    let fifo = AudioFIFO::new(4096);
    fifo.configure(256, Some(48000.0));

    // Pollute the ring buffer with loud non-zero values, then reset.
    let loud = vec![0.9f32; 4096];
    fifo.write(&loud, &loud);
    fifo.reset();

    // Write a small block (< target cushion) so initial !has_read rewinds read_pos into prefix slots.
    let fresh = vec![0.25f32; 128];
    fifo.write(&fresh, &fresh);

    let mut out_l = vec![0.0f32; 256];
    let mut out_r = vec![0.0f32; 256];
    fifo.read_into(&mut out_l, &mut out_r);

    // Every sample must be <= 0.30 (smoothly transitioning from zeroed prefix to 0.25), never stale 0.9!
    for (i, (&l, &r)) in out_l.iter().zip(out_r.iter()).enumerate() {
        assert!(
            l.abs() < 0.35 && r.abs() < 0.35,
            "stale ring audio leaked at index {i}: l={l}, r={r}"
        );
    }
}

/// Generates a deterministic speech-like amplitude-modulated signal with sharp syllable onsets
/// and varied cadence (non-periodic) so cross-correlation has an unambiguous causal peak.
fn generate_synthetic_speech(sr: usize, total_secs: f64) -> Vec<f32> {
    let n = (sr as f64 * total_secs).round() as usize;
    let mut out = vec![0.0f32; n];
    // Irregular syllable onset times (seconds) and durations (seconds)
    let syllables: &[(f64, f64, f32)] = &[
        (0.08, 0.11, 0.85),
        (0.29, 0.08, 0.65),
        (0.46, 0.15, 0.95),
        (0.75, 0.09, 0.70),
        (0.92, 0.13, 0.90),
        (1.19, 0.07, 0.60),
        (1.37, 0.16, 1.00),
        (1.66, 0.10, 0.80),
        (1.88, 0.12, 0.75),
        (2.14, 0.09, 0.92),
        (2.35, 0.14, 0.88),
        (2.61, 0.08, 0.72),
        (2.83, 0.15, 0.96),
        (3.11, 0.11, 0.82),
        (3.38, 0.13, 0.90),
        (3.65, 0.09, 0.78),
    ];
    for &(start_s, dur_s, amp) in syllables {
        let s0 = (start_s * sr as f64) as usize;
        let len = (dur_s * sr as f64) as usize;
        for j in 0..len {
            let idx = s0 + j;
            if idx >= n {
                break;
            }
            let frac = j as f32 / len.max(1) as f32;
            // Fast attack (first 10%), exponential decay tail
            let env = if frac < 0.10 {
                frac / 0.10
            } else {
                (-3.0 * (frac - 0.10)).exp()
            };
            let t = idx as f32 / sr as f32;
            let carrier = (2.0 * std::f32::consts::PI * 220.0 * t).sin() * 0.6
                + (2.0 * std::f32::consts::PI * 580.0 * t).sin() * 0.4;
            out[idx] += amp * env * carrier;
        }
    }
    out
}

#[test]
fn test_latency_probe_synthetic_delayed_envelopes_with_async_skew() {
    let sr = 48_000usize;
    let total_secs = 4.0f64;
    let src = generate_synthetic_speech(sr, total_secs);
    let base_t = Instant::now();

    // Test delays across short, medium, and long VC pipelines (100ms, 480ms, 950ms)
    // combined with async write-thread timestamp skews (+10ms, 0ms, -12ms).
    let delays_ms = [100.0f64, 480.0f64, 950.0f64];
    let skews_ms = [10.0f64, 0.0f64, -12.0f64];

    for &delay_ms in &delays_ms {
        for &skew_ms in &skews_ms {
            let probe_a = LatencyProbe::new(2.8, sr as u32);
            let probe_b = LatencyProbe::new(2.8, sr as u32);

            let delay_smp = ((delay_ms / 1000.0) * sr as f64).round() as usize;
            let skew_smp = ((skew_ms / 1000.0) * sr as f64).round() as isize;

            // Physical timeline:
            // Probe A ends at wall-clock time `t_a = base_t + 3.8s + max(skew, 0)`
            // Probe B ends at wall-clock time `t_b = base_t + 3.8s + max(-skew, 0)`
            // so `t_a - t_b == skew_ms`!
            // Sample at physical time `p` in probe B equals `src[p - delay_smp]`.
            let window_smp = (2.8 * sr as f64) as usize;
            let end_a = src.len() - 2000;
            let end_b = (end_a as isize - skew_smp) as usize;

            let start_a = end_a - window_smp;
            let start_b = end_b - window_smp;

            let a_vec = &src[start_a..end_a];
            let mut b_vec = vec![0.0f32; window_smp];
            for i in 0..window_smp {
                let phys_idx = start_b + i;
                if phys_idx >= delay_smp && (phys_idx - delay_smp) < src.len() {
                    b_vec[i] = src[phys_idx - delay_smp];
                }
            }

            let (ts_a, ts_b) = if skew_ms >= 0.0 {
                (
                    base_t + Duration::from_secs_f64(1.0 + skew_ms / 1000.0),
                    base_t + Duration::from_secs_f64(1.0),
                )
            } else {
                (
                    base_t + Duration::from_secs_f64(1.0),
                    base_t + Duration::from_secs_f64(1.0 + (-skew_ms) / 1000.0),
                )
            };

            probe_a.write_at(a_vec, ts_a);
            probe_b.write_at(&b_vec, ts_b);

            let est = estimate_latency_ms_with_ceiling(&probe_a, &probe_b, 1500.0, 1450.0)
                .unwrap_or_else(|| {
                    panic!(
                        "LatencyProbe returned None for delay={delay_ms}ms, skew={skew_ms}ms"
                    )
                });
            let err = (est - delay_ms).abs();
            assert!(
                err < 2.0,
                "LatencyProbe error too large for delay={delay_ms}ms, skew={skew_ms}ms: got {est:.2}ms (err={err:.2}ms)"
            );
        }
    }
}

#[test]
fn test_pipeline_normalize_latency_ms_no_unit_flip() {
    // Regression: old heuristic treated v <= 2.0 as seconds (`v * 1000.0`), turning 2.0 ms into 2000 ms!
    assert!((normalize_latency_ms(0.5) - 0.5).abs() < 1e-6);
    assert!((normalize_latency_ms(1.5) - 1.5).abs() < 1e-6);
    assert!((normalize_latency_ms(2.0) - 2.0).abs() < 1e-6);
    assert!((normalize_latency_ms(20.0) - 20.0).abs() < 1e-6);
    assert!((normalize_latency_ms(1000.0) - 1000.0).abs() < 1e-6);
    assert!((normalize_latency_ms(2500.0) - 2000.0).abs() < 1e-6);
    assert_eq!(normalize_latency_ms(-5.0), 0.0);
    assert_eq!(normalize_latency_ms(f32::NAN), 0.0);
    assert_eq!(normalize_latency_ms(f32::INFINITY), 0.0);
}

#[test]
fn test_pipeline_hw_dma_ms_tracks_distinct_capture_and_render_without_buffer_size_cap() {
    let mut line_a = AudioPipeline::new("Line A");
    let mut line_b = AudioPipeline::new("Line B");
    line_a.sample_rate = 48_000;
    line_b.sample_rate = 48_000;
    // Nominal user buffer_size is 256 (5.33 ms), while actual hardware quantum/padding is larger.
    line_a.buffer_size = 256;
    line_b.buffer_size = 256;

    // Line A capture HW quantum = 480 frames (10.0 ms); must NOT be capped to 256 frames!
    line_a.shared.hw_cap_frames.store(480, Ordering::Relaxed);
    line_a.shared.hw_ren_frames.store(128, Ordering::Relaxed);
    let dma_a = line_a.estimated_hw_quantum_ms();
    assert!(
        (dma_a - 10.0).abs() < 0.05,
        "Line A hw_dma_ms should reflect hw_cap_frames (10.0 ms), got {dma_a:.3} ms"
    );

    // Line B render HW quantum/padding = 960 frames (20.0 ms), distinct from capture actual_in_frames!
    line_b.shared.actual_in_frames.store(256, Ordering::Relaxed);
    line_b.shared.hw_cap_frames.store(256, Ordering::Relaxed);
    line_b.shared.hw_ren_frames.store(960, Ordering::Relaxed);
    let dma_b = line_b.estimated_hw_quantum_ms();
    assert!(
        (dma_b - 20.0).abs() < 0.05,
        "Line B hw_dma_ms should reflect hw_ren_frames (20.0 ms), got {dma_b:.3} ms"
    );
}

#[test]
fn test_latency_probe_cross_sample_rate_44100_vs_48000() {
    let sr_a = 44_100usize;
    let sr_b = 48_000usize;
    let total_secs = 4.0f64;
    let src_a = generate_synthetic_speech(sr_a, total_secs);
    let src_b = generate_synthetic_speech(sr_b, total_secs);
    let base_t = Instant::now();

    for &delay_ms in &[100.0f64, 480.0f64, 950.0f64] {
        for &skew_ms in &[10.0f64, -12.0f64] {
            let probe_a = LatencyProbe::new(2.8, sr_a as u32);
            let probe_b = LatencyProbe::new(2.8, sr_b as u32);

            let win_a = (2.8 * sr_a as f64) as usize;
            let win_b = (2.8 * sr_b as f64) as usize;

            let end_time_a = 3.85f64;
            let end_time_b = end_time_a - (skew_ms / 1000.0);

            let end_idx_a = (end_time_a * sr_a as f64).round() as usize;
            let start_idx_a = end_idx_a - win_a;

            let end_idx_b = (end_time_b * sr_b as f64).round() as usize;
            let start_idx_b = end_idx_b - win_b;
            let delay_smp_b = ((delay_ms / 1000.0) * sr_b as f64).round() as usize;

            let a_vec = &src_a[start_idx_a..end_idx_a];
            let mut b_vec = vec![0.0f32; win_b];
            for (i, slot) in b_vec.iter_mut().enumerate() {
                let phys_b = start_idx_b + i;
                if phys_b >= delay_smp_b && (phys_b - delay_smp_b) < src_b.len() {
                    *slot = src_b[phys_b - delay_smp_b];
                }
            }

            let (ts_a, ts_b) = if skew_ms >= 0.0 {
                (
                    base_t + Duration::from_secs_f64(1.0 + skew_ms / 1000.0),
                    base_t + Duration::from_secs_f64(1.0),
                )
            } else {
                (
                    base_t + Duration::from_secs_f64(1.0),
                    base_t + Duration::from_secs_f64(1.0 + (-skew_ms) / 1000.0),
                )
            };

            probe_a.write_at(a_vec, ts_a);
            probe_b.write_at(&b_vec, ts_b);

            let est = estimate_latency_ms_with_ceiling(&probe_a, &probe_b, 1500.0, 1450.0)
                .unwrap_or_else(|| panic!("Cross-SR LatencyProbe returned None for delay={delay_ms}ms"));
            let err = (est - delay_ms).abs();
            assert!(
                err < 2.0,
                "Cross-SR 44.1k/48k LatencyProbe drift too large for delay={delay_ms}ms, skew={skew_ms}ms: got {est:.2}ms (err={err:.2}ms)"
            );
        }
    }
}

#[test]
fn test_audio_scratch_buffers_presized_and_capped() {
    let mut s = AudioScratchBuffers::with_capacity(256);
    assert_eq!(s.left.len(), 8192);
    assert_eq!(s.right.len(), 8192);
    // Normal burst <= 8192 must not grow
    s.ensure_frames(4096);
    assert_eq!(s.left.len(), 8192);
    // Runaway request must be capped cleanly to MAX_SCRATCH_FRAMES
    s.ensure_frames(1_000_000);
    assert_eq!(s.left.len(), MAX_SCRATCH_FRAMES);
    assert_eq!(s.bus_r.len(), MAX_SCRATCH_FRAMES);
}

#[test]
fn test_audio_fifo_short_fade_s_curve_converges_smoothly_32_48_64_128() {
    let fifo = AudioFIFO::new(16_384);

    // 1. Direct S-curve monotonicity and exact endpoint convergence (0.0 -> 1.0) for fade_len = 32, 48, 64, 128.
    for &fade_len in &[32usize, 48, 64, 128] {
        let mut prev_w = 0.0f32;
        for i in 0..fade_len {
            let w = fifo.s_curve_weight(i, fade_len);
            assert!(
                w >= prev_w && w <= 1.0,
                "s_curve_weight must be monotonic in [0, 1] for fade_len={fade_len}, i={i}: prev={prev_w}, w={w}"
            );
            prev_w = w;
        }
        // Start near 0.0 (smooth raised-cosine onset) and converge to EXACT 1.0 at the final sample
        // (and 1.0 - w == 0.0 for fade-out), eliminating mid-ramp truncation at 50-69%!
        let w_first = fifo.s_curve_weight(0, fade_len);
        let w_last = fifo.s_curve_weight(fade_len - 1, fade_len);
        assert!(
            w_first > 0.0 && w_first < 0.01,
            "Expected smooth raised-cosine start near 0.0 for fade_len={fade_len}, got {w_first}"
        );
        assert!(
            (w_last - 1.0).abs() < 1e-6,
            "Expected exact 1.0 endpoint convergence for fade_len={fade_len}, got {w_last}"
        );
        assert!(
            (1.0 - w_last).abs() < 1e-6,
            "Expected exact 0.0 fade-out endpoint for fade_len={fade_len}, got {}",
            1.0 - w_last
        );
    }

    // 2. Verify bit-identical 128-sample lookup against exact raised-cosine formula.
    for i in 0..FADE_N {
        let t = (i + 1) as f32 / (FADE_N as f32);
        let expected = 0.5 * (1.0 - (std::f32::consts::PI * t).cos());
        assert_eq!(
            fifo.s_curve_weight(i, FADE_N).to_bits(),
            expected.to_bits(),
            "fade_len == 128 must remain 100% bit-identical at idx={i}"
        );
    }

    // 3. End-to-end AudioFIFO short-underrun fade-out (`32 <= take < 128`) and short-block fade-in (`n = 32, 48, 64, 128`).
    for &take_len in &[32usize, 48, 64] {
        let f = AudioFIFO::new(16_384);
        f.configure(256, Some(48_000.0));
        f.reset();
        // Write a constant DC=1.0 signal of length `take_len` (< min_bridge_avail=128 for n=256)
        // after priming initial consumer state cleanly so read hits the `take >= 32` fade-out path.
        let dc_in = vec![1.0f32; take_len];
        // Drain initial zero-prefix first via empty reads, then feed exact `take_len` samples.
        let mut sink_l = vec![0.0f32; 256];
        let mut sink_r = vec![0.0f32; 256];
        for _ in 0..6 {
            f.read_into(&mut sink_l, &mut sink_r);
        }
        f.write(&dc_in, &dc_in);
        let mut out_l = vec![-99.0f32; 256];
        let mut out_r = vec![-99.0f32; 256];
        f.read_into(&mut out_l, &mut out_r);
        // Sample `take_len - 1` must smoothly reach 0.0 (no snap from ~0.50..0.69 down to 0.0 at `take_len`!)
        assert!(
            out_l[take_len - 1].abs() < 1e-6 && out_r[take_len - 1].abs() < 1e-6,
            "Short underrun fade-out (take={take_len}) must converge to 0.0 at last sample, got L={} R={}",
            out_l[take_len - 1],
            out_r[take_len - 1]
        );
        assert_eq!(out_l[take_len], 0.0);
        // Verify smooth monotonic decay across 0..take_len
        for j in 1..take_len {
            assert!(
                out_l[j] <= out_l[j - 1] + 1e-6,
                "Short underrun fade-out (take={take_len}) not monotonic at j={j}: {} > {}",
                out_l[j],
                out_l[j - 1]
            );
        }
    }

    // 4. Short-block fade-in (`n = 32, 48, 64, 128`) after underrun must converge cleanly to 1.0 at `out[n - 1]`.
    for &blk in &[32usize, 48, 64, 128] {
        let f = AudioFIFO::new(16_384);
        f.configure(blk, Some(48_000.0));
        f.reset();
        let mut sink_l = vec![0.0f32; blk];
        let mut sink_r = vec![0.0f32; blk];
        // Trigger underrun so `fade_in = true` and `last_out = [0.0, 0.0]`
        for _ in 0..40 {
            f.read_into(&mut sink_l, &mut sink_r);
        }
        // Now fill plenty of 1.0 samples so read_into exits underrun and runs the `fade_in` ramp of length `blk.min(128)`
        let fill = vec![1.0f32; 2048];
        f.write(&fill, &fill);
        let mut out_l = vec![0.0f32; blk];
        let mut out_r = vec![0.0f32; blk];
        f.read_into(&mut out_l, &mut out_r);
        assert!(
            (out_l[blk - 1] - 1.0).abs() < 1e-5 && (out_r[blk - 1] - 1.0).abs() < 1e-5,
            "Short-block fade-in (blk={blk}) must converge smoothly to 1.0 at end of block, got L={} R={}",
            out_l[blk - 1],
            out_r[blk - 1]
        );
        assert!(
            out_l[0] < 0.02,
            "Short-block fade-in (blk={blk}) must start smoothly near 0.0, got {}",
            out_l[0]
        );
    }
}

struct InMemoryTrayAutoResetTransport {
    bus_a: Arc<SharedAsioBus>,
    bus_b: Arc<SharedAsioBus>,
    sync_pkt: Arc<Mutex<OrionAutoResetSyncPacket>>,
    on_completed_calls: Arc<AtomicU32>,
    bus_b_resync_calls: Arc<AtomicU32>,
    epoch_ms: Arc<Mutex<u64>>,
}

impl AutoResetTransport for InMemoryTrayAutoResetTransport {
    fn check_and_clear_orion_reset_flags(&self) -> bool {
        // Must NOT short-circuit `||`: clear bit 27 on BOTH bus_a and bus_b in one pass!
        let a = self.bus_a.check_and_clear_coord_reset_flag();
        let b = self.bus_b.check_and_clear_coord_reset_flag();
        a || b
    }

    fn trigger_rsb_reset_flags(&self) {
        self.bus_a.trigger_coord_reset_flag();
        self.bus_b.trigger_coord_reset_flag();
    }

    fn reset_bus_a_if_vc_inactive(&self) {
        if !self.bus_a.is_vc_active() {
            self.bus_a.reset();
        }
    }

    fn reset_bus_b_spike_and_resync(&self) {
        self.bus_b_resync_calls.fetch_add(1, Ordering::Relaxed);
        self.bus_b.reset_spike_and_resync_cushion();
    }

    fn read_sync_pkt(&self) -> Option<OrionAutoResetSyncPacket> {
        let g = self.sync_pkt.lock().unwrap();
        if g.magic == AUTORESET_SYNC_MAGIC {
            Some(*g)
        } else {
            None
        }
    }

    fn publish_config_shm(&self, enabled: bool, interval_mins: u32, cooldown_mins: u32) -> u32 {
        let mut g = self.sync_pkt.lock().unwrap();
        g.magic = AUTORESET_SYNC_MAGIC;
        g.version = 1;
        g.config_seq = g.config_seq.wrapping_add(1);
        g.enabled = if enabled { 1 } else { 0 };
        g.interval_mins = interval_mins.clamp(1, 240);
        g.cooldown_mins = cooldown_mins.clamp(1, 240);
        g.writer_id = 1;
        g.config_seq
    }

    fn publish_trigger_shm(&self, reset_count: u32) -> u32 {
        let mut g = self.sync_pkt.lock().unwrap();
        g.magic = AUTORESET_SYNC_MAGIC;
        g.version = 1;
        g.trigger_seq = g.trigger_seq.wrapping_add(1);
        g.reset_count = reset_count;
        g.last_reset_epoch_ms = *self.epoch_ms.lock().unwrap();
        g.writer_id = 1;
        g.trigger_seq
    }

    fn sync_epoch_ms_shm(&self, epoch_ms: u64) {
        let mut g = self.sync_pkt.lock().unwrap();
        if g.magic == AUTORESET_SYNC_MAGIC && epoch_ms > 0 {
            g.last_reset_epoch_ms = epoch_ms;
        }
    }

    fn now_epoch_ms(&self) -> u64 {
        *self.epoch_ms.lock().unwrap()
    }

    fn on_reset_completed(&self) {
        self.on_completed_calls.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn test_tray_mode_background_watchdog_bidirectional_orion_rsb_reset_zero_echo() {
    // Simulates RSB hidden in System Tray (`SW_HIDE`): ZERO `eframe::App::update()` calls!
    // Only the dedicated background watchdog thread runs, backed by real `SharedAsioBus::new_in_memory()`
    // and real `AudioPipeline` `SharedAudioState` instances for Line A & Line B.
    let pipe_a = AudioPipeline::new("Line A");
    let pipe_b = AudioPipeline::new("Line B");
    pipe_a.shared.is_enabled.store(true, Ordering::Release);
    pipe_b.shared.is_enabled.store(true, Ordering::Release);
    pipe_a.shared.streams_running_flag.store(true, Ordering::Release);
    pipe_b.shared.streams_running_flag.store(true, Ordering::Release);
    // Keep Line B non-silent during Phase 1 so RSB's own silence timer cannot interfere.
    pipe_a.shared.peak_level_bits.store(0.25f32.to_bits(), Ordering::Release);
    pipe_b.shared.peak_level_bits.store(0.25f32.to_bits(), Ordering::Release);

    // Put non-empty audio in Line A & B FIFOs to confirm background reset flushes them in place.
    let tone = vec![0.4f32; 512];
    pipe_a.shared.fifo.lock().write(&tone, &tone, 4096);
    pipe_b.shared.fifo.lock().write(&tone, &tone, 4096);

    let bus_a = Arc::new(SharedAsioBus::new_in_memory());
    let bus_b = Arc::new(SharedAsioBus::new_in_memory());
    bus_a.set_rsb_driver_mode(3); // OrionDirect
    bus_b.set_rsb_driver_mode(3);

    let sync_pkt = Arc::new(Mutex::new(OrionAutoResetSyncPacket {
        magic: AUTORESET_SYNC_MAGIC,
        version: 1,
        config_seq: 1,
        enabled: 1,
        interval_mins: 60,
        cooldown_mins: 10,
        trigger_seq: 5,
        reset_count: 0,
        last_reset_epoch_ms: 1_760_000_000_000,
        writer_id: 1,
        reserved: [0; 5],
    }));
    let on_completed_calls = Arc::new(AtomicU32::new(0));
    let bus_b_resync_calls = Arc::new(AtomicU32::new(0));
    let epoch_ms = Arc::new(Mutex::new(1_760_000_000_000u64));

    let transport = Arc::new(InMemoryTrayAutoResetTransport {
        bus_a: Arc::clone(&bus_a),
        bus_b: Arc::clone(&bus_b),
        sync_pkt: Arc::clone(&sync_pkt),
        on_completed_calls: Arc::clone(&on_completed_calls),
        bus_b_resync_calls: Arc::clone(&bus_b_resync_calls),
        epoch_ms: Arc::clone(&epoch_ms),
    });

    let coord = AutoResetCoordinator::new(
        Arc::clone(&pipe_a.shared),
        Arc::clone(&pipe_b.shared),
        transport,
        true,
        60,
        10,
    );
    // Start dedicated background watchdog thread at 30 ms tick (simulating tray mode; 0 App::update calls!)
    coord.start_watchdog(Duration::from_millis(30));

    // -------------------------------------------------------------------------
    // PHASE 1: ORION -> RSB coordinated reset while RSB is hidden in System Tray
    // -------------------------------------------------------------------------
    bus_a.set_orion_coord_reset_flag_for_test();
    bus_b.set_orion_coord_reset_flag_for_test();
    {
        let mut g = sync_pkt.lock().unwrap();
        g.trigger_seq = g.trigger_seq.wrapping_add(1);
        g.writer_id = 2; // 2 = ORION
    }

    let deadline = Instant::now() + Duration::from_millis(400);
    while coord.snapshot().reset_count < 1 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(15));
    }
    let snap1 = coord.snapshot();
    assert_eq!(
        snap1.reset_count, 1,
        "Background watchdog must execute ORION->RSB reset within <=400ms while hidden in tray"
    );
    assert!(snap1.flash_until.is_some(), "Flash indicator must be set for tray restore");
    assert!(snap1.last_reason.contains("ORION"), "Unexpected reason: {}", snap1.last_reason);
    assert_eq!(
        bus_a.raw_adaptive_flags() & SHM_FLAG_COORD_RESET_FROM_ORION,
        0,
        "Bit 27 (FROM_ORION) on bus_a must be cleared"
    );
    assert_eq!(
        bus_b.raw_adaptive_flags() & SHM_FLAG_COORD_RESET_FROM_ORION,
        0,
        "Bit 27 (FROM_ORION) on bus_b must be cleared"
    );
    assert_eq!(
        bus_a.raw_adaptive_flags() & SHM_FLAG_COORD_RESET_FROM_RSB,
        0,
        "ORION->RSB reset must NEVER echo bit 31 (FROM_RSB) back to ORION"
    );
    assert_eq!(
        bus_b.raw_adaptive_flags() & SHM_FLAG_COORD_RESET_FROM_RSB,
        0,
        "ORION->RSB reset must NEVER echo bit 31 (FROM_RSB) back to ORION"
    );
    assert_eq!(sync_pkt.lock().unwrap().writer_id, 2, "Must not overwrite ORION trigger_seq");
    assert_eq!(on_completed_calls.load(Ordering::Relaxed), 1);
    assert_eq!(bus_b_resync_calls.load(Ordering::Relaxed), 1);

    // Wait several more ticks + inject split delayed duplicate `trigger_seq` from ORION -> 0 echo / 0 double-fire!
    {
        let mut g = sync_pkt.lock().unwrap();
        g.trigger_seq = g.trigger_seq.wrapping_add(1);
        g.writer_id = 2;
    }
    std::thread::sleep(Duration::from_millis(120));
    assert_eq!(
        coord.snapshot().reset_count,
        1,
        "Split bit27/trigger_seq arrival within 2s must NOT double-fire or echo loop"
    );

    // -------------------------------------------------------------------------
    // PHASE 2: ORION -> RSB config sync while hidden in System Tray
    // -------------------------------------------------------------------------
    {
        let mut g = sync_pkt.lock().unwrap();
        g.config_seq = g.config_seq.wrapping_add(1);
        g.enabled = 1;
        g.interval_mins = 45;
        g.cooldown_mins = 12;
        g.writer_id = 2;
    }
    let deadline_cfg = Instant::now() + Duration::from_millis(300);
    while coord.snapshot().interval_mins != 45 && Instant::now() < deadline_cfg {
        std::thread::sleep(Duration::from_millis(15));
    }
    assert_eq!(
        coord.take_peer_config_if_dirty(),
        Some((true, 45, 12)),
        "Remote ORION config change while hidden in tray must be applied and surfaced on tray restore"
    );

    // -------------------------------------------------------------------------
    // PHASE 3: RSB -> ORION autonomous timer reset while hidden in System Tray
    // -------------------------------------------------------------------------
    // Simulate Line B silent (`0.0`) after cooldown (12 min) has elapsed, with 50 ms silence requirement.
    pipe_a.shared.peak_level_bits.store(0.0f32.to_bits(), Ordering::Release);
    pipe_b.shared.peak_level_bits.store(0.0f32.to_bits(), Ordering::Release);
    coord.configure_timing_for_test(Duration::from_secs(13 * 60), 0.05);

    let deadline_rsb = Instant::now() + Duration::from_millis(600);
    while coord.snapshot().reset_count < 2 && Instant::now() < deadline_rsb {
        std::thread::sleep(Duration::from_millis(15));
    }
    let snap2 = coord.snapshot();
    assert_eq!(
        snap2.reset_count, 2,
        "Background watchdog must fire RSB's own Auto-Reset timer while hidden in tray"
    );
    assert_ne!(
        bus_a.raw_adaptive_flags() & SHM_FLAG_COORD_RESET_FROM_RSB,
        0,
        "RSB->ORION reset must set bit 31 (FROM_RSB) on bus_a"
    );
    assert_ne!(
        bus_b.raw_adaptive_flags() & SHM_FLAG_COORD_RESET_FROM_RSB,
        0,
        "RSB->ORION reset must set bit 31 (FROM_RSB) on bus_b"
    );
    assert_eq!(
        bus_a.get_rsb_driver_mode(),
        3,
        "Setting bit 31 must preserve driver_mode bits 28..30"
    );
    {
        let pkt = *sync_pkt.lock().unwrap();
        assert_eq!(pkt.writer_id, 1, "RSB->ORION trigger must publish writer_id = 1");
        assert_eq!(pkt.reset_count, 2, "RSB->ORION trigger must publish updated reset_count = 2");
    }

    // Simulate ORION consuming bit 31, wait additional watchdog ticks -> ZERO self-retrigger / echo loop!
    assert!(bus_a.clear_rsb_coord_reset_flag_for_test());
    assert!(bus_b.clear_rsb_coord_reset_flag_for_test());
    std::thread::sleep(Duration::from_millis(120));
    assert_eq!(
        coord.snapshot().reset_count,
        2,
        "RSB must NOT self-retrigger after signaling ORION (0 echo loops)"
    );

    // Clean shutdown of background watchdog thread
    coord.stop_watchdog();
}
