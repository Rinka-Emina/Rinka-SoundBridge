//! Dual-Line Multi-Driver Audio Pipeline v2.0 (`AudioPipeline`)
//! Mendukung 4 Backend Driver Audio secara Native:
//! 1. `WASAPI Shared` (Auto-Convert, ramah multi-aplikasi OBS/Discord)
//! 2. `WASAPI Exclusive` (Direct hardware buffer via COM `IAudioClient` Exclusive Event-Driven + auto-fallback)
//! 3. `ASIO Native` (Direct Steinberg COM `IASIO` via Registry `HKLM\SOFTWARE\ASIO` + Control Panel)
//! 4. `ASIO Experimental (Flex-Bridge)` (Event-Driven Low-Latency AutoConvert + Jembatan Langsung `Rinka SoundBridge ASIO`)
//!
//! Dilengkapi peningkatan kualitas suara & performa v2.0:
//! - Zero-Allocation Real-Time Callback (`AudioScratchBuffers`)
//! - Hardware Denormal Killer (`FTZ`/`DAZ` di `boost_audio_thread`) & Windows 11 EcoQoS Disabler
//! - Zero-Latency 20Hz Pre-VST DC Blocker & Sub-Sonic Filter (`DcBlockerStereo`)
//! - First-Order ADAA Anti-Aliasing True-Peak Soft Limiter (`soft_limit_stereo`)
//! - Rantai VST3 Copy-On-Write bebas lock (`ArcSwap`) + Mode Aman (Fail-Closed)

use crate::asio_host::{
    compute_asio_reported_latency_samples, compute_rsb_flexasio_reported_latency_samples,
    get_asio_bus_line_a_out, get_asio_bus_line_b_in, get_dsp_rsb_bus_a_out, get_dsp_rsb_bus_b_in,
    scan_asio_drivers, AsioDriverInfo, AsioStreamHandle,
};
use crate::dsp::{
    soft_limit_pre_vst_stereo, soft_limit_stereo, AudioScratchBuffers, DcBlockerStereo,
    LatencyProbe, OverlayBank,
};
use crate::fifo::{FifoStats, FifoVariant};
use crate::logger::{log_error, log_info, log_warn, record_breadcrumb};
use crate::config::{DspRsbAsioConfig, FlexAsioProfileConfig, RsbAsioConfig};
use crate::vst3_host::VSTSlot;
use crate::wasapi_exclusive::{FlexAsioEndpointOptions, WasapiExclusivePair, WasapiFlexAsioPair};

/// Standalone Virtual ASIO ID 1 (Line A Output -> sent to Voice Changer Input)
pub const ASIO_LINE_A_OUT_DEVICE_NAME: &str =
    "🚀 [ASIO ID: Line-A-Out] Rinka ASIO Line A Out (To VC Input)";
pub const ASIO_LINE_A_OUT_LEGACY_NAME: &str =
    "🚀 [ASIO ID: Line-A-Out] Rinka ASIO Line A Out (Ke Input VC)";

/// Standalone Virtual ASIO ID 2 (Line B Input <- received from Voice Changer Output)
pub const ASIO_LINE_B_IN_DEVICE_NAME: &str =
    "🚀 [ASIO ID: Line-B-In] Rinka ASIO Line B In (From VC Output)";
pub const ASIO_LINE_B_IN_LEGACY_NAME: &str =
    "🚀 [ASIO ID: Line-B-In] Rinka ASIO Line B In (Dari Output VC)";

#[inline]
pub fn is_virtual_line_a_out_name(name: &str) -> bool {
    name == ASIO_LINE_A_OUT_DEVICE_NAME
        || name == ASIO_LINE_A_OUT_LEGACY_NAME
        || name.contains("[ASIO ID: Line-A-Out]")
}

#[inline]
pub fn is_virtual_line_b_in_name(name: &str) -> bool {
    name == ASIO_LINE_B_IN_DEVICE_NAME
        || name == ASIO_LINE_B_IN_LEGACY_NAME
        || name.contains("[ASIO ID: Line-B-In]")
}

pub fn normalize_virtual_device_name(name: &str) -> String {
    if is_virtual_line_a_out_name(name) {
        ASIO_LINE_A_OUT_DEVICE_NAME.to_string()
    } else if is_virtual_line_b_in_name(name) {
        ASIO_LINE_B_IN_DEVICE_NAME.to_string()
    } else {
        name.to_string()
    }
}
use crate::win32::{boost_audio_thread, enable_cpu_denormal_flush};
use arc_swap::ArcSwap;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriverBackend {
    WasapiShared,
    WasapiExclusive,
    AsioNative,
    AsioExperimental,
    AsioDspRsb,
    RsbAsio,
}

impl DriverBackend {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::WasapiShared => "wasapi_shared",
            Self::WasapiExclusive => "wasapi_exclusive",
            Self::AsioNative => "asio",
            Self::AsioExperimental => "asio_experimental",
            Self::AsioDspRsb => "asio_dsp_rsb",
            Self::RsbAsio => "rsb_asio",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "asio" | "asio_native" => Self::AsioNative,
            "rsb_asio" | "rsb_flex_asio" | "builtin_flexasio" => Self::RsbAsio,
            "soundbridge_asio"
            | "asio_dsp_rsb"
            | "dsp_rsb"
            | "asio_dsp"
            | "asio_experimental"
            | "flex_asio"
            | "asio_exp" => Self::AsioDspRsb,
            _ => Self::WasapiShared,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::WasapiShared | Self::WasapiExclusive => "WASAPI Shared",
            Self::AsioNative => "ASIO Native",
            Self::RsbAsio => "Rsb ASIO (Experimental)",
            Self::AsioDspRsb | Self::AsioExperimental => "SoundBridge ASIO (Experimental)",
        }
    }
}

#[derive(Clone, Debug)]
pub struct WasapiDeviceInfo {
    pub index: usize,
    pub name: String,
    pub max_channels: u16,
    pub default_sample_rate: u32,
}

pub fn get_wasapi_devices() -> (Vec<WasapiDeviceInfo>, Vec<WasapiDeviceInfo>) {
    let host = cpal::default_host();
    let mut inputs = Vec::new();
    let mut outputs = Vec::new();

    if let Ok(devs) = host.input_devices() {
        for (idx, dev) in devs.enumerate() {
            let name = dev.name().unwrap_or_else(|_| format!("Input Device {}", idx));
            let (ch, sr) = dev
                .default_input_config()
                .map(|c| (c.channels(), c.sample_rate().0))
                .unwrap_or((2, 48000));
            if ch > 0 {
                inputs.push(WasapiDeviceInfo {
                    index: idx,
                    name,
                    max_channels: ch,
                    default_sample_rate: sr,
                });
            }
        }
    }

    if let Ok(devs) = host.output_devices() {
        for (idx, dev) in devs.enumerate() {
            let name = dev.name().unwrap_or_else(|_| format!("Output Device {}", idx));
            let (ch, sr) = dev
                .default_output_config()
                .map(|c| (c.channels(), c.sample_rate().0))
                .unwrap_or((2, 48000));
            if ch > 0 {
                outputs.push(WasapiDeviceInfo {
                    index: idx,
                    name,
                    max_channels: ch,
                    default_sample_rate: sr,
                });
            }
        }
    }

    (inputs, outputs)
}

pub fn get_asio_devices() -> Vec<AsioDriverInfo> {
    scan_asio_drivers()
}

pub struct SharedAudioState {
    pub name: String,
    pub is_enabled: AtomicBool,
    pub is_muted: AtomicBool,
    pub dc_blocker_enabled: AtomicBool,
    pub master_gain_bits: AtomicU32,
    pub active_sample_rate: AtomicU32,
    pub buffer_size: AtomicU32,
    pub actual_in_frames: AtomicU32,
    pub is_asio_experimental: AtomicBool,
    pub is_asio_dsp_rsb: AtomicBool,
    pub is_rsb_asio: AtomicBool,
    pub rsb_asio_out_cush_smp: AtomicU32,
    pub is_direct_bridge_in: AtomicBool,
    pub asio_exp_in_frames: AtomicU32,
    pub asio_exp_out_frames: AtomicU32,
    pub fail_closed: AtomicBool,
    pub failsafe_until_ms: AtomicU64,
    pub start_instant: Instant,
    pub peak_level_bits: AtomicU32,
    pub dsp_load_bits: AtomicU32,
    pub dsp_peak_bits: AtomicU32,
    pub in_xruns: AtomicU64,
    pub out_xruns: AtomicU64,
    pub stream_faulted: AtomicBool,
    pub fifo: FifoVariant,
    pub dc_blocker: Mutex<DcBlockerStereo>,
    pub chain: ArcSwap<Vec<Arc<VSTSlot>>>,
    pub probe_in: LatencyProbe,
    pub probe_out: LatencyProbe,
    pub overlay_input_bank: OverlayBank,
    pub overlay_output_bank: OverlayBank,
}

impl SharedAudioState {
    pub fn elapsed_ms(&self) -> u64 {
        self.start_instant.elapsed().as_millis() as u64
    }

    pub fn is_in_failsafe(&self) -> bool {
        self.elapsed_ms() < self.failsafe_until_ms.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn update_dsp_load(&self, elapsed_sec: f64, frames: usize) {
        let sr = self.active_sample_rate.load(Ordering::Relaxed).max(48000) as f64;
        let budget = frames as f64 / sr;
        if budget > 0.0 {
            let load = (elapsed_sec / budget) as f32;
            let prev_load = f32::from_bits(self.dsp_load_bits.load(Ordering::Relaxed));
            let new_load = prev_load + 0.1 * (load - prev_load);
            self.dsp_load_bits.store(new_load.to_bits(), Ordering::Relaxed);

            let prev_peak = f32::from_bits(self.dsp_peak_bits.load(Ordering::Relaxed));
            let new_peak = load.max(prev_peak * 0.999);
            self.dsp_peak_bits.store(new_peak.to_bits(), Ordering::Relaxed);
        }
    }
}

enum ActiveStreamEngine {
    None,
    WasapiShared {
        _in_stream: Option<cpal::Stream>,
        _out_stream: Option<cpal::Stream>,
    },
    WasapiExclusive {
        _pair: WasapiExclusivePair,
    },
    AsioExperimental {
        _pair: WasapiFlexAsioPair,
    },
    Asio {
        _handle: AsioStreamHandle,
    },
}

pub struct AudioPipeline {
    pub name: String,
    pub shared: Arc<SharedAudioState>,
    pub vst_slots: Vec<Arc<VSTSlot>>,
    graveyard: Vec<(Instant, Arc<VSTSlot>)>,

    pub driver_backend: DriverBackend,
    pub input_device_name: Option<String>,
    pub output_device_name: Option<String>,
    pub asio_driver_name: Option<String>,

    pub sample_rate: u32,
    pub buffer_size: u32,
    pub asio_exp_in_samples: u32,
    pub asio_exp_out_samples: u32,
    pub asio_set_in_enabled: bool,
    pub asio_set_in_latency: f32,
    pub asio_set_out_enabled: bool,
    pub asio_set_out_latency: f32,
    pub asio_profile: FlexAsioProfileConfig,
    pub dsp_rsb_config: DspRsbAsioConfig,
    pub rsb_asio_config: RsbAsioConfig,
    pub fifo_mode: String,

    pub in_channels: u16,
    pub out_channels: u16,
    pub status_message: String,

    engine: ActiveStreamEngine,

    restart_attempts: u32,
    healthy_ticks: u32,
    gave_up: bool,
    last_restart_try: Option<Instant>,
}

impl AudioPipeline {
    pub const MAX_AUTO_RESTART: u32 = 3;

    pub fn new(name: &str) -> Self {
        let shared = Arc::new(SharedAudioState {
            name: name.to_string(),
            is_enabled: AtomicBool::new(true),
            is_muted: AtomicBool::new(false),
            dc_blocker_enabled: AtomicBool::new(true),
            master_gain_bits: AtomicU32::new(1.0f32.to_bits()),
            active_sample_rate: AtomicU32::new(48000),
            buffer_size: AtomicU32::new(256),
            actual_in_frames: AtomicU32::new(256),
            is_asio_experimental: AtomicBool::new(false),
            is_asio_dsp_rsb: AtomicBool::new(false),
            is_rsb_asio: AtomicBool::new(false),
            rsb_asio_out_cush_smp: AtomicU32::new(3840),
            is_direct_bridge_in: AtomicBool::new(false),
            asio_exp_in_frames: AtomicU32::new(256),
            asio_exp_out_frames: AtomicU32::new(256),
            fail_closed: AtomicBool::new(true),
            failsafe_until_ms: AtomicU64::new(0),
            start_instant: Instant::now(),
            peak_level_bits: AtomicU32::new(0.0f32.to_bits()),
            dsp_load_bits: AtomicU32::new(0.0f32.to_bits()),
            dsp_peak_bits: AtomicU32::new(0.0f32.to_bits()),
            in_xruns: AtomicU64::new(0),
            out_xruns: AtomicU64::new(0),
            stream_faulted: AtomicBool::new(false),
            fifo: FifoVariant::new("adaptive", 16384),
            dc_blocker: Mutex::new(DcBlockerStereo::new(48000)),
            chain: ArcSwap::from_pointee(Vec::new()),
            probe_in: LatencyProbe::new(1.8, 48000),
            probe_out: LatencyProbe::new(1.8, 48000),
            overlay_input_bank: OverlayBank::new(),
            overlay_output_bank: OverlayBank::new(),
        });

        Self {
            name: name.to_string(),
            shared,
            vst_slots: Vec::new(),
            graveyard: Vec::new(),
            driver_backend: DriverBackend::WasapiShared,
            input_device_name: None,
            output_device_name: None,
            asio_driver_name: None,
            sample_rate: 48000,
            buffer_size: 256,
            asio_exp_in_samples: 256,
            asio_exp_out_samples: 256,
            asio_set_in_enabled: false,
            asio_set_in_latency: 0.0,
            asio_set_out_enabled: false,
            asio_set_out_latency: 0.0,
            asio_profile: FlexAsioProfileConfig::default(),
            dsp_rsb_config: DspRsbAsioConfig::default(),
            rsb_asio_config: RsbAsioConfig::default(),
            fifo_mode: "adaptive".to_string(),
            in_channels: 2,
            out_channels: 2,
            status_message: "Ready".to_string(),
            engine: ActiveStreamEngine::None,
            restart_attempts: 0,
            healthy_ticks: 0,
            gave_up: false,
            last_restart_try: None,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.shared.is_enabled.load(Ordering::Relaxed)
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.shared.is_enabled.store(enabled, Ordering::Relaxed);
        self.reset_watchdog();
        if !enabled {
            self.stop_stream();
            self.shared.peak_level_bits.store(0.0f32.to_bits(), Ordering::Relaxed);
            self.status_message = "⚪ Line Disabled (OFF)".to_string();
        } else {
            self.restart_stream();
        }
    }

    pub fn is_muted(&self) -> bool {
        self.shared.is_muted.load(Ordering::Relaxed)
    }

    pub fn set_muted(&self, muted: bool) {
        self.shared.is_muted.store(muted, Ordering::Relaxed);
    }

    pub fn dc_blocker_enabled(&self) -> bool {
        self.shared.dc_blocker_enabled.load(Ordering::Relaxed)
    }

    pub fn set_dc_blocker_enabled(&self, enabled: bool) {
        self.shared.dc_blocker_enabled.store(enabled, Ordering::Relaxed);
        if !enabled {
            self.shared.dc_blocker.lock().reset();
        }
    }

    pub fn master_gain(&self) -> f32 {
        f32::from_bits(self.shared.master_gain_bits.load(Ordering::Relaxed))
    }

    pub fn set_master_gain(&self, gain: f32) {
        self.shared
            .master_gain_bits
            .store(gain.clamp(0.0, 2.0).to_bits(), Ordering::Relaxed);
    }

    pub fn fail_closed(&self) -> bool {
        self.shared.fail_closed.load(Ordering::Relaxed)
    }

    pub fn set_fail_closed(&self, val: bool) {
        self.shared.fail_closed.store(val, Ordering::Relaxed);
    }

    pub fn peak_level(&self) -> f32 {
        f32::from_bits(self.shared.peak_level_bits.load(Ordering::Relaxed))
    }

    pub fn dsp_load(&self) -> f32 {
        f32::from_bits(self.shared.dsp_load_bits.load(Ordering::Relaxed))
    }

    pub fn dsp_peak(&self) -> f32 {
        f32::from_bits(self.shared.dsp_peak_bits.load(Ordering::Relaxed))
    }

    pub fn in_xruns(&self) -> u64 {
        self.shared.in_xruns.load(Ordering::Relaxed)
    }

    pub fn out_xruns(&self) -> u64 {
        self.shared.out_xruns.load(Ordering::Relaxed)
    }

    pub fn fifo_stats(&self) -> FifoStats {
        self.shared.fifo.lock().stats()
    }

    pub fn effective_sample_rate(&self) -> u32 {
        if self.streams_running() {
            self.shared.active_sample_rate.load(Ordering::Relaxed)
        } else {
            self.sample_rate
        }
    }

    /// Hitung estimasi delay real-time aktual Line ini (Buffer Hardware Aktual + FIFO + Lookahead Vokal Aktif) dalam milidetik.
    pub fn estimated_line_latency_ms(&self) -> Option<f64> {
        if !self.is_enabled() || !self.streams_running() {
            return None;
        }
        let sr = self.effective_sample_rate().max(44100) as f64;
        let f_stats = self.fifo_stats();
        let is_vc_direct_a = self.name == "Line A"
            && self.driver_backend == DriverBackend::AsioExperimental
            && self.output_device_name.as_deref().map(is_virtual_line_a_out_name).unwrap_or(false);
        let is_vc_direct_b =
            self.name == "Line B" && self.shared.is_direct_bridge_in.load(Ordering::Relaxed);
        let actual_hw = self.shared.actual_in_frames.load(Ordering::Relaxed) as usize;
        let (hw_samples, fifo_samples) = if self.driver_backend == DriverBackend::RsbAsio {
            let smp = self.rsb_asio_config.effective_buffer_size(self.buffer_size) as usize;
            let hw_q = smp.clamp(64, 512);
            if self.name == "Line A" {
                let v = self.rsb_asio_config.in_latency_val;
                let extra_in = if self.rsb_asio_config.set_in_latency_enabled && v > 0.0 && v < 0.5 {
                    ((v.min(0.040) as f64) * sr) as usize
                } else {
                    0
                };
                (hw_q, extra_in)
            } else {
                let v = self.rsb_asio_config.out_latency_val;
                let extra_out = if self.rsb_asio_config.set_out_latency_enabled && v > 0.0 && v < 0.5 {
                    ((v.min(0.040) as f64) * sr) as usize
                } else {
                    0
                };
                (hw_q, extra_out)
            }
        } else if self.driver_backend == DriverBackend::AsioDspRsb {
            let smp = self.dsp_rsb_config.effective_buffer_size(self.buffer_size) as usize;
            let hw_q = smp.clamp(64, 512);
            if self.name == "Line A" {
                let extra_in = if self.dsp_rsb_config.set_in_latency_enabled {
                    ((self.dsp_rsb_config.in_latency_val.max(0.0) as f64 / 1000.0) * sr) as usize
                } else {
                    0
                };
                (hw_q, extra_in)
            } else {
                let extra_out = if self.dsp_rsb_config.set_out_latency_enabled {
                    ((self.dsp_rsb_config.out_latency_val.max(0.0) as f64 / 1000.0) * sr) as usize
                } else {
                    0
                };
                (hw_q, extra_out)
            }
        } else if is_vc_direct_a {
            let in_b = self.buffer_size as usize;
            let extra_in = if self.asio_set_in_enabled {
                ((self.asio_set_in_latency.max(0.0) as f64 / 1000.0) * sr) as usize
            } else {
                0
            };
            (in_b, extra_in)
        } else if is_vc_direct_b {
            let out_b = self.buffer_size as usize;
            let extra_out = if self.asio_set_out_enabled {
                ((self.asio_set_out_latency.max(0.0) as f64 / 1000.0) * sr) as usize
            } else {
                0
            };
            (0, out_b + extra_out)
        } else if self.driver_backend == DriverBackend::AsioExperimental {
            // Pada ASIO Experimental, paket capture (480 sampel = 10.0ms) langsung masuk ke `f_stats.fill`
            // di dalam callback input, sehingga menghitung `hw + ff` akan menghitung ganda blok 480 sampel tersebut!
            let smp = self.buffer_size as usize;
            let ff = if f_stats.fill > 0 {
                f_stats.fill.min(f_stats.target + (smp / 2))
            } else {
                f_stats.target.min(528)
            };
            (0, ff)
        } else {
            let hw = if actual_hw > 0 {
                actual_hw.min(self.buffer_size as usize)
            } else {
                self.buffer_size as usize
            };
            let ff = if f_stats.fill > 0 {
                f_stats.fill
            } else {
                f_stats.target.max(hw)
            };
            (hw, ff)
        };
        let max_realtime_vst_hop = ((sr * 0.040) as usize).max(1920);
        let vst_realtime_samples: usize = self
            .vst_slots
            .iter()
            .filter(|s| !s.is_bypassed())
            .map(|s| {
                let raw = s.latency_samples.load(Ordering::Relaxed) as usize;
                raw.min(max_realtime_vst_hop)
            })
            .sum();
        let total_samples = hw_samples + fifo_samples + vst_realtime_samples;
        Some((total_samples as f64 / sr) * 1000.0)
    }

    /// Total PDC mentah yang dilaporkan oleh `IAudioProcessor::getLatencySamples()` (untuk info diagnostik tooltip).
    pub fn reported_vst_pdc_ms(&self) -> f64 {
        let sr = self.effective_sample_rate().max(44100) as f64;
        let raw_samples: usize = self
            .vst_slots
            .iter()
            .filter(|s| !s.is_bypassed())
            .map(|s| s.latency_samples.load(Ordering::Relaxed) as usize)
            .sum();
        (raw_samples as f64 / sr) * 1000.0
    }

    pub fn streams_running(&self) -> bool {
        !matches!(self.engine, ActiveStreamEngine::None)
    }

    pub fn set_fifo_mode(&mut self, mode: &str) {
        let m = if mode == "classic" { "classic" } else { "adaptive" };
        if m == self.fifo_mode {
            return;
        }
        self.fifo_mode = m.to_string();
        self.shared.fifo.set_mode(m);
        self.shared
            .fifo
            .configure(self.buffer_size as usize, Some(self.effective_sample_rate() as f64));
    }

    fn publish_chain(&self) {
        self.shared.chain.store(Arc::new(self.vst_slots.clone()));
    }

    pub fn purge_graveyard(&mut self, min_age_sec: f64) {
        if self.graveyard.is_empty() {
            return;
        }
        let now = Instant::now();
        self.graveyard.retain(|(t, slot)| {
            if now.saturating_duration_since(*t).as_secs_f64() >= min_age_sec
                && Arc::strong_count(slot) == 1
            {
                slot.shutdown_and_release();
                false
            } else {
                true
            }
        });
    }

    fn reset_watchdog(&mut self) {
        self.restart_attempts = 0;
        self.healthy_ticks = 0;
        self.gave_up = false;
    }

    pub fn stop_stream(&mut self) {
        self.engine = ActiveStreamEngine::None;
        if self.name == "Line A" {
            get_asio_bus_line_a_out().mark_writer_stopped();
            get_dsp_rsb_bus_a_out().mark_writer_stopped();
        }
        self.shared.fifo.lock().reset();
        self.shared.dc_blocker.lock().reset();
    }

    fn sync_overlay_rates(&self, active_sr: u32) {
        for bank in [&self.shared.overlay_input_bank, &self.shared.overlay_output_bank] {
            for player in &bank.players {
                if let Some(Err(e)) = player.ensure_sample_rate(active_sr) {
                    log_warn(format!(
                        "[{}] Overlay gagal dimuat ulang @ {}Hz: {}",
                        self.name, active_sr, e
                    ));
                }
            }
        }
    }

    pub fn restart_stream(&mut self) {
        self.stop_stream();
        if !self.is_enabled() {
            self.status_message = "⚪ Line Disabled (OFF)".to_string();
            return;
        }

        let target_sr = self.sample_rate.max(44100) as f64;
        let target_blk = (self.buffer_size as usize).max(2048);
        for slot in &self.vst_slots {
            slot.reset_pending.store(false, Ordering::Relaxed);
            slot.flush_pending.store(true, Ordering::Relaxed);
            if let Some(ref mut p) = *slot.audio_proc.lock() {
                if (p.active_sample_rate - target_sr).abs() > 0.5 || p.max_block_size < target_blk {
                    p.setup_processing(target_sr, target_blk);
                } else {
                    p.reset();
                }
                slot.latency_samples.store(p.latency_samples, Ordering::Relaxed);
            }
        }

        self.shared.dsp_load_bits.store(0.0f32.to_bits(), Ordering::Relaxed);
        self.shared.dsp_peak_bits.store(0.0f32.to_bits(), Ordering::Relaxed);
        self.shared.stream_faulted.store(false, Ordering::Relaxed);
        self.shared.buffer_size.store(self.buffer_size, Ordering::Relaxed);
        let is_exp = self.driver_backend == DriverBackend::AsioExperimental;
        let is_dsp_rsb = self.driver_backend == DriverBackend::AsioDspRsb;
        let is_rsb_asio = self.driver_backend == DriverBackend::RsbAsio;
        let unified_smp = self.buffer_size.clamp(32, 1024);
        self.asio_exp_in_samples = unified_smp;
        self.asio_exp_out_samples = unified_smp;
        let is_virtual_in_b = (is_exp
            && self.name == "Line B"
            && self.input_device_name.as_deref().map(is_virtual_line_b_in_name).unwrap_or(false))
            || ((is_dsp_rsb || is_rsb_asio) && self.name == "Line B");
        self.shared.is_asio_experimental.store(is_exp, Ordering::Relaxed);
        self.shared.is_asio_dsp_rsb.store(is_dsp_rsb, Ordering::Relaxed);
        self.shared.is_rsb_asio.store(is_rsb_asio, Ordering::Relaxed);
        let rsb_out_cush_smp = ((0.200 * (self.sample_rate.max(44100) as f64)).round() as u32)
            .clamp(4800, 19200);
        self.shared
            .rsb_asio_out_cush_smp
            .store(rsb_out_cush_smp, Ordering::Relaxed);
        self.shared.is_direct_bridge_in.store(is_virtual_in_b, Ordering::Relaxed);
        self.shared
            .asio_exp_in_frames
            .store(unified_smp, Ordering::Relaxed);
        self.shared
            .asio_exp_out_frames
            .store(unified_smp, Ordering::Relaxed);

        let _in_lat_smp = unified_smp
            + if self.asio_set_in_enabled {
                ((self.asio_set_in_latency.max(0.0) as f64 / 1000.0) * self.sample_rate as f64)
                    as u32
            } else {
                0
            };
        let out_lat_smp = unified_smp
            + if self.asio_set_out_enabled {
                ((self.asio_set_out_latency.max(0.0) as f64 / 1000.0) * self.sample_rate as f64)
                    as u32
            } else {
                0
            };
        let dsp_sr = self.sample_rate.clamp(44100, 192000);
        let dsp_smp = self.dsp_rsb_config.effective_buffer_size(self.buffer_size);
        let eff_cush_ms = self.dsp_rsb_config.dsp_b_cushion_ms.clamp(20.0, 450.0);
        let dsp_cush_smp = (((eff_cush_ms as f64) / 1000.0) * (dsp_sr as f64)).round() as u32;
        let dsp_cush_clamped = dsp_cush_smp.min(24000);
        let dsp_stretch_flag = 1u32;
        let eff_rep_in = if self.dsp_rsb_config.adaptive_reported_latency {
            0.0
        } else {
            self.dsp_rsb_config.reported_in_ms
        };
        let eff_rep_out = if self.dsp_rsb_config.adaptive_reported_latency {
            0.0
        } else {
            self.dsp_rsb_config.reported_out_ms
        };
        let (dsp_rep_in_smp, dsp_rep_out_smp) = compute_asio_reported_latency_samples(
            dsp_sr,
            dsp_smp,
            eff_rep_in,
            eff_rep_out,
        );

        get_asio_bus_line_a_out().sync_config(self.sample_rate, unified_smp, dsp_rep_in_smp, dsp_rep_out_smp);
        get_asio_bus_line_b_in().sync_config(self.sample_rate, unified_smp, dsp_rep_in_smp, dsp_rep_out_smp);
        get_dsp_rsb_bus_a_out().sync_config(dsp_sr, dsp_smp, dsp_rep_in_smp, dsp_rep_out_smp);
        get_dsp_rsb_bus_b_in().set_adaptive_flags_with_mult(
            self.dsp_rsb_config.adaptive_cushion,
            self.dsp_rsb_config.adaptive_reported_latency,
            self.dsp_rsb_config.gpu_stall_multiplier,
        );
        get_dsp_rsb_bus_b_in().set_adaptive_mode(self.dsp_rsb_config.adaptive_mode as u8);
        get_dsp_rsb_bus_b_in().set_user_extra_buffer_ms(self.dsp_rsb_config.adaptive_extra_ms);
        get_dsp_rsb_bus_b_in().sync_config(dsp_sr, dsp_smp, dsp_stretch_flag, dsp_cush_clamped);
        if self.name == "Line A" {
            get_asio_bus_line_a_out().reset();
            get_dsp_rsb_bus_a_out().reset_reader_with_cushion((dsp_smp as usize * 4).clamp(1200, 1920));
        } else {
            let cushion = (out_lat_smp as usize).max(unified_smp as usize) + 480;
            get_asio_bus_line_b_in().reset_reader_with_cushion(cushion);
            if dsp_cush_clamped > 0 {
                get_dsp_rsb_bus_b_in().reset_reader_with_cushion(dsp_cush_clamped as usize);
            } else {
                get_dsp_rsb_bus_b_in().reset();
            }
        }

        match self.driver_backend {
            DriverBackend::AsioNative => self.start_asio_backend(),
            DriverBackend::AsioExperimental => self.start_asio_experimental_backend(),
            DriverBackend::AsioDspRsb => self.start_asio_dsp_rsb_backend(),
            DriverBackend::RsbAsio => self.start_rsb_asio_backend(),
            DriverBackend::WasapiExclusive => self.start_wasapi_backend(true),
            DriverBackend::WasapiShared => self.start_wasapi_backend(false),
        }
    }

    fn start_asio_backend(&mut self) {
        let Some(ref drv_name) = self.asio_driver_name else {
            self.status_message = "⚪ Inactive - select ASIO Driver".to_string();
            return;
        };
        if drv_name.is_empty() {
            self.status_message = "⚪ Inactive - select ASIO Driver".to_string();
            return;
        }

        {
            let fifo = self.shared.fifo.lock();
            fifo.configure(self.buffer_size as usize, Some(self.sample_rate as f64));
            fifo.reset();
        }
        let shared = Arc::clone(&self.shared);
        let mut scratch = AudioScratchBuffers::with_capacity(self.buffer_size as usize * 2);

        let res = AsioStreamHandle::start(
            drv_name,
            self.sample_rate,
            self.buffer_size,
            Box::new(move |in_interleaved: &[f32], out_interleaved: &mut [f32], frames: usize| {
                let t0 = Instant::now();
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    process_input_block(&shared, &mut scratch, in_interleaved, 2, frames);
                    render_output_block(&shared, &mut scratch, out_interleaved, 2, frames);
                }));
                shared.update_dsp_load(t0.elapsed().as_secs_f64(), frames);
            }),
        );

        match res {
            Ok(handle) => {
                let active_sr = handle.active_sample_rate;
                let active_buf = handle.active_buffer_size;
                self.shared.active_sample_rate.store(active_sr, Ordering::Relaxed);
                self.shared.probe_in.set_sample_rate(active_sr, 1.8);
                self.shared.probe_out.set_sample_rate(active_sr, 1.8);
                self.sync_overlay_rates(active_sr);
                {
                    let fifo = self.shared.fifo.lock();
                    fifo.configure(active_buf as usize, Some(active_sr as f64));
                }
                self.engine = ActiveStreamEngine::Asio { _handle: handle };
                self.status_message = format!(
                    "🎹 ASIO Native Active ({} @ {}Hz / {} smp)",
                    drv_name, active_sr, active_buf
                );
            }
            Err(e) => {
                self.status_message = format!("⚠️ ASIO Error: {}", &e.chars().take(38).collect::<String>());
                log_error(format!("[{}] ASIO gagal: {}", self.name, e));
            }
        }
    }

    fn start_asio_experimental_backend(&mut self) {
        if !self.asio_profile.input_device.is_empty() {
            self.input_device_name = Some(self.asio_profile.input_device.clone());
        }
        if !self.asio_profile.output_device.is_empty() {
            self.output_device_name = Some(self.asio_profile.output_device.clone());
        }
        self.buffer_size = self.asio_profile.buffer_size.clamp(32, 2048);
        self.asio_exp_in_samples = self.buffer_size;
        self.asio_exp_out_samples = self.buffer_size;
        self.asio_set_in_enabled = self.asio_profile.set_in_latency_enabled;
        self.asio_set_in_latency = self.asio_profile.in_latency_val;
        self.asio_set_out_enabled = self.asio_profile.set_out_latency_enabled;
        self.asio_set_out_latency = self.asio_profile.out_latency_val;

        let (in_name, out_name) = match (&self.input_device_name, &self.output_device_name) {
            (Some(i), Some(o)) if !i.is_empty() && !o.is_empty() => (i.clone(), o.clone()),
            _ => {
                self.status_message = "⚪ Inactive - open ASIO Setting to select In & Out".to_string();
                return;
            }
        };

        let is_virtual_in_b = self.name == "Line B" && is_virtual_line_b_in_name(&in_name);
        let is_virtual_out_a = self.name == "Line A" && is_virtual_line_a_out_name(&out_name);
        self.shared
            .is_direct_bridge_in
            .store(is_virtual_in_b, Ordering::Relaxed);

        let host = cpal::default_host();

        let in_idx_opt = if is_virtual_in_b {
            None
        } else {
            let found = host
                .input_devices()
                .ok()
                .and_then(|it| {
                    it.enumerate()
                        .find(|(_, d)| d.name().ok().as_deref() == Some(in_name.as_str()))
                })
                .map(|(idx, _)| idx);
            if found.is_none() {
                self.status_message = "⚠️ Error: ASIO Input not found".to_string();
                return;
            }
            found
        };

        let out_idx_opt = if is_virtual_out_a {
            None
        } else {
            let found = host
                .output_devices()
                .ok()
                .and_then(|it| {
                    it.enumerate()
                        .find(|(_, d)| d.name().ok().as_deref() == Some(out_name.as_str()))
                })
                .map(|(idx, _)| idx);
            if found.is_none() {
                self.status_message = "⚠️ Error: ASIO Output not found".to_string();
                return;
            }
            found
        };

        let unified_smp = self.buffer_size.clamp(32, 2048);
        let set_in_opt = if self.asio_set_in_enabled {
            Some(self.asio_set_in_latency.max(0.0))
        } else {
            None
        };
        let set_out_opt = if self.asio_set_out_enabled {
            Some(self.asio_set_out_latency.max(0.0))
        } else {
            None
        };

        let in_opts = FlexAsioEndpointOptions {
            set_latency_ms: set_in_opt,
            wasapi_set: self.asio_profile.wasapi_in_set,
            wasapi_exclusive: self.asio_profile.wasapi_in_exclusive,
            wasapi_autoconvert: self.asio_profile.wasapi_in_autoconvert,
            channels: self.asio_profile.in_channels,
        };
        let out_opts = FlexAsioEndpointOptions {
            set_latency_ms: set_out_opt,
            wasapi_set: self.asio_profile.wasapi_out_set,
            wasapi_exclusive: self.asio_profile.wasapi_out_exclusive,
            wasapi_autoconvert: self.asio_profile.wasapi_out_autoconvert,
            channels: self.asio_profile.out_channels,
        };

        {
            let extra_in_init = set_in_opt
                .map(|ms| ((ms as f64 / 1000.0) * self.sample_rate as f64) as usize)
                .unwrap_or(0);
            let extra_out_init = set_out_opt
                .map(|ms| ((ms as f64 / 1000.0) * self.sample_rate as f64) as usize)
                .unwrap_or(0);
            let fifo = self.shared.fifo.lock();
            fifo.configure_flex_asio(
                unified_smp as usize,
                extra_in_init,
                extra_out_init,
                Some(self.sample_rate as f64),
            );
            fifo.reset();
        }

        let shared_in = Arc::clone(&self.shared);
        let shared_out = Arc::clone(&self.shared);
        let mut scratch_in = AudioScratchBuffers::with_capacity((unified_smp as usize).max(1024) * 4);
        let mut scratch_direct = AudioScratchBuffers::with_capacity((unified_smp as usize).max(1024) * 4);

        let flex_res = WasapiFlexAsioPair::try_start_with_options(
            in_idx_opt,
            out_idx_opt,
            self.sample_rate,
            unified_smp,
            in_opts,
            out_opts,
            move |data, ch, frames| {
                let t0 = Instant::now();
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    process_input_block(&shared_in, &mut scratch_in, data, ch, frames);
                }));
                shared_in.update_dsp_load(t0.elapsed().as_secs_f64(), frames);
            },
            move |data, ch, frames| {
                let t0 = Instant::now();
                let use_vc_bus = shared_out.name == "Line B"
                    && shared_out.is_direct_bridge_in.load(Ordering::Relaxed);
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    if use_vc_bus {
                        scratch_direct.ensure_frames(frames);
                        get_asio_bus_line_b_in().read_stereo_into(
                            &mut scratch_direct.bus_l[..frames],
                            &mut scratch_direct.bus_r[..frames],
                        );
                        process_and_render_direct_stereo_from_scratch(
                            &shared_out,
                            &mut scratch_direct,
                            data,
                            ch,
                            frames,
                        );
                    } else {
                        render_output_block(&shared_out, &mut scratch_direct, data, ch, frames);
                    }
                }));
                if use_vc_bus {
                    shared_out.update_dsp_load(t0.elapsed().as_secs_f64(), frames);
                }
            },
        );

        match flex_res {
            Ok(pair) => {
                let active_sr = pair.active_sample_rate;
                let act_in = pair.actual_in_buf;
                let _act_out = pair.actual_out_buf;
                self.shared.active_sample_rate.store(active_sr, Ordering::Relaxed);
                self.shared.actual_in_frames.store(act_in, Ordering::Relaxed);
                self.shared.probe_in.set_sample_rate(active_sr, 1.8);
                self.shared.probe_out.set_sample_rate(active_sr, 1.8);
                self.sync_overlay_rates(active_sr);
                if active_sr != self.sample_rate {
                    let extra_in_smp = set_in_opt
                        .map(|ms| ((ms as f64 / 1000.0) * active_sr as f64) as usize)
                        .unwrap_or(0);
                    let extra_out_smp = set_out_opt
                        .map(|ms| ((ms as f64 / 1000.0) * active_sr as f64) as usize)
                        .unwrap_or(0);
                    let fifo = self.shared.fifo.lock();
                    fifo.configure_flex_asio(
                        unified_smp as usize,
                        extra_in_smp,
                        extra_out_smp,
                        Some(active_sr as f64),
                    );
                }
                let in_lat_str = set_in_opt
                    .map(|v| format!("{:.1}", v))
                    .unwrap_or_else(|| "Auto".into());
                let out_lat_str = set_out_opt
                    .map(|v| format!("{:.1}", v))
                    .unwrap_or_else(|| "Auto".into());
                self.status_message = if is_virtual_out_a {
                    format!(
                        "🚀 ASIO Exp [In -> ASIO Line A Out ({}smp) @ {}Hz]",
                        unified_smp, active_sr
                    )
                } else if is_virtual_in_b {
                    format!(
                        "🚀 ASIO Exp [ASIO Line B In -> Out ({}smp) @ {}Hz]",
                        unified_smp, active_sr
                    )
                } else {
                    format!(
                        "🚀 ASIO Exp [{}smp | In:{} Out:{} @ {}Hz]",
                        unified_smp, in_lat_str, out_lat_str, active_sr
                    )
                };
                self.engine = ActiveStreamEngine::AsioExperimental { _pair: pair };
            }
            Err(flex_err) => {
                log_warn(format!(
                    "[{}] WasapiFlexAsioPair gagal ({}). Fallback ke cpal Fixed-Buffer ASIO Exp...",
                    self.name, flex_err
                ));
                self.start_wasapi_backend(false);
            }
        }
    }

    /// Mode `SoundBridge ASIO` (Pure Internal DSP Bridge):
    /// - Line A: Hanya membuka perangkat Input fisik (`SSL Input 1` -> `VST Chain A` -> `DSP A Out`
    ///   ke driver `"DSP RSB Exp ASIO"` di RAM, tanpa perangkat Output fisik dan tanpa FIFO).
    /// - Line B: Hanya membuka perangkat Output fisik (menerima dari `"DSP RSB Exp ASIO"` -> `DSP B In`
    ///   -> `VST Chain B` -> `Speaker / Otogai`, tanpa perangkat Input fisik dan tanpa FIFO).
    /// Mode `WASAPI + ASIO Bridge` (`DriverBackend::AsioDspRsb`):
    /// - **Line A (`In A -> VST Chain A -> Out A`)**: 100% **WASAPI Murni**!
    ///   User memilih `Line A Input` secara manual (WASAPI Capture), diproses oleh VST3 Chain Line A,
    ///   dan `Line A Output` mengalir **otomatis** ke jembatan tunggal ASIO (`BUS_DSP_A_OUT` -> Input Vonovox).
    /// - **Line B (`In B -> VST Chain B -> Out B`)**: 100% **WASAPI Murni**!
    ///   `Line B Input` menerima **otomatis** dari jembatan tunggal ASIO (`BUS_DSP_B_IN` <- Output Vonovox),
    ///   diproses oleh VST3 Chain Line B, dan mengalir ke `Line B Output` yang dipilih manual (WASAPI Render).
    /// - **Jembatan `A Out -> B In`**: Satu-satunya jalur ASIO di seluruh sistem (`Rinka SoundBridge ASIO`),
    ///   menggunakan **Adaptive FlexASIO Reported Latency (`504.0 ms / 210.7 ms`)** & **Adaptive GPU Cushion**.
    fn start_asio_dsp_rsb_backend(&mut self) {
        let host = cpal::default_host();
        let target_sr = self.sample_rate.clamp(44100, 192000);
        let unified_smp = self.dsp_rsb_config.effective_buffer_size(self.buffer_size);
        let eff_cush_ms = self.dsp_rsb_config.dsp_b_cushion_ms.clamp(20.0, 450.0);
        let dsp_cush_smp = (((eff_cush_ms as f64) / 1000.0) * (target_sr as f64)).round() as usize;
        let dsp_cush_clamped = dsp_cush_smp.min(24000);
        let stretch_flag = 1u32;
        let eff_rep_in = if self.dsp_rsb_config.adaptive_reported_latency {
            0.0
        } else {
            self.dsp_rsb_config.reported_in_ms
        };
        let eff_rep_out = if self.dsp_rsb_config.adaptive_reported_latency {
            0.0
        } else {
            self.dsp_rsb_config.reported_out_ms
        };

        // 1. JALUR UTAMA FULL ASIO: Gunakan mesin Event-Driven Low-Latency ASIO (`WasapiFlexAsioPair`)
        //    dari Input A -> Output A (ASIO Bridge) -> Input B (ASIO Bridge) -> Output B!
        let (in_idx_opt, out_idx_opt) = if self.name == "Line A" {
            let Some(ref in_name) = self.input_device_name else {
                self.status_message = "⚪ Inactive - select Line A Input (ASIO)".to_string();
                return;
            };
            if in_name.is_empty() {
                self.status_message = "⚪ Inactive - select Line A Input (ASIO)".to_string();
                return;
            }
            let found_in = host
                .input_devices()
                .ok()
                .and_then(|it| {
                    it.enumerate()
                        .find(|(_, d)| d.name().ok().as_deref() == Some(in_name.as_str()))
                })
                .map(|(idx, _)| idx);
            if found_in.is_none() {
                self.status_message = "⚠️ Error: Line A Input not found".to_string();
                return;
            }
            (found_in, None)
        } else {
            let Some(ref out_name) = self.output_device_name else {
                self.status_message = "⚪ Inactive - select Line B Output (ASIO)".to_string();
                return;
            };
            if out_name.is_empty() {
                self.status_message = "⚪ Inactive - select Line B Output (ASIO)".to_string();
                return;
            }
            let found_out = host
                .output_devices()
                .ok()
                .and_then(|it| {
                    it.enumerate()
                        .find(|(_, d)| d.name().ok().as_deref() == Some(out_name.as_str()))
                })
                .map(|(idx, _)| idx);
            if found_out.is_none() {
                self.status_message = "⚠️ Error: Line B Output not found".to_string();
                return;
            }
            (None, found_out)
        };

        let set_in_opt = if self.dsp_rsb_config.set_in_latency_enabled {
            let v = self.dsp_rsb_config.in_latency_val.max(0.0);
            Some(if v <= 2.0 { (v * 1000.0).min(2000.0) } else { v.min(2000.0) })
        } else {
            Some(0.0)
        };
        let set_out_opt = if self.dsp_rsb_config.set_out_latency_enabled {
            let v = self.dsp_rsb_config.out_latency_val.max(0.0);
            Some(if v <= 2.0 { (v * 1000.0).min(2000.0) } else { v.min(2000.0) })
        } else {
            Some(0.0)
        };

        let in_opts = FlexAsioEndpointOptions {
            set_latency_ms: set_in_opt,
            wasapi_set: self.dsp_rsb_config.wasapi_in_set,
            wasapi_exclusive: self.dsp_rsb_config.wasapi_in_exclusive,
            wasapi_autoconvert: self.dsp_rsb_config.wasapi_in_autoconvert,
            channels: self.dsp_rsb_config.in_channels,
        };
        let out_opts = FlexAsioEndpointOptions {
            set_latency_ms: set_out_opt,
            wasapi_set: self.dsp_rsb_config.wasapi_out_set,
            wasapi_exclusive: self.dsp_rsb_config.wasapi_out_exclusive,
            wasapi_autoconvert: self.dsp_rsb_config.wasapi_out_autoconvert,
            channels: self.dsp_rsb_config.out_channels,
        };

        let shared_in = Arc::clone(&self.shared);
        let shared_out = Arc::clone(&self.shared);
        let mut scratch_in = AudioScratchBuffers::with_capacity((unified_smp as usize).max(1024) * 4);
        let mut scratch_direct = AudioScratchBuffers::with_capacity((unified_smp as usize).max(1024) * 4);

        let dsp_res = WasapiFlexAsioPair::try_start_with_options(
            in_idx_opt,
            out_idx_opt,
            target_sr,
            unified_smp,
            in_opts,
            out_opts,
            move |data, ch, frames| {
                let t0 = Instant::now();
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    process_input_block(&shared_in, &mut scratch_in, data, ch, frames);
                }));
                shared_in.update_dsp_load(t0.elapsed().as_secs_f64(), frames);
            },
            move |data, ch, frames| {
                if shared_out.name == "Line B" {
                    let t0 = Instant::now();
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        scratch_direct.ensure_frames(frames);
                        let (cush_target, stretch_en) = get_dsp_rsb_bus_b_in().get_dsp_b_params();
                        get_dsp_rsb_bus_b_in().read_stereo_dsp_b_into(
                            &mut scratch_direct.bus_l[..frames],
                            &mut scratch_direct.bus_r[..frames],
                            cush_target,
                            stretch_en,
                        );
                        process_and_render_direct_stereo_from_scratch(
                            &shared_out,
                            &mut scratch_direct,
                            data,
                            ch,
                            frames,
                        );
                    }));
                    shared_out.update_dsp_load(t0.elapsed().as_secs_f64(), frames);
                } else {
                    data.fill(0.0);
                }
            },
        );

        match dsp_res {
            Ok(pair) => {
                let active_sr = pair.active_sample_rate;
                let act_in = pair.actual_in_buf;
                self.shared.active_sample_rate.store(active_sr, Ordering::Relaxed);
                self.shared.actual_in_frames.store(act_in, Ordering::Relaxed);
                self.shared.probe_in.set_sample_rate(active_sr, 1.8);
                self.shared.probe_out.set_sample_rate(active_sr, 1.8);
                self.sync_overlay_rates(active_sr);
                let (rep_in_smp, rep_out_smp) = compute_asio_reported_latency_samples(
                    active_sr,
                    unified_smp,
                    eff_rep_in,
                    eff_rep_out,
                );
                get_dsp_rsb_bus_a_out().sync_config(active_sr, unified_smp, rep_in_smp, rep_out_smp);
                get_dsp_rsb_bus_b_in().set_adaptive_flags_with_mult(
                    self.dsp_rsb_config.adaptive_cushion,
                    self.dsp_rsb_config.adaptive_reported_latency,
                    self.dsp_rsb_config.gpu_stall_multiplier,
                );
                get_dsp_rsb_bus_b_in().set_adaptive_mode(self.dsp_rsb_config.adaptive_mode as u8);
                get_dsp_rsb_bus_b_in().set_user_extra_buffer_ms(self.dsp_rsb_config.adaptive_extra_ms);
                get_dsp_rsb_bus_b_in().sync_config(
                    active_sr,
                    unified_smp,
                    stretch_flag,
                    dsp_cush_clamped as u32,
                );
                if self.name == "Line A" {
                    get_dsp_rsb_bus_a_out().reset_reader_with_cushion((unified_smp as usize * 4).clamp(1200, 1920));
                }
                self.status_message = if self.name == "Line A" {
                    format!(
                        "⚡ ASIO [A In ({}smp) -> VST -> ASIO Bridge Out @ {}Hz]",
                        unified_smp, active_sr
                    )
                } else {
                    format!(
                        "⚡ ASIO [ASIO Bridge In -> VST -> B Out ({}smp) @ {}Hz]",
                        unified_smp, active_sr
                    )
                };
                self.engine = ActiveStreamEngine::AsioExperimental { _pair: pair };
                return;
            }
            Err(e) => {
                log_warn(format!(
                    "[{}] WasapiFlexAsioPair gagal ({}), mencoba fallback cpal...",
                    self.name, e
                ));
            }
        }

        // 2. Fallback cpal jika format hardware sangat khusus
        if self.name == "Line A" {
            let in_name = self.input_device_name.clone().unwrap_or_default();
            if let Some((_, in_dev)) = host
                .input_devices()
                .ok()
                .and_then(|it| it.enumerate().find(|(_, d)| d.name().ok().as_deref() == Some(in_name.as_str())))
            {
                let in_def = in_dev.default_input_config().ok();
                let def_ch = in_def.as_ref().map(|c| c.channels().clamp(1, 16)).unwrap_or(2);
                let def_sr = in_def.as_ref().map(|c| c.sample_rate().0).unwrap_or(target_sr);
                if let Ok((ins, active_sr, act_ch)) =
                    self.try_open_shared_input_only(&in_dev, target_sr, def_sr, def_ch)
                {
                    if ins.play().is_ok() {
                        self.in_channels = act_ch;
                        self.shared.active_sample_rate.store(active_sr, Ordering::Relaxed);
                        self.shared.actual_in_frames.store(unified_smp, Ordering::Relaxed);
                        self.shared.probe_in.set_sample_rate(active_sr, 1.8);
                        self.shared.probe_out.set_sample_rate(active_sr, 1.8);
                        self.sync_overlay_rates(active_sr);
                        let (rep_in_smp, rep_out_smp) = compute_asio_reported_latency_samples(
                            active_sr,
                            unified_smp,
                            eff_rep_in,
                            eff_rep_out,
                        );
                        get_dsp_rsb_bus_a_out().sync_config(active_sr, unified_smp, rep_in_smp, rep_out_smp);
                        get_dsp_rsb_bus_a_out().ensure_min_cushion((unified_smp as usize * 4).clamp(1200, 1920));
                        self.status_message = format!(
                            "⚡ ASIO [A In ({}ch) -> VST -> ASIO Bridge Out @ {}Hz]",
                            act_ch, active_sr
                        );
                        self.engine = ActiveStreamEngine::WasapiShared {
                            _in_stream: Some(ins),
                            _out_stream: None,
                        };
                        return;
                    }
                }
            }
            self.status_message = "⚠️ SoundBridge ASIO Input Error".to_string();
        } else {
            let out_name = self.output_device_name.clone().unwrap_or_default();
            if let Some((_, out_dev)) = host
                .output_devices()
                .ok()
                .and_then(|it| it.enumerate().find(|(_, d)| d.name().ok().as_deref() == Some(out_name.as_str())))
            {
                let out_def = out_dev.default_output_config().ok();
                let def_ch = out_def.as_ref().map(|c| c.channels().clamp(1, 16)).unwrap_or(2);
                let def_sr = out_def.as_ref().map(|c| c.sample_rate().0).unwrap_or(target_sr);
                if let Ok((outs, active_sr, act_ch)) =
                    self.try_open_shared_output_only(&out_dev, target_sr, def_sr, def_ch)
                {
                    if outs.play().is_ok() {
                        self.out_channels = act_ch;
                        self.shared.active_sample_rate.store(active_sr, Ordering::Relaxed);
                        self.shared.actual_in_frames.store(unified_smp, Ordering::Relaxed);
                        self.shared.probe_in.set_sample_rate(active_sr, 1.8);
                        self.shared.probe_out.set_sample_rate(active_sr, 1.8);
                        self.sync_overlay_rates(active_sr);
                        get_dsp_rsb_bus_b_in().sync_config(
                            active_sr,
                            unified_smp,
                            stretch_flag,
                            dsp_cush_clamped as u32,
                        );
                        self.status_message = format!(
                            "⚡ ASIO [ASIO Bridge In -> VST -> B Out ({}ch) @ {}Hz]",
                            act_ch, active_sr
                        );
                        self.engine = ActiveStreamEngine::WasapiShared {
                            _in_stream: None,
                            _out_stream: Some(outs),
                        };
                        return;
                    }
                }
            }
            self.status_message = "⚠️ SoundBridge ASIO Output Error".to_string();
        }
    }

    /// Mode `RSB ASIO (Built-in FlexASIO)` (`DriverBackend::RsbAsio`):
    /// - Arsitektur Internal Loopback (Tanpa Virtual Cable):
    ///   `WASAPI In A (Mic Input A) -> VST A -> Out A [Internal RSB ASIO] -> Vonovox ("ASIO: RSB ASIO") -> Loopback In B [Internal RSB ASIO] -> WASAPI Loopback B -> VST B -> WASAPI Out B (Speaker Output B)`
    /// - Menggunakan WASAPI Event-Driven Ultra-Low-Latency (`WasapiFlexAsioPair` pada `10 ms` atau Set Latency manual)
    ///   dan pembacaan **Pure Steady 1:1 FlexASIO (`read_stereo_rsb_flex_b_into`)** yang 100% bebas stutter dan bebas ubahan pitch!
    fn start_rsb_asio_backend(&mut self) {
        let host = cpal::default_host();
        let target_sr = self.sample_rate.clamp(44100, 192000);
        let unified_smp = self.rsb_asio_config.effective_buffer_size(self.buffer_size);
        let out_v = self.rsb_asio_config.out_latency_val;
        let out_extra_smp = if self.rsb_asio_config.set_out_latency_enabled && out_v > 0.0 && out_v < 0.5 {
            ((out_v.min(0.250) as f64) * (target_sr as f64)).round() as usize
        } else {
            0
        };
        let std_200ms_smp = (((0.024 * (target_sr as f64)).round() as usize) + out_extra_smp).clamp(960, 24000);
        self.shared
            .rsb_asio_out_cush_smp
            .store(std_200ms_smp as u32, Ordering::Relaxed);

        if self.name == "Line A" && !self.rsb_asio_config.input_device.is_empty() && self.input_device_name.is_none() {
            self.input_device_name = Some(self.rsb_asio_config.input_device.clone());
        }
        if self.name == "Line B" && !self.rsb_asio_config.output_device.is_empty() && self.output_device_name.is_none() {
            self.output_device_name = Some(self.rsb_asio_config.output_device.clone());
        }

        let (in_idx_opt, out_idx_opt) = if self.name == "Line A" {
            let Some(ref in_name) = self.input_device_name else {
                self.status_message = "⚪ Inactive - select Input Device (WASAPI)".to_string();
                return;
            };
            if in_name.is_empty() {
                self.status_message = "⚪ Inactive - select Input Device (WASAPI)".to_string();
                return;
            };
            let found_in = host
                .input_devices()
                .ok()
                .and_then(|it| {
                    it.enumerate()
                        .find(|(_, d)| d.name().ok().as_deref() == Some(in_name.as_str()))
                })
                .map(|(idx, _)| idx);
            if found_in.is_none() {
                self.status_message = "⚠️ Error: Input Device not found".to_string();
                return;
            }
            (found_in, None)
        } else {
            let Some(ref out_name) = self.output_device_name else {
                self.status_message = "⚪ Inactive - select Output Device (WASAPI)".to_string();
                return;
            };
            if out_name.is_empty() {
                self.status_message = "⚪ Inactive - select Output Device (WASAPI)".to_string();
                return;
            };
            let found_out = host
                .output_devices()
                .ok()
                .and_then(|it| {
                    it.enumerate()
                        .find(|(_, d)| d.name().ok().as_deref() == Some(out_name.as_str()))
                })
                .map(|(idx, _)| idx);
            if found_out.is_none() {
                self.status_message = "⚠️ Error: Output Device not found".to_string();
                return;
            }
            (None, found_out)
        };

        let in_v = self.rsb_asio_config.in_latency_val;
        let set_in_opt = if self.rsb_asio_config.set_in_latency_enabled && in_v > 0.0 && in_v < 0.5 {
            Some((in_v * 1000.0).min(250.0))
        } else {
            Some(0.0)
        };
        let set_out_opt = if self.rsb_asio_config.set_out_latency_enabled && out_v > 0.0 && out_v < 0.5 {
            Some((out_v * 1000.0).min(250.0))
        } else {
            Some(0.0)
        };
        let in_opts = FlexAsioEndpointOptions {
            set_latency_ms: set_in_opt,
            wasapi_set: self.rsb_asio_config.wasapi_in_set,
            wasapi_exclusive: self.rsb_asio_config.wasapi_in_exclusive,
            wasapi_autoconvert: self.rsb_asio_config.wasapi_in_autoconvert,
            channels: self.rsb_asio_config.in_channels,
        };
        let out_opts = FlexAsioEndpointOptions {
            set_latency_ms: set_out_opt,
            wasapi_set: self.rsb_asio_config.wasapi_out_set,
            wasapi_exclusive: self.rsb_asio_config.wasapi_out_exclusive,
            wasapi_autoconvert: self.rsb_asio_config.wasapi_out_autoconvert,
            channels: self.rsb_asio_config.out_channels,
        };

        let shared_in = Arc::clone(&self.shared);
        let shared_out = Arc::clone(&self.shared);
        let mut scratch_in = AudioScratchBuffers::with_capacity((unified_smp as usize).max(1024) * 4);
        let mut scratch_direct = AudioScratchBuffers::with_capacity((unified_smp as usize).max(1024) * 4);

        let rsb_res = WasapiFlexAsioPair::try_start_with_options(
            in_idx_opt,
            out_idx_opt,
            target_sr,
            unified_smp,
            in_opts,
            out_opts,
            move |data, ch, frames| {
                let t0 = Instant::now();
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    process_input_block(&shared_in, &mut scratch_in, data, ch, frames);
                }));
                shared_in.update_dsp_load(t0.elapsed().as_secs_f64(), frames);
            },
            move |data, ch, frames| {
                if shared_out.name == "Line B" {
                    let t0 = Instant::now();
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        scratch_direct.ensure_frames(frames);
                        let steady_cush = shared_out
                            .rsb_asio_out_cush_smp
                            .load(Ordering::Relaxed)
                            .clamp(960, 24000) as usize;
                        get_dsp_rsb_bus_b_in().read_stereo_rsb_flex_b_into(
                            &mut scratch_direct.bus_l[..frames],
                            &mut scratch_direct.bus_r[..frames],
                            steady_cush,
                        );
                        process_and_render_direct_stereo_from_scratch(
                            &shared_out,
                            &mut scratch_direct,
                            data,
                            ch,
                            frames,
                        );
                    }));
                    shared_out.update_dsp_load(t0.elapsed().as_secs_f64(), frames);
                } else {
                    data.fill(0.0);
                }
            },
        );

        match rsb_res {
            Ok(pair) => {
                let active_sr = pair.active_sample_rate;
                let act_in = pair.actual_in_buf;
                self.shared.active_sample_rate.store(active_sr, Ordering::Relaxed);
                self.shared.actual_in_frames.store(act_in, Ordering::Relaxed);
                self.shared.probe_in.set_sample_rate(active_sr, 1.8);
                self.shared.probe_out.set_sample_rate(active_sr, 1.8);
                self.sync_overlay_rates(active_sr);
                let (rep_in_smp, rep_out_smp) = compute_rsb_flexasio_reported_latency_samples(
                    active_sr,
                    unified_smp,
                    self.rsb_asio_config.set_in_latency_enabled,
                    self.rsb_asio_config.in_latency_val,
                    self.rsb_asio_config.set_out_latency_enabled,
                    self.rsb_asio_config.out_latency_val,
                );
                get_dsp_rsb_bus_a_out().sync_config(active_sr, unified_smp, rep_in_smp, rep_out_smp);
                get_dsp_rsb_bus_b_in().set_adaptive_flags_with_mult(true, true, 1.05);
                get_dsp_rsb_bus_b_in().set_adaptive_mode(3);
                get_dsp_rsb_bus_b_in().set_user_extra_buffer_ms(0.0);
                get_dsp_rsb_bus_b_in().sync_config(
                    active_sr,
                    unified_smp,
                    1,
                    std_200ms_smp as u32,
                );
                if self.name == "Line A" {
                    let in_cush = (unified_smp as usize * 2).clamp(480, 768);
                    get_dsp_rsb_bus_a_out().reset_reader_with_cushion(in_cush);
                } else {
                    get_dsp_rsb_bus_b_in().reset_reader_with_cushion(std_200ms_smp);
                }
                self.status_message = if self.name == "Line A" {
                    format!(
                        "🎛️ RSB ASIO [WASAPI In A -> VST A -> RSB ASIO ({}smp) @ {}Hz]",
                        unified_smp, active_sr
                    )
                } else {
                    format!(
                        "🎛️ RSB ASIO [RSB ASIO -> VST B -> WASAPI Out B ({}smp) @ {}Hz]",
                        unified_smp, active_sr
                    )
                };
                self.engine = ActiveStreamEngine::AsioExperimental { _pair: pair };
                return;
            }
            Err(e) => {
                log_warn(format!(
                    "[{}] RSB ASIO WasapiFlexAsioPair gagal ({}), mencoba fallback cpal...",
                    self.name, e
                ));
            }
        }

        // Fallback cpal untuk RSB ASIO
        if self.name == "Line A" {
            let in_name = self.input_device_name.clone().unwrap_or_default();
            if let Some((_, in_dev)) = host
                .input_devices()
                .ok()
                .and_then(|it| it.enumerate().find(|(_, d)| d.name().ok().as_deref() == Some(in_name.as_str())))
            {
                let in_def = in_dev.default_input_config().ok();
                let def_ch = in_def.as_ref().map(|c| c.channels().clamp(1, 16)).unwrap_or(2);
                let def_sr = in_def.as_ref().map(|c| c.sample_rate().0).unwrap_or(target_sr);
                if let Ok((ins, active_sr, act_ch)) =
                    self.try_open_shared_input_only(&in_dev, target_sr, def_sr, def_ch)
                {
                    if ins.play().is_ok() {
                        self.in_channels = act_ch;
                        self.shared.active_sample_rate.store(active_sr, Ordering::Relaxed);
                        self.shared.actual_in_frames.store(unified_smp, Ordering::Relaxed);
                        self.shared.probe_in.set_sample_rate(active_sr, 1.8);
                        self.shared.probe_out.set_sample_rate(active_sr, 1.8);
                        self.sync_overlay_rates(active_sr);
                        self.status_message = format!(
                            "🎛️ RSB ASIO [WASAPI In A ({}ch) -> VST A -> ASIO Out A @ {}Hz]",
                            act_ch, active_sr
                        );
                        self.engine = ActiveStreamEngine::WasapiShared {
                            _in_stream: Some(ins),
                            _out_stream: None,
                        };
                        return;
                    }
                }
            }
            self.status_message = "⚠️ RSB ASIO Input Error".to_string();
        } else {
            let out_name = self.output_device_name.clone().unwrap_or_default();
            if let Some((_, out_dev)) = host
                .output_devices()
                .ok()
                .and_then(|it| it.enumerate().find(|(_, d)| d.name().ok().as_deref() == Some(out_name.as_str())))
            {
                let out_def = out_dev.default_output_config().ok();
                let def_ch = out_def.as_ref().map(|c| c.channels().clamp(1, 16)).unwrap_or(2);
                let def_sr = out_def.as_ref().map(|c| c.sample_rate().0).unwrap_or(target_sr);
                if let Ok((outs, active_sr, act_ch)) =
                    self.try_open_shared_output_only(&out_dev, target_sr, def_sr, def_ch)
                {
                    if outs.play().is_ok() {
                        self.out_channels = act_ch;
                        self.shared.active_sample_rate.store(active_sr, Ordering::Relaxed);
                        self.shared.actual_in_frames.store(unified_smp, Ordering::Relaxed);
                        self.shared.probe_in.set_sample_rate(active_sr, 1.8);
                        self.shared.probe_out.set_sample_rate(active_sr, 1.8);
                        self.sync_overlay_rates(active_sr);
                        self.status_message = format!(
                            "🎛️ RSB ASIO [Loopback B -> VST B -> WASAPI Out B ({}ch) @ {}Hz]",
                            act_ch, active_sr
                        );
                        self.engine = ActiveStreamEngine::WasapiShared {
                            _in_stream: None,
                            _out_stream: Some(outs),
                        };
                        return;
                    }
                }
            }
            self.status_message = "⚠️ RSB ASIO Output Error".to_string();
        }
    }

    fn start_wasapi_backend(&mut self, try_exclusive: bool) {
        let (in_name, out_name) = match (&self.input_device_name, &self.output_device_name) {
            (Some(i), Some(o)) if !i.is_empty() && !o.is_empty() => (i.clone(), o.clone()),
            _ => {
                self.status_message = "⚪ Inactive - select Input & Output".to_string();
                return;
            }
        };

        let host = cpal::default_host();
        let in_dev_opt = host
            .input_devices()
            .ok()
            .and_then(|it| it.enumerate().find(|(_, d)| d.name().ok().as_deref() == Some(in_name.as_str())));
        let out_dev_opt = host
            .output_devices()
            .ok()
            .and_then(|it| it.enumerate().find(|(_, d)| d.name().ok().as_deref() == Some(out_name.as_str())));

        let ((in_idx, in_dev), (out_idx, out_dev)) = match (in_dev_opt, out_dev_opt) {
            (Some(i), Some(o)) => (i, o),
            _ => {
                self.status_message = "⚠️ Error: WASAPI device not found".to_string();
                return;
            }
        };

        // 1. Jika user memilih WASAPI Exclusive, coba jalankan lewat COM IAudioClient Exclusive terlebih dahulu
        if try_exclusive {
            {
                let fifo = self.shared.fifo.lock();
                fifo.configure(self.buffer_size as usize, Some(self.sample_rate as f64));
                fifo.reset();
            }
            let shared_in = Arc::clone(&self.shared);
            let shared_out = Arc::clone(&self.shared);
            let mut scratch_in = AudioScratchBuffers::with_capacity(self.buffer_size as usize * 2);
            let mut scratch_out = AudioScratchBuffers::with_capacity(self.buffer_size as usize * 2);

            let excl_res = WasapiExclusivePair::try_start(
                in_idx,
                out_idx,
                self.sample_rate,
                self.buffer_size,
                move |data, ch, frames| {
                    let t0 = Instant::now();
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        process_input_block(&shared_in, &mut scratch_in, data, ch, frames);
                    }));
                    shared_in.update_dsp_load(t0.elapsed().as_secs_f64(), frames);
                },
                move |data, ch, frames| {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        render_output_block(&shared_out, &mut scratch_out, data, ch, frames);
                    }));
                },
            );

            match excl_res {
                Ok(pair) => {
                    let active_sr = pair.active_sample_rate;
                    self.shared.active_sample_rate.store(active_sr, Ordering::Relaxed);
                    self.shared.probe_in.set_sample_rate(active_sr, 1.8);
                    self.shared.probe_out.set_sample_rate(active_sr, 1.8);
                    self.sync_overlay_rates(active_sr);
                    if active_sr != self.sample_rate {
                        let fifo = self.shared.fifo.lock();
                        fifo.configure(self.buffer_size as usize, Some(active_sr as f64));
                    }
                    self.engine = ActiveStreamEngine::WasapiExclusive { _pair: pair };
                    self.status_message = format!(
                        "⚡ WASAPI Exclusive Active (2 In -> 2 Out @ {}Hz)",
                        active_sr
                    );
                    return;
                }
                Err(excl_err) => {
                    log_warn(format!(
                        "[{}] WASAPI Exclusive ditolak perangkat ({}). Otomatis beralih ke WASAPI Shared Low-Latency...",
                        self.name, excl_err
                    ));
                }
            }
        }

        // 2. ULTRA-OPTIMIZED WASAPI SHARED (Event-Driven Direct COM `WasapiFlexAsioPair` 10ms):
        //    Memangkas ~45-55% latensi WASAPI Shared standar dibanding polling `cpal` sekaligus
        //    menjaga kompatibilitas penuh multi-aplikasi (OBS / Discord / Browser) via AutoConvert PCM!
        if !try_exclusive {
            let unified_smp = self.buffer_size.clamp(32, 2048);
            {
                let fifo = self.shared.fifo.lock();
                fifo.configure_flex_asio(
                    unified_smp as usize,
                    0,
                    0,
                    Some(self.sample_rate as f64),
                );
                fifo.reset();
            }
            let fast_in_opts = FlexAsioEndpointOptions {
                set_latency_ms: Some(0.0),
                wasapi_set: true,
                wasapi_exclusive: false,
                wasapi_autoconvert: true,
                channels: 0,
            };
            let fast_out_opts = FlexAsioEndpointOptions {
                set_latency_ms: Some(0.0),
                wasapi_set: true,
                wasapi_exclusive: false,
                wasapi_autoconvert: true,
                channels: 0,
            };
            let shared_in = Arc::clone(&self.shared);
            let shared_out = Arc::clone(&self.shared);
            let mut scratch_in = AudioScratchBuffers::with_capacity((unified_smp as usize).max(1024) * 4);
            let mut scratch_out = AudioScratchBuffers::with_capacity((unified_smp as usize).max(1024) * 4);

            if let Ok(pair) = WasapiFlexAsioPair::try_start_with_options(
                Some(in_idx),
                Some(out_idx),
                self.sample_rate,
                unified_smp,
                fast_in_opts,
                fast_out_opts,
                move |data, ch, frames| {
                    let t0 = Instant::now();
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        process_input_block(&shared_in, &mut scratch_in, data, ch, frames);
                    }));
                    shared_in.update_dsp_load(t0.elapsed().as_secs_f64(), frames);
                },
                move |data, ch, frames| {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        render_output_block(&shared_out, &mut scratch_out, data, ch, frames);
                    }));
                },
            ) {
                let active_sr = pair.active_sample_rate;
                let act_in = pair.actual_in_buf;
                self.shared.active_sample_rate.store(active_sr, Ordering::Relaxed);
                self.shared.actual_in_frames.store(act_in, Ordering::Relaxed);
                self.shared.probe_in.set_sample_rate(active_sr, 1.8);
                self.shared.probe_out.set_sample_rate(active_sr, 1.8);
                self.sync_overlay_rates(active_sr);
                if active_sr != self.sample_rate {
                    let fifo = self.shared.fifo.lock();
                    fifo.configure_flex_asio(unified_smp as usize, 0, 0, Some(active_sr as f64));
                }
                self.engine = ActiveStreamEngine::AsioExperimental { _pair: pair };
                self.status_message = format!(
                    "🟢 WASAPI Shared Fast-Event ({}smp @ {}Hz)",
                    unified_smp, active_sr
                );
                return;
            }
        }

        // 3. Fallback WASAPI Shared cpal (jika perangkat virtual menolak direct event-driven)
        let in_def = in_dev.default_input_config().ok();
        let out_def = out_dev.default_output_config().ok();

        self.in_channels = in_def.as_ref().map(|c| c.channels().clamp(1, 2)).unwrap_or(2);
        self.out_channels = out_def.as_ref().map(|c| c.channels().clamp(1, 2)).unwrap_or(2);

        let requested_sr = self.sample_rate;
        let fallback_sr = out_def
            .as_ref()
            .map(|c| c.sample_rate().0)
            .or_else(|| in_def.as_ref().map(|c| c.sample_rate().0))
            .unwrap_or(48000);

        match self.try_open_shared_pair(&in_dev, &out_dev, requested_sr) {
            Ok((ins, outs)) => {
                self.finalize_shared_start(ins, outs, requested_sr, try_exclusive);
            }
            Err(err1) => {
                if fallback_sr != requested_sr {
                    log_warn(format!(
                        "[{}] Sample rate {}Hz ditolak ({}). Fallback ke {}Hz...",
                        self.name, requested_sr, err1, fallback_sr
                    ));
                    match self.try_open_shared_pair(&in_dev, &out_dev, fallback_sr) {
                        Ok((ins, outs)) => {
                            self.finalize_shared_start(ins, outs, fallback_sr, try_exclusive);
                        }
                        Err(err2) => {
                            self.status_message = format!("⚠️ Error: {}", &err2.chars().take(38).collect::<String>());
                            log_error(format!("[{}] WASAPI Pipeline Error: {}", self.name, err2));
                        }
                    }
                } else {
                    self.status_message = format!("⚠️ Error: {}", &err1.chars().take(38).collect::<String>());
                    log_error(format!("[{}] WASAPI Pipeline Error: {}", self.name, err1));
                }
            }
        }
    }

    fn finalize_shared_start(
        &mut self,
        ins: cpal::Stream,
        outs: cpal::Stream,
        active_sr: u32,
        was_excl_fallback: bool,
    ) {
        self.shared.active_sample_rate.store(active_sr, Ordering::Relaxed);
        self.shared.probe_in.set_sample_rate(active_sr, 1.8);
        self.shared.probe_out.set_sample_rate(active_sr, 1.8);
        self.sync_overlay_rates(active_sr);
        {
            let fifo = self.shared.fifo.lock();
            if self.driver_backend == DriverBackend::AsioExperimental {
                let extra_in = if self.asio_set_in_enabled {
                    ((self.asio_set_in_latency.max(0.0) as f64 / 1000.0) * active_sr as f64) as usize
                } else {
                    (self.buffer_size as usize) / 2
                };
                let extra_out = if self.asio_set_out_enabled {
                    ((self.asio_set_out_latency.max(0.0) as f64 / 1000.0) * active_sr as f64) as usize
                } else {
                    (self.buffer_size as usize) / 2
                };
                fifo.configure_flex_asio(
                    self.buffer_size as usize,
                    extra_in,
                    extra_out,
                    Some(active_sr as f64),
                );
            } else {
                fifo.configure(self.buffer_size as usize, Some(active_sr as f64));
            }
            fifo.reset();
        }
        if let Err(e) = ins.play().and_then(|_| outs.play()) {
            self.status_message = format!("⚠️ Failed to start stream: {}", e);
            log_error(format!("[{}] Gagal play stream: {}", self.name, e));
            return;
        }
        self.engine = ActiveStreamEngine::WasapiShared {
            _in_stream: Some(ins),
            _out_stream: Some(outs),
        };
        if self.driver_backend == DriverBackend::AsioExperimental {
            self.status_message = format!(
                "🚀 ASIO Exp [In {}smp -> Out {}smp @ {}Hz]",
                self.asio_exp_in_samples, self.asio_exp_out_samples, active_sr
            );
            return;
        }
        let mut notes = Vec::new();
        if active_sr != self.sample_rate {
            notes.push("fallback SR");
        }
        if was_excl_fallback {
            notes.push("Shared fallback");
        }
        let suffix = if notes.is_empty() {
            String::new()
        } else {
            format!(" ({})", notes.join(", "))
        };
        self.status_message = format!(
            "🟢 WASAPI Active ({} In -> {} Out @ {}Hz{})",
            self.in_channels, self.out_channels, active_sr, suffix
        );
    }

    fn try_open_shared_input_only(
        &self,
        in_dev: &cpal::Device,
        target_sr: u32,
        fallback_sr: u32,
        native_ch: u16,
    ) -> Result<(cpal::Stream, u32, u16), String> {
        let mut candidates = Vec::new();
        for &sr in &[target_sr, fallback_sr] {
            for &ch in &[2u16, 1u16, native_ch] {
                let pair = (sr, ch.max(1));
                if !candidates.contains(&pair) {
                    candidates.push(pair);
                }
            }
        }
        let mut last_err = String::from("Unable to open WASAPI input stream");
        for (sr, ch) in candidates {
            let cfg_def = cpal::StreamConfig {
                channels: ch,
                sample_rate: cpal::SampleRate(sr),
                buffer_size: cpal::BufferSize::Default,
            };
            match self.build_input_stream(in_dev, &cfg_def) {
                Ok(s) => return Ok((s, sr, ch)),
                Err(e) => last_err = e,
            }
            let cfg_fix = cpal::StreamConfig {
                channels: ch,
                sample_rate: cpal::SampleRate(sr),
                buffer_size: cpal::BufferSize::Fixed(self.buffer_size),
            };
            if let Ok(s) = self.build_input_stream(in_dev, &cfg_fix) {
                return Ok((s, sr, ch));
            }
        }
        Err(last_err)
    }

    fn try_open_shared_output_only(
        &self,
        out_dev: &cpal::Device,
        target_sr: u32,
        fallback_sr: u32,
        native_ch: u16,
    ) -> Result<(cpal::Stream, u32, u16), String> {
        let mut candidates = Vec::new();
        for &sr in &[target_sr, fallback_sr] {
            for &ch in &[2u16, native_ch, 1u16] {
                let pair = (sr, ch.max(1));
                if !candidates.contains(&pair) {
                    candidates.push(pair);
                }
            }
        }
        let mut last_err = String::from("Unable to open WASAPI output stream");
        for (sr, ch) in candidates {
            let cfg_def = cpal::StreamConfig {
                channels: ch,
                sample_rate: cpal::SampleRate(sr),
                buffer_size: cpal::BufferSize::Default,
            };
            match self.build_output_stream(out_dev, &cfg_def) {
                Ok(s) => return Ok((s, sr, ch)),
                Err(e) => last_err = e,
            }
            let cfg_fix = cpal::StreamConfig {
                channels: ch,
                sample_rate: cpal::SampleRate(sr),
                buffer_size: cpal::BufferSize::Fixed(self.buffer_size),
            };
            if let Ok(s) = self.build_output_stream(out_dev, &cfg_fix) {
                return Ok((s, sr, ch));
            }
        }
        Err(last_err)
    }

    fn try_open_shared_pair(
        &self,
        in_dev: &cpal::Device,
        out_dev: &cpal::Device,
        sr: u32,
    ) -> Result<(cpal::Stream, cpal::Stream), String> {
        let (req_in_buf, req_out_buf) = if self.driver_backend == DriverBackend::AsioExperimental {
            (
                self.asio_exp_in_samples.clamp(32, 1024),
                self.asio_exp_out_samples.clamp(32, 1024),
            )
        } else {
            (self.buffer_size, self.buffer_size)
        };
        let in_cfg = cpal::StreamConfig {
            channels: self.in_channels,
            sample_rate: cpal::SampleRate(sr),
            buffer_size: cpal::BufferSize::Fixed(req_in_buf),
        };
        let out_cfg = cpal::StreamConfig {
            channels: self.out_channels,
            sample_rate: cpal::SampleRate(sr),
            buffer_size: cpal::BufferSize::Fixed(req_out_buf),
        };

        let in_cfg_fallback = cpal::StreamConfig {
            channels: self.in_channels,
            sample_rate: cpal::SampleRate(sr),
            buffer_size: cpal::BufferSize::Default,
        };
        let out_cfg_fallback = cpal::StreamConfig {
            channels: self.out_channels,
            sample_rate: cpal::SampleRate(sr),
            buffer_size: cpal::BufferSize::Default,
        };

        let ins = self
            .build_input_stream(in_dev, &in_cfg)
            .or_else(|_| self.build_input_stream(in_dev, &in_cfg_fallback))?;
        let outs = self
            .build_output_stream(out_dev, &out_cfg)
            .or_else(|_| self.build_output_stream(out_dev, &out_cfg_fallback))?;

        Ok((ins, outs))
    }

    fn build_input_stream(
        &self,
        dev: &cpal::Device,
        cfg: &cpal::StreamConfig,
    ) -> Result<cpal::Stream, String> {
        let shared = Arc::clone(&self.shared);
        let shared_err = Arc::clone(&self.shared);
        let in_ch = cfg.channels as usize;
        let mut boosted = false;
        let mut scratch = AudioScratchBuffers::with_capacity(self.buffer_size as usize * 2);

        dev.build_input_stream(
            cfg,
            move |data: &[f32], _info: &cpal::InputCallbackInfo| {
                if !boosted {
                    boosted = true;
                    if let Some(how) = boost_audio_thread() {
                        log_info(format!("[{}] Thread callback input dioptimalkan ({})", shared.name, how));
                    }
                }

                let t0 = Instant::now();
                let frames = data.len() / in_ch.max(1);
                let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    process_input_block(&shared, &mut scratch, data, in_ch, frames);
                }));
                if res.is_err() {
                    shared.in_xruns.fetch_add(1, Ordering::Relaxed);
                }
                shared.update_dsp_load(t0.elapsed().as_secs_f64(), frames);
            },
            move |err| {
                shared_err.in_xruns.fetch_add(1, Ordering::Relaxed);
                shared_err.stream_faulted.store(true, Ordering::Relaxed);
                log_error(format!("[{}] Stream input error: {}", shared_err.name, err));
            },
            Some(Duration::from_secs(2)),
        )
        .map_err(|e| e.to_string())
    }

    fn build_output_stream(
        &self,
        dev: &cpal::Device,
        cfg: &cpal::StreamConfig,
    ) -> Result<cpal::Stream, String> {
        let shared = Arc::clone(&self.shared);
        let shared_err = Arc::clone(&self.shared);
        let out_ch = cfg.channels as usize;
        let mut boosted = false;
        let mut scratch_direct = AudioScratchBuffers::with_capacity((self.buffer_size as usize).max(1024) * 4);

        dev.build_output_stream(
            cfg,
            move |data: &mut [f32], _info: &cpal::OutputCallbackInfo| {
                if !boosted {
                    boosted = true;
                    if let Some(how) = boost_audio_thread() {
                        log_info(format!("[{}] Thread callback output dioptimalkan ({})", shared.name, how));
                    }
                }
                let t0 = Instant::now();
                let frames = data.len() / out_ch.max(1);
                let is_rsb_asio_b = shared.is_rsb_asio.load(Ordering::Relaxed)
                    && shared.name == "Line B";
                let is_dsp_rsb_b = shared.is_asio_dsp_rsb.load(Ordering::Relaxed)
                    && shared.name == "Line B";
                let use_vc_bus = shared.is_asio_experimental.load(Ordering::Relaxed)
                    && shared.name == "Line B"
                    && shared.is_direct_bridge_in.load(Ordering::Relaxed);
                let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    if is_rsb_asio_b {
                        scratch_direct.ensure_frames(frames);
                        let steady_cush = shared
                            .rsb_asio_out_cush_smp
                            .load(Ordering::Relaxed)
                            .clamp(480, 24000) as usize;
                        get_dsp_rsb_bus_b_in().read_stereo_rsb_flex_b_into(
                            &mut scratch_direct.bus_l[..frames],
                            &mut scratch_direct.bus_r[..frames],
                            steady_cush,
                        );
                        process_and_render_direct_stereo_from_scratch(
                            &shared,
                            &mut scratch_direct,
                            data,
                            out_ch,
                            frames,
                        );
                    } else if is_dsp_rsb_b {
                        scratch_direct.ensure_frames(frames);
                        let (cush_target, stretch_en) = get_dsp_rsb_bus_b_in().get_dsp_b_params();
                        get_dsp_rsb_bus_b_in().read_stereo_dsp_b_into(
                            &mut scratch_direct.bus_l[..frames],
                            &mut scratch_direct.bus_r[..frames],
                            cush_target,
                            stretch_en,
                        );
                        process_and_render_direct_stereo_from_scratch(
                            &shared,
                            &mut scratch_direct,
                            data,
                            out_ch,
                            frames,
                        );
                    } else if use_vc_bus {
                        scratch_direct.ensure_frames(frames);
                        get_asio_bus_line_b_in().read_stereo_into(
                            &mut scratch_direct.bus_l[..frames],
                            &mut scratch_direct.bus_r[..frames],
                        );
                        process_and_render_direct_stereo_from_scratch(
                            &shared,
                            &mut scratch_direct,
                            data,
                            out_ch,
                            frames,
                        );
                    } else {
                        render_output_block(&shared, &mut scratch_direct, data, out_ch, frames);
                    }
                }));
                if res.is_err() {
                    data.fill(0.0);
                    shared.peak_level_bits.store(0.0f32.to_bits(), Ordering::Relaxed);
                    shared.out_xruns.fetch_add(1, Ordering::Relaxed);
                } else if is_rsb_asio_b || is_dsp_rsb_b || use_vc_bus {
                    shared.update_dsp_load(t0.elapsed().as_secs_f64(), frames);
                }
            },
            move |err| {
                shared_err.out_xruns.fetch_add(1, Ordering::Relaxed);
                shared_err.stream_faulted.store(true, Ordering::Relaxed);
                log_error(format!("[{}] Stream output error: {}", shared_err.name, err));
            },
            Some(Duration::from_secs(2)),
        )
        .map_err(|e| e.to_string())
    }

    pub fn check_stream_health(&mut self) -> bool {
        self.purge_graveyard(1.5);
        if !self.is_enabled() || !self.streams_running() {
            return false;
        }

        let faulted = self.shared.stream_faulted.load(Ordering::Relaxed);
        if !faulted {
            self.healthy_ticks += 1;
            if self.healthy_ticks >= 5 {
                self.restart_attempts = 0;
                self.gave_up = false;
            }
            return false;
        }

        self.healthy_ticks = 0;
        if self.restart_attempts >= Self::MAX_AUTO_RESTART {
            if !self.gave_up {
                self.gave_up = true;
                self.status_message =
                    "⚠️ Stream stopped - toggle Line ON/OFF or Refresh Devices".to_string();
                log_error(format!(
                    "[{}] Stream berhenti & auto-restart gagal {}x",
                    self.name, self.restart_attempts
                ));
                return true;
            }
            return false;
        }

        let now = Instant::now();
        if let Some(last) = self.last_restart_try {
            if now.saturating_duration_since(last).as_secs_f64() < 3.0 {
                return false;
            }
        }
        self.last_restart_try = Some(now);
        self.restart_attempts += 1;
        log_warn(format!(
            "[{}] Stream terdeteksi berhenti, auto-restart ({}/{})",
            self.name,
            self.restart_attempts,
            Self::MAX_AUTO_RESTART
        ));
        self.restart_stream();
        true
    }

    pub fn add_vst(&mut self, file_path: &str) -> Result<Arc<VSTSlot>, String> {
        let sr = self.effective_sample_rate().max(44100) as f64;
        // Gunakan ukuran blok maksimum 2048 agar VST3 dapat memproses blok 32..1024 sampel
        // secara utuh dalam sekali jalan tanpa pemotongan sub-blok ganjil!
        let max_blk = (self.buffer_size as usize).max(2048);
        let slot = Arc::new(VSTSlot::load(file_path.to_string(), sr, max_blk)?);
        slot.flush_pending.store(true, Ordering::Relaxed);
        record_breadcrumb(format!("[{}] Menambahkan VST '{}'", self.name, slot.name));
        self.vst_slots.push(Arc::clone(&slot));
        self.publish_chain();
        Ok(slot)
    }

    pub fn remove_vst(&mut self, index: usize) {
        if index < self.vst_slots.len() {
            let removed = self.vst_slots.remove(index);
            record_breadcrumb(format!("[{}] Menghapus VST '{}'", self.name, removed.name));
            removed.bypassed.store(true, Ordering::SeqCst);
            removed.hide_editor_window();
            if index < self.vst_slots.len() {
                self.vst_slots[index].flush_pending.store(true, Ordering::Relaxed);
            }
            self.publish_chain();
            self.graveyard.push((Instant::now(), removed));
        }
    }

    pub fn move_vst(&mut self, index: usize, direction: isize) {
        let new_idx = index as isize + direction;
        if index < self.vst_slots.len() && new_idx >= 0 && (new_idx as usize) < self.vst_slots.len() {
            let dst = new_idx as usize;
            record_breadcrumb(format!(
                "[{}] Memindahkan urutan VST '{}' ({} -> {})",
                self.name,
                self.vst_slots[index].name,
                index + 1,
                dst + 1
            ));
            self.vst_slots.swap(index, dst);
            self.vst_slots[index].flush_pending.store(true, Ordering::Relaxed);
            self.vst_slots[dst].flush_pending.store(true, Ordering::Relaxed);
            self.publish_chain();
        }
    }
}

/// Menjalankan rantai DSP Input (DC Blocker + Overlay Input + VST3 Chain + Mode Aman + Soft Limiter)
/// secara in-place pada slice `left` dan `right`.
fn run_dsp_vst_chain_stereo(
    shared: &SharedAudioState,
    left: &mut [f32],
    right: &mut [f32],
    prev_l: &mut [f32],
    prev_r: &mut [f32],
    frames: usize,
) {
    let sr = shared.active_sample_rate.load(Ordering::Relaxed).max(44100);

    // Filter DC Offset & Sub-Sonic Rumble 20 Hz sebelum masuk ke rantai VST3 (ONNX / Pitch Shifter)
    if shared.dc_blocker_enabled.load(Ordering::Relaxed) {
        if let Some(mut dcb) = shared.dc_blocker.try_lock() {
            dcb.process(left, right, sr);
        }
    }

    shared.overlay_input_bank.mix_into(left, right);
    // Tahap 1: Pelindung Pre-VST Ultra-Transparan (Headroom Tinggi 0.98) agar dinamika alami vokal
    // tidak mengalami kompresi ganda sebelum masuk ke Noise Gate / Compressor / EQ VST3!
    soft_limit_pre_vst_stereo(left, right);

    let mut chain_failed = false;
    let chain_snapshot = shared.chain.load();

    for slot in chain_snapshot.iter() {
        if slot.is_bypassed() {
            continue;
        }
        let Some(mut plugin_guard) = slot.audio_proc.try_lock() else {
            continue;
        };
        let plugin = match plugin_guard.as_mut() {
            Some(p) => p,
            None => continue,
        };

        if slot.reset_pending.swap(false, Ordering::Relaxed) {
            plugin.reset();
        }
        let need_crossfade = slot.flush_pending.swap(false, Ordering::Relaxed);
        if need_crossfade {
            prev_l.copy_from_slice(left);
            prev_r.copy_from_slice(right);
        }

        let t_vst = Instant::now();
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            plugin.process_stereo(left, right, sr)
        }));
        let ns = t_vst.elapsed().as_nanos() as u64;
        let prev_ns = slot.last_process_ns.load(Ordering::Relaxed);
        let smooth_ns = if prev_ns == 0 { ns } else { (prev_ns * 7 + ns * 3) / 10 };
        slot.last_process_ns.store(smooth_ns, Ordering::Relaxed);
        slot.latency_samples.store(plugin.latency_samples, Ordering::Relaxed);

        match res {
            Ok(Ok(())) => {
                if need_crossfade {
                    let cf_len = frames.min(128);
                    for i in 0..cf_len {
                        let t = (i as f32) / (cf_len.max(1) as f32);
                        // Kurva S-Curve Raised-Cosine bebas klik
                        let w = 0.5 * (1.0 - (std::f32::consts::PI * t).cos());
                        left[i] = prev_l[i] * (1.0 - w) + left[i] * w;
                        right[i] = prev_r[i] * (1.0 - w) + right[i] * w;
                    }
                }
            }
            Ok(Err(err_msg)) => {
                chain_failed = true;
                plugin.reset();
                slot.record_error(err_msg.clone());
                log_slot_error(&shared.name, slot, &err_msg);
            }
            Err(_) => {
                chain_failed = true;
                plugin.reset();
                let msg = "VST plugin encountered an internal panic".to_string();
                slot.record_error(msg.clone());
                log_slot_error(&shared.name, slot, &msg);
            }
        }
    }

    if chain_failed && shared.fail_closed.load(Ordering::Relaxed) {
        left.fill(0.0);
        right.fill(0.0);
        shared
            .failsafe_until_ms
            .store(shared.elapsed_ms() + 1000, Ordering::Relaxed);
    }

    // Jaga sanitasi NaN/Inf dan puncak di atas 0.98 seusai rantai VST3 (Mastering Limiter akhir
    // dijalankan satu kali setelah Master Volume & Overlay Output agar suara vokal tetap jernih!)
    soft_limit_pre_vst_stereo(left, right);
}

fn process_input_block(
    shared: &SharedAudioState,
    scratch: &mut AudioScratchBuffers,
    indata: &[f32],
    in_ch: usize,
    frames: usize,
) {
    enable_cpu_denormal_flush();
    if !shared.is_enabled.load(Ordering::Relaxed) || frames == 0 {
        return;
    }
    shared.actual_in_frames.store(frames as u32, Ordering::Relaxed);

    scratch.ensure_frames(frames);
    let left = &mut scratch.left[..frames];
    let right = &mut scratch.right[..frames];
    let prev_l = &mut scratch.prev_l[..frames];
    let prev_r = &mut scratch.prev_r[..frames];
    let mono_probe = &mut scratch.mono_probe[..frames];

    if in_ch == 1 {
        for (i, &s) in indata.iter().take(frames).enumerate() {
            left[i] = s;
            right[i] = s;
            mono_probe[i] = s;
        }
    } else {
        for (i, chunk) in indata.chunks(in_ch).take(frames).enumerate() {
            let l = chunk[0];
            let r = chunk[1];
            left[i] = l;
            right[i] = r;
            mono_probe[i] = 0.5 * (l + r);
        }
    }

    shared.probe_in.write(mono_probe);

    run_dsp_vst_chain_stereo(shared, left, right, prev_l, prev_r, frames);

    // Jalur Langsung Bebas-FIFO untuk `BUS_LINE_A_OUT` (menuju Input Vonovox):
    // Menulis sampel mikrofon yang sudah melewati DC Blocker + VST3 Chain + Master Gain Line A
    // LANGSUNG dari callback capture ke `BUS_LINE_A_OUT` tanpa melewati `AudioFIFO` Line A!
    let is_rsb_asio = shared.is_rsb_asio.load(Ordering::Relaxed);
    let is_dsp_rsb = shared.is_asio_dsp_rsb.load(Ordering::Relaxed) || is_rsb_asio;
    let is_exp = shared.is_asio_experimental.load(Ordering::Relaxed);
    let need_dsp_bus = is_dsp_rsb;
    let need_asio_bus = !is_dsp_rsb && is_exp;
    if shared.name == "Line A" && (need_dsp_bus || need_asio_bus) {
        if shared.is_muted.load(Ordering::Relaxed) || shared.is_in_failsafe() {
            prev_l.fill(0.0);
            prev_r.fill(0.0);
        } else {
            let gain = f32::from_bits(shared.master_gain_bits.load(Ordering::Relaxed));
            for i in 0..frames {
                prev_l[i] = left[i] * gain;
                prev_r[i] = right[i] * gain;
            }
            shared.overlay_output_bank.mix_into(prev_l, prev_r);
            // Gunakan sanitasi Float32 100% Bit-Exact tanpa kompresi ganda sebelum masuk ke Vonovox!
            soft_limit_pre_vst_stereo(prev_l, prev_r);
        }
        if need_dsp_bus || need_asio_bus {
            get_dsp_rsb_bus_a_out().write_stereo(prev_l, prev_r, 48000);
        }
        if need_asio_bus {
            get_asio_bus_line_a_out().write_stereo(prev_l, prev_r, 48000);
        }

        if is_dsp_rsb {
            let mut peak = 0.0f32;
            for i in 0..frames {
                let m = prev_l[i].abs().max(prev_r[i].abs());
                if m > peak {
                    peak = m;
                }
            }
            shared
                .peak_level_bits
                .store(peak.min(1.5).to_bits(), Ordering::Relaxed);
            return;
        }
    }

    let eff_blk = frames.max(shared.buffer_size.load(Ordering::Relaxed) as usize).max(480);
    let max_lead = (eff_blk * 6 + 1024).clamp(2400, 8192);
    shared.fifo.lock().write(left, right, max_lead);
}

/// Versi Zero-Allocation dari `process_and_render_direct_stereo_block` yang membaca langsung
/// dari `scratch.bus_l` dan `scratch.bus_r` tanpa alokasi Vec di dalam callback real-time!
fn process_and_render_direct_stereo_from_scratch(
    shared: &SharedAudioState,
    scratch: &mut AudioScratchBuffers,
    outdata: &mut [f32],
    out_ch: usize,
    frames: usize,
) {
    enable_cpu_denormal_flush();
    if !shared.is_enabled.load(Ordering::Relaxed) || frames == 0 {
        outdata.fill(0.0);
        shared.peak_level_bits.store(0.0f32.to_bits(), Ordering::Relaxed);
        return;
    }
    shared.actual_in_frames.store(frames as u32, Ordering::Relaxed);

    scratch.ensure_frames(frames);
    for i in 0..frames {
        let l = scratch.bus_l[i];
        let r = scratch.bus_r[i];
        scratch.left[i] = l;
        scratch.right[i] = r;
        scratch.mono_probe[i] = 0.5 * (l + r);
    }
    shared.probe_in.write(&scratch.mono_probe[..frames]);

    run_dsp_vst_chain_stereo(
        shared,
        &mut scratch.left[..frames],
        &mut scratch.right[..frames],
        &mut scratch.prev_l[..frames],
        &mut scratch.prev_r[..frames],
        frames,
    );

    let left = &mut scratch.left[..frames];
    let right = &mut scratch.right[..frames];
    let mono_probe = &mut scratch.mono_probe[..frames];

    if shared.is_muted.load(Ordering::Relaxed) || shared.is_in_failsafe() {
        left.fill(0.0);
        right.fill(0.0);
    } else {
        let gain = f32::from_bits(shared.master_gain_bits.load(Ordering::Relaxed));
        for i in 0..frames {
            left[i] *= gain;
            right[i] *= gain;
        }
        shared.overlay_output_bank.mix_into(left, right);
        soft_limit_stereo(left, right);
    }

    let mut peak = 0.0f32;
    for i in 0..frames {
        let l = left[i];
        let r = right[i];
        mono_probe[i] = 0.5 * (l + r);
        let m = l.abs().max(r.abs());
        if m > peak {
            peak = m;
        }
    }
    shared.probe_out.write(mono_probe);
    shared
        .peak_level_bits
        .store(peak.min(1.5).to_bits(), Ordering::Relaxed);

    if out_ch == 1 {
        for i in 0..frames {
            outdata[i] = 0.5 * (left[i] + right[i]);
        }
    } else {
        for (i, chunk) in outdata.chunks_mut(out_ch).take(frames).enumerate() {
            chunk[0] = left[i];
            chunk[1] = right[i];
            for extra_ch in &mut chunk[2..] {
                *extra_ch = 0.0;
            }
        }
    }
}

/// Jalur langsung bebas-FIFO untuk Line B saat menerima hasil konversi dari `BUS_LINE_B_IN` (Vonovox).
#[allow(dead_code)]
fn process_and_render_direct_stereo_block(
    shared: &SharedAudioState,
    scratch: &mut AudioScratchBuffers,
    bus_l: &[f32],
    bus_r: &[f32],
    outdata: &mut [f32],
    out_ch: usize,
    frames: usize,
) {
    enable_cpu_denormal_flush();
    if !shared.is_enabled.load(Ordering::Relaxed) || frames == 0 {
        outdata.fill(0.0);
        shared.peak_level_bits.store(0.0f32.to_bits(), Ordering::Relaxed);
        return;
    }
    shared.actual_in_frames.store(frames as u32, Ordering::Relaxed);

    scratch.ensure_frames(frames);
    let left = &mut scratch.left[..frames];
    let right = &mut scratch.right[..frames];
    let prev_l = &mut scratch.prev_l[..frames];
    let prev_r = &mut scratch.prev_r[..frames];
    let mono_probe = &mut scratch.mono_probe[..frames];

    for i in 0..frames {
        let l = bus_l.get(i).copied().unwrap_or(0.0);
        let r = bus_r.get(i).copied().unwrap_or(0.0);
        left[i] = l;
        right[i] = r;
        mono_probe[i] = 0.5 * (l + r);
    }
    shared.probe_in.write(mono_probe);

    run_dsp_vst_chain_stereo(shared, left, right, prev_l, prev_r, frames);

    if shared.is_muted.load(Ordering::Relaxed) || shared.is_in_failsafe() {
        left.fill(0.0);
        right.fill(0.0);
    } else {
        let gain = f32::from_bits(shared.master_gain_bits.load(Ordering::Relaxed));
        for i in 0..frames {
            left[i] *= gain;
            right[i] *= gain;
        }
        shared.overlay_output_bank.mix_into(left, right);
        soft_limit_stereo(left, right);
    }

    let mut peak = 0.0f32;
    for i in 0..frames {
        let l = left[i];
        let r = right[i];
        mono_probe[i] = 0.5 * (l + r);
        let m = l.abs().max(r.abs());
        if m > peak {
            peak = m;
        }
    }
    shared.probe_out.write(mono_probe);
    shared
        .peak_level_bits
        .store(peak.min(1.5).to_bits(), Ordering::Relaxed);

    if out_ch == 1 {
        for i in 0..frames {
            outdata[i] = 0.5 * (left[i] + right[i]);
        }
    } else {
        for (i, chunk) in outdata.chunks_mut(out_ch).take(frames).enumerate() {
            chunk[0] = left[i];
            chunk[1] = right[i];
            for extra_ch in &mut chunk[2..] {
                *extra_ch = 0.0;
            }
        }
    }
}

fn log_slot_error(line_name: &str, slot: &VSTSlot, err_msg: &str) {
    let now = Instant::now();
    let mut last_guard = slot.last_logged_time.lock();
    let should_log = match *last_guard {
        None => true,
        Some(prev) => now.saturating_duration_since(prev).as_secs_f64() >= 2.0,
    };
    if should_log {
        let suppressed = slot.suppressed_logs.swap(0, Ordering::Relaxed);
        *last_guard = Some(now);
        let extra = if suppressed > 0 {
            format!(" (+{} error disembunyikan)", suppressed)
        } else {
            String::new()
        };
        let count = slot.error_count.load(Ordering::Relaxed);
        log_error(format!(
            "[{}] VST '{}' gagal diproses (ke-{}x): {}{}",
            line_name, slot.name, count, err_msg, extra
        ));
    } else {
        slot.suppressed_logs.fetch_add(1, Ordering::Relaxed);
    }
}

fn render_output_block(
    shared: &SharedAudioState,
    scratch: &mut AudioScratchBuffers,
    outdata: &mut [f32],
    out_ch: usize,
    frames: usize,
) {
    enable_cpu_denormal_flush();
    if !shared.is_enabled.load(Ordering::Relaxed) || frames == 0 {
        outdata.fill(0.0);
        shared.peak_level_bits.store(0.0f32.to_bits(), Ordering::Relaxed);
        return;
    }

    scratch.ensure_frames(frames);
    let left = &mut scratch.left[..frames];
    let right = &mut scratch.right[..frames];
    let mono_probe = &mut scratch.mono_probe[..frames];

    shared.fifo.lock().read_into(left, right);

    if shared.is_muted.load(Ordering::Relaxed) || shared.is_in_failsafe() {
        left.fill(0.0);
        right.fill(0.0);
    } else {
        let gain = f32::from_bits(shared.master_gain_bits.load(Ordering::Relaxed));
        for i in 0..frames {
            left[i] *= gain;
            right[i] *= gain;
        }
        shared.overlay_output_bank.mix_into(left, right);
        soft_limit_stereo(left, right);
    }

    if out_ch == 1 {
        for i in 0..frames {
            outdata[i] = 0.5 * (left[i] + right[i]);
        }
    } else {
        for (i, chunk) in outdata.chunks_mut(out_ch).take(frames).enumerate() {
            chunk[0] = left[i];
            chunk[1] = right[i];
            for extra_ch in &mut chunk[2..] {
                *extra_ch = 0.0;
            }
        }
    }

    let mut peak = 0.0f32;
    for i in 0..frames {
        let l = left[i];
        let r = right[i];
        let m = l.abs().max(r.abs());
        if m > peak {
            peak = m;
        }
        mono_probe[i] = 0.5 * (l + r);
    }
    shared.probe_out.write(mono_probe);
    shared
        .peak_level_bits
        .store(peak.min(1.5).to_bits(), Ordering::Relaxed);
}
