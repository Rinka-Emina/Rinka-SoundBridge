//! Manajemen Konfigurasi v2.0 (`rinka_soundbridge_config.json`)
//! 100% kompatibel mundur dengan file konfigurasi v1.22 / v1.3,
//! ditambah konfigurasi **Triple ASIO (`asio_line_a`, `asio_line_b`, `asio_vc`)**
//! yang masing-masing memiliki pilihan Input, Output, Buffer `smp`, Latency, WASAPI Set/Exclusive/AutoConvert, & Channels.

use crate::logger::{get_base_dir, get_savedata_dir, log_error, log_warn};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

pub const DEFAULT_THEME_NAME: &str = "Windows 11 Dark";

pub fn get_config_path() -> PathBuf {
    get_savedata_dir().join("rinka_soundbridge_config.json")
}

fn default_buffer_size() -> u32 { 256 }
fn default_asio_exp_buf() -> u32 { 256 }
fn default_asio_lat_ms() -> f32 { 0.0 }
fn default_dsp_cushion_ms() -> f32 { 60.0 }
fn default_adaptive_extra_ms() -> f32 { 25.0 }
fn default_gpu_stall_mult() -> f32 { 1.15 }
fn default_reported_in_ms() -> f32 { 304.0 }
fn default_reported_out_ms() -> f32 { 16.0 }
fn default_sample_rate() -> u32 { 48000 }
fn default_win_w() -> Option<u32> { Some(1080) }
fn default_win_h() -> Option<u32> { Some(980) }
fn default_theme() -> String { DEFAULT_THEME_NAME.to_string() }
fn default_fifo_mode() -> String { "adaptive".to_string() }
fn default_driver_mode() -> String { "wasapi_shared".to_string() }
fn default_asio_cpu_cores() -> u16 { 2 }
fn default_true() -> bool { true }
fn default_vol() -> i32 { 100 }
fn default_gain() -> f32 { 0.5 }

/// Konfigurasi lengkap per-engine untuk **Triple ASIO** (identik dengan fitur FlexASIO GUI):
/// Digunakan oleh:
/// 1. `ASIO Line A` (Internal RSB Line A)
/// 2. `ASIO Line B` (Internal RSB Line B)
/// 3. `ASIO Khusus VC` (`"Rinka SoundBridge ASIO"` untuk Vonovox / Voice Changer)
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct FlexAsioProfileConfig {
    #[serde(default)]
    pub input_device: String,
    #[serde(default)]
    pub output_device: String,
    #[serde(default = "default_buffer_size")]
    pub buffer_size: u32,

    #[serde(default)]
    pub set_in_latency_enabled: bool,
    #[serde(default = "default_asio_lat_ms")]
    pub in_latency_val: f32,
    #[serde(default)]
    pub set_out_latency_enabled: bool,
    #[serde(default = "default_asio_lat_ms")]
    pub out_latency_val: f32,

    #[serde(default = "default_true")]
    pub wasapi_in_set: bool,
    #[serde(default)]
    pub wasapi_in_exclusive: bool,
    #[serde(default = "default_true")]
    pub wasapi_in_autoconvert: bool,

    #[serde(default = "default_true")]
    pub wasapi_out_set: bool,
    #[serde(default)]
    pub wasapi_out_exclusive: bool,
    #[serde(default = "default_true")]
    pub wasapi_out_autoconvert: bool,

    #[serde(default)]
    pub in_channels: u16,
    #[serde(default)]
    pub out_channels: u16,
}

impl Default for FlexAsioProfileConfig {
    fn default() -> Self {
        Self {
            input_device: String::new(),
            output_device: String::new(),
            buffer_size: 256,
            set_in_latency_enabled: false,
            in_latency_val: 0.0,
            set_out_latency_enabled: false,
            out_latency_val: 0.0,
            wasapi_in_set: true,
            wasapi_in_exclusive: false,
            wasapi_in_autoconvert: true,
            wasapi_out_set: true,
            wasapi_out_exclusive: false,
            wasapi_out_autoconvert: true,
            in_channels: 0,
            out_channels: 0,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct GlobalConfig {
    #[serde(default = "default_buffer_size")]
    pub buffer_size: u32,
    #[serde(default = "default_sample_rate")]
    pub sample_rate: u32,
    #[serde(default)]
    pub window_geometry: Option<String>,
    #[serde(default)]
    pub window_x: Option<i32>,
    #[serde(default)]
    pub window_y: Option<i32>,
    #[serde(default = "default_win_w")]
    pub window_w: Option<u32>,
    #[serde(default = "default_win_h")]
    pub window_h: Option<u32>,
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default = "default_fifo_mode")]
    pub fifo_mode: String,
    #[serde(default = "default_true")]
    pub audio_priority: bool,
    #[serde(default = "default_true")]
    pub show_delay_bar: bool,
    #[serde(default = "default_true")]
    pub sync_triple_asio: bool,
    #[serde(default = "default_true")]
    pub asio_set_in_enabled: bool,
    #[serde(default = "default_asio_lat_ms")]
    pub asio_set_in_latency: f32,
    #[serde(default = "default_true")]
    pub asio_set_out_enabled: bool,
    #[serde(default = "default_asio_lat_ms")]
    pub asio_set_out_latency: f32,
    #[serde(default)]
    pub gpu_boost_lock_enabled: bool,
    #[serde(default = "default_gpu_lock_mhz")]
    pub gpu_boost_lock_mhz: u32,
    #[serde(default = "default_true")]
    pub gpu_boost_lock_failsafe: bool,
}

fn default_gpu_lock_mhz() -> u32 {
    1980
}

impl Default for GlobalConfig {
    fn default() -> Self {
        Self {
            buffer_size: 256,
            sample_rate: 48000,
            window_geometry: None,
            window_x: None,
            window_y: None,
            window_w: Some(1020),
            window_h: Some(940),
            theme: DEFAULT_THEME_NAME.to_string(),
            fifo_mode: "adaptive".to_string(),
            audio_priority: true,
            show_delay_bar: true,
            sync_triple_asio: true,
            asio_set_in_enabled: true,
            asio_set_in_latency: 0.0,
            asio_set_out_enabled: true,
            asio_set_out_latency: 0.0,
            gpu_boost_lock_enabled: false,
            gpu_boost_lock_mhz: 1980,
            gpu_boost_lock_failsafe: true,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct OverlaySlotConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub file_path: Option<String>,
    #[serde(default = "default_gain")]
    pub gain: f32,
}

impl Default for OverlaySlotConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            file_path: None,
            gain: 0.5,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct VstSlotConfig {
    pub path: String,
    #[serde(default)]
    pub bypassed: bool,
    #[serde(default)]
    pub params: HashMap<String, f64>,
    #[serde(default)]
    pub component_state_hex: Option<String>,
    #[serde(default)]
    pub controller_state_hex: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct LineConfig {
    #[serde(default = "default_true")]
    pub is_enabled: bool,
    #[serde(default = "default_driver_mode")]
    pub driver_mode: String,
    #[serde(default)]
    pub input_device_name: Option<String>,
    #[serde(default)]
    pub output_device_name: Option<String>,
    #[serde(default)]
    pub asio_driver_name: Option<String>,
    #[serde(default = "default_asio_exp_buf")]
    pub asio_exp_in_samples: u32,
    #[serde(default = "default_asio_exp_buf")]
    pub asio_exp_out_samples: u32,
    #[serde(default = "default_vol")]
    pub master_volume: i32,
    #[serde(default)]
    pub is_muted: bool,
    #[serde(default = "default_true")]
    pub fail_closed: bool,
    #[serde(default = "default_true")]
    pub dc_blocker: bool,
    #[serde(default)]
    pub overlay_input: Vec<OverlaySlotConfig>,
    #[serde(default)]
    pub overlay_output: Vec<OverlaySlotConfig>,
    #[serde(default)]
    pub vst_slots: Vec<VstSlotConfig>,
}

impl Default for LineConfig {
    fn default() -> Self {
        Self {
            is_enabled: true,
            driver_mode: "wasapi_shared".to_string(),
            input_device_name: None,
            output_device_name: None,
            asio_driver_name: None,
            asio_exp_in_samples: 256,
            asio_exp_out_samples: 256,
            master_volume: 100,
            is_muted: false,
            fail_closed: true,
            dc_blocker: true,
            overlay_input: vec![OverlaySlotConfig::default()],
            overlay_output: vec![OverlaySlotConfig::default()],
            vst_slots: Vec::new(),
        }
    }
}

/// Konfigurasi khusus mode **⚡ SoundBridge ASIO** (`"Rinka SoundBridge ASIO"`):
/// Mendukung:
/// - Pilihan Buffer Size (`smp`: 64, 128, 256, 512, 1024, 2048, dst.)
/// - Sample Rate (`Hz`/`kHz`) otomatis mengikuti Rinka SoundBridge
/// - Bantalan Anti-Stutter GPU (`dsp_b_cushion_ms`) yang bebas dipilih manual oleh user (mis. 60, 70, 90, 100 ms)
/// - Pengali Peak GPU Stall (`gpu_stall_multiplier`: 1.01x – 1.30x, default 1.15x + 15 ms)
/// - Pilihan ASIO CPU Cores (`asio_cpu_cores`, default = 2 Cores)
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdaptiveCushionMode {
    #[serde(rename = "pysync_injection")]
    PySyncInjection = 0,
    #[serde(rename = "asio_stopwatch_outlier")]
    AsioStopwatchOutlier = 1,
    #[serde(rename = "decoupled_pacing")]
    DecoupledPacing = 2,
    #[serde(rename = "hybrid_dual_fusion")]
    HybridDualFusion = 3,
    #[serde(rename = "predictive_preemptive_plc")]
    PredictivePreemptivePlc = 4,
}

impl Default for AdaptiveCushionMode {
    fn default() -> Self {
        Self::HybridDualFusion
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DspRsbAsioConfig {
    #[serde(default = "default_true")]
    pub sync_buffer_size_with_rsb: bool,
    #[serde(default = "default_buffer_size")]
    pub manual_buffer_size: u32,
    #[serde(default = "default_true")]
    pub sync_sample_rate_with_rsb: bool,
    #[serde(default = "default_sample_rate")]
    pub manual_sample_rate: u32,
    #[serde(default = "default_buffer_size")]
    pub buffer_size: u32,
    #[serde(default = "default_true")]
    pub adaptive_cushion: bool,
    #[serde(default)]
    pub adaptive_mode: AdaptiveCushionMode,
    #[serde(default = "default_dsp_cushion_ms")]
    pub dsp_b_cushion_ms: f32,
    #[serde(default = "default_adaptive_extra_ms")]
    pub adaptive_extra_ms: f32,
    #[serde(default = "default_gpu_stall_mult")]
    pub gpu_stall_multiplier: f32,
    #[serde(default = "default_true")]
    pub micro_stretch_rescue: bool,
    #[serde(default = "default_true")]
    pub adaptive_reported_latency: bool,
    #[serde(default = "default_reported_in_ms")]
    pub reported_in_ms: f32,
    #[serde(default = "default_reported_out_ms")]
    pub reported_out_ms: f32,
    #[serde(default = "default_asio_cpu_cores")]
    pub asio_cpu_cores: u16,
    #[serde(default)]
    pub cpu_cores_user_configured: bool,

    #[serde(default)]
    pub set_in_latency_enabled: bool,
    #[serde(default = "default_asio_lat_ms")]
    pub in_latency_val: f32,
    #[serde(default = "default_true")]
    pub wasapi_in_set: bool,
    #[serde(default)]
    pub wasapi_in_exclusive: bool,
    #[serde(default = "default_true")]
    pub wasapi_in_autoconvert: bool,
    #[serde(default)]
    pub in_channels: u16,

    #[serde(default)]
    pub set_out_latency_enabled: bool,
    #[serde(default = "default_asio_lat_ms")]
    pub out_latency_val: f32,
    #[serde(default = "default_true")]
    pub wasapi_out_set: bool,
    #[serde(default)]
    pub wasapi_out_exclusive: bool,
    #[serde(default = "default_true")]
    pub wasapi_out_autoconvert: bool,
    #[serde(default)]
    pub out_channels: u16,
}

impl DspRsbAsioConfig {
    #[inline]
    pub fn effective_buffer_size(&self, rsb_global_buf: u32) -> u32 {
        if self.sync_buffer_size_with_rsb {
            rsb_global_buf.clamp(32, 4096)
        } else {
            self.manual_buffer_size.clamp(32, 4096)
        }
    }
}

impl Default for DspRsbAsioConfig {
    fn default() -> Self {
        Self {
            sync_buffer_size_with_rsb: true,
            manual_buffer_size: 256,
            sync_sample_rate_with_rsb: true,
            manual_sample_rate: 48000,
            buffer_size: 256,
            adaptive_cushion: true,
            adaptive_mode: AdaptiveCushionMode::default(),
            dsp_b_cushion_ms: 60.0,
            adaptive_extra_ms: 25.0,
            gpu_stall_multiplier: 1.15,
            micro_stretch_rescue: true,
            adaptive_reported_latency: true,
            reported_in_ms: 304.0,
            reported_out_ms: 16.0,
            asio_cpu_cores: 2,
            cpu_cores_user_configured: true,
            set_in_latency_enabled: false,
            in_latency_val: 0.0,
            wasapi_in_set: true,
            wasapi_in_exclusive: false,
            wasapi_in_autoconvert: true,
            in_channels: 0,
            set_out_latency_enabled: false,
            out_latency_val: 0.0,
            wasapi_out_set: true,
            wasapi_out_exclusive: false,
            wasapi_out_autoconvert: true,
            out_channels: 0,
        }
    }
}

/// Konfigurasi Murni **🎛️ RSB ASIO (Built-in FlexASIO GUI)**:
/// - Identik 1:1 dengan **FlexASIO GUI v0.35** (`Backend: Windows WASAPI`, `Set Buffer Size`,
///   `Input Device`, `Output Device`, `Output Preview`, `Set Input/Output Latency [0.0]`,
///   `WASAPI Set / Exclusive / AutoConvert`, `Channels`), ditambah fitur **`Auto-Sync Buffer Size with RSB`**.
/// - Nama Driver ASIO di Windows Registry & Vonovox: `"RSB ASIO"` (`CLSID {D8E4A975-42B1-4C9A-9F31-8A7B6C5D4E05}`),
///   sehingga 100% aman berdampingan dan tidak pernah konflik dengan FlexASIO eksternal.
/// - Tidak memiliki buffer/cushion buatan maupun auto-report kustom; bekerja murni seperti FlexASIO standar.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RsbAsioConfig {
    #[serde(default = "default_true")]
    pub sync_buffer_size_with_rsb: bool,
    #[serde(default = "default_true")]
    pub set_buffer_size_enabled: bool,
    #[serde(default = "default_buffer_size")]
    pub manual_buffer_size: u32,
    #[serde(default = "default_buffer_size")]
    pub buffer_size: u32,

    #[serde(default)]
    pub input_device: String,
    #[serde(default)]
    pub output_device: String,

    /// FlexASIO `suggestedLatencySeconds` untuk `[input]` (default `true` dengan nilai `0.0` detik, persis FlexASIO GUI)
    #[serde(default = "default_true")]
    pub set_in_latency_enabled: bool,
    #[serde(default = "default_asio_lat_ms")]
    pub in_latency_val: f32,

    /// FlexASIO `suggestedLatencySeconds` untuk `[output]` (default `true` dengan nilai `0.0` detik, persis FlexASIO GUI)
    #[serde(default = "default_true")]
    pub set_out_latency_enabled: bool,
    #[serde(default = "default_asio_lat_ms")]
    pub out_latency_val: f32,

    #[serde(default = "default_true")]
    pub wasapi_in_set: bool,
    #[serde(default)]
    pub wasapi_in_exclusive: bool,
    #[serde(default = "default_true")]
    pub wasapi_in_autoconvert: bool,
    #[serde(default)]
    pub in_channels: u16,

    #[serde(default = "default_true")]
    pub wasapi_out_set: bool,
    #[serde(default)]
    pub wasapi_out_exclusive: bool,
    #[serde(default = "default_true")]
    pub wasapi_out_autoconvert: bool,
    #[serde(default)]
    pub out_channels: u16,
}

impl RsbAsioConfig {
    #[inline]
    pub fn effective_buffer_size(&self, rsb_global_buf: u32) -> u32 {
        if self.sync_buffer_size_with_rsb {
            rsb_global_buf.clamp(32, 4096)
        } else if self.set_buffer_size_enabled {
            self.manual_buffer_size.clamp(32, 4096)
        } else {
            256
        }
    }

    /// Menghasilkan teks `Output Preview` berformat `FlexASIO.toml` persis seperti di FlexASIO GUI v0.35.
    pub fn render_toml_preview(&self, rsb_global_buf: u32, fallback_in: &str, fallback_out: &str) -> String {
        let mut lines = Vec::new();
        lines.push("backend = \"Windows WASAPI\"".to_string());
        if self.sync_buffer_size_with_rsb || self.set_buffer_size_enabled {
            lines.push(format!("bufferSizeSamples = {}", self.effective_buffer_size(rsb_global_buf)));
        }
        lines.push(String::new());
        lines.push("[input]".to_string());
        let in_dev = if !self.input_device.is_empty() {
            self.input_device.as_str()
        } else {
            fallback_in
        };
        lines.push(format!("device = \"{}\"", in_dev));
        if self.set_in_latency_enabled {
            lines.push(format!("suggestedLatencySeconds = {:.1}", self.in_latency_val.max(0.0)));
        }
        if self.wasapi_in_set {
            lines.push(format!(
                "wasapiExclusiveMode = {}",
                if self.wasapi_in_exclusive { "true" } else { "false" }
            ));
            lines.push(format!(
                "wasapiAutoConvert = {}",
                if self.wasapi_in_autoconvert { "true" } else { "false" }
            ));
        }
        if self.in_channels > 0 {
            lines.push(format!("channels = {}", self.in_channels));
        }

        lines.push(String::new());
        lines.push("[output]".to_string());
        let out_dev = if !self.output_device.is_empty() {
            self.output_device.as_str()
        } else {
            fallback_out
        };
        lines.push(format!("device = \"{}\"", out_dev));
        if self.set_out_latency_enabled {
            lines.push(format!("suggestedLatencySeconds = {:.1}", self.out_latency_val.max(0.0)));
        }
        if self.wasapi_out_set {
            lines.push(format!(
                "wasapiExclusiveMode = {}",
                if self.wasapi_out_exclusive { "true" } else { "false" }
            ));
            lines.push(format!(
                "wasapiAutoConvert = {}",
                if self.wasapi_out_autoconvert { "true" } else { "false" }
            ));
        }
        if self.out_channels > 0 {
            lines.push(format!("channels = {}", self.out_channels));
        }
        lines.join("\n")
    }
}

impl Default for RsbAsioConfig {
    fn default() -> Self {
        Self {
            sync_buffer_size_with_rsb: true,
            set_buffer_size_enabled: true,
            manual_buffer_size: 256,
            buffer_size: 256,
            input_device: String::new(),
            output_device: String::new(),
            set_in_latency_enabled: true,
            in_latency_val: 0.0,
            set_out_latency_enabled: true,
            out_latency_val: 0.0,
            wasapi_in_set: true,
            wasapi_in_exclusive: false,
            wasapi_in_autoconvert: true,
            in_channels: 0,
            wasapi_out_set: true,
            wasapi_out_exclusive: false,
            wasapi_out_autoconvert: true,
            out_channels: 0,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct AppConfig {
    #[serde(default)]
    pub global: GlobalConfig,
    #[serde(default)]
    pub line_a: LineConfig,
    #[serde(default)]
    pub line_b: LineConfig,
    #[serde(default)]
    pub asio_line_a: FlexAsioProfileConfig,
    #[serde(default)]
    pub asio_line_b: FlexAsioProfileConfig,
    #[serde(default)]
    pub asio_vc: FlexAsioProfileConfig,
    #[serde(default)]
    pub asio_dsp_rsb: DspRsbAsioConfig,
    #[serde(default)]
    pub rsb_asio: RsbAsioConfig,
}

impl AppConfig {
    pub fn load() -> Option<Self> {
        let cfg_path = get_config_path();
        let bak_path = cfg_path.with_extension("json.bak");

        // Migrasi otomatis file config lama dari root/induk ke dalam folder Savedata\
        if !cfg_path.exists() {
            let base = get_base_dir();
            let legacy_candidates = [
                base.join("rinka_soundbridge_config.json"),
                base.join("..").join("Savedata").join("rinka_soundbridge_config.json"),
                base.join("..").join("rinka_soundbridge_config.json"),
            ];
            for leg in &legacy_candidates {
                if leg.exists() && leg != &cfg_path {
                    let _ = fs::copy(leg, &cfg_path);
                    break;
                }
            }
        }
        // Bersihkan file config lepasan di root exe_dir jika sudah ada di Savedata\
        let root_legacy = get_base_dir().join("rinka_soundbridge_config.json");
        if cfg_path.exists() && root_legacy.exists() && root_legacy != cfg_path {
            let _ = fs::remove_file(&root_legacy);
            let _ = fs::remove_file(root_legacy.with_extension("json.bak"));
            let _ = fs::remove_file(root_legacy.with_extension("json.tmp"));
        }

        for (idx, path) in [&cfg_path, &bak_path].iter().enumerate() {
            if !path.exists() {
                continue;
            }
            match fs::read_to_string(path) {
                Ok(text) => match serde_json::from_str::<AppConfig>(&text) {
                    Ok(mut cfg) => {
                        if cfg.global.fifo_mode.to_lowercase().contains("classic") {
                            cfg.global.fifo_mode = "classic".to_string();
                        } else {
                            cfg.global.fifo_mode = "adaptive".to_string();
                        }
                        if idx == 1 {
                            log_warn("Config utama rusak/hilang - memakai backup (.bak)");
                        }
                        // Migrasi otomatis dari config versi sebelumnya jika profil Triple ASIO masih kosong
                        if cfg.asio_line_a.input_device.is_empty() {
                            if let Some(ref d) = cfg.line_a.input_device_name {
                                cfg.asio_line_a.input_device = d.clone();
                            }
                        }
                        if cfg.asio_line_a.output_device.is_empty() {
                            if let Some(ref d) = cfg.line_a.output_device_name {
                                cfg.asio_line_a.output_device = d.clone();
                            }
                        }
                        if cfg.asio_line_b.input_device.is_empty() {
                            if let Some(ref d) = cfg.line_b.input_device_name {
                                cfg.asio_line_b.input_device = d.clone();
                            }
                        }
                        if cfg.asio_line_b.output_device.is_empty() {
                            if let Some(ref d) = cfg.line_b.output_device_name {
                                cfg.asio_line_b.output_device = d.clone();
                            }
                        }
                        // Normalisasi nilai Set Latency lama (0.0 / 2.0) pada mode WASAPI Shared
                        for prof in [&mut cfg.asio_line_a, &mut cfg.asio_line_b, &mut cfg.asio_vc] {
                            if !prof.wasapi_in_exclusive && prof.in_latency_val <= 2.05 {
                                prof.set_in_latency_enabled = false;
                                prof.in_latency_val = 0.0;
                            }
                            if !prof.wasapi_out_exclusive && prof.out_latency_val <= 2.05 {
                                prof.set_out_latency_enabled = false;
                                prof.out_latency_val = 0.0;
                            }
                        }
                        // Pastikan ASIO CPU Cores default = 2 jika dimuat dari config versi lama yang belum memiliki flag
                        if !cfg.asio_dsp_rsb.cpu_cores_user_configured {
                            if cfg.asio_dsp_rsb.asio_cpu_cores == 0 {
                                cfg.asio_dsp_rsb.asio_cpu_cores = 2;
                            }
                            cfg.asio_dsp_rsb.cpu_cores_user_configured = true;
                        }
                        // Jangan pernah menimpa nilai dsp_b_cushion_ms yang diinput user (baik 60, 70, 90, 100, maupun 200 ms)!
                        if !cfg.asio_dsp_rsb.dsp_b_cushion_ms.is_finite()
                            || cfg.asio_dsp_rsb.dsp_b_cushion_ms <= 0.0
                        {
                            cfg.asio_dsp_rsb.dsp_b_cushion_ms = 60.0;
                        }
                        cfg.asio_dsp_rsb.dsp_b_cushion_ms =
                            cfg.asio_dsp_rsb.dsp_b_cushion_ms.clamp(20.0, 450.0);
                        cfg.asio_dsp_rsb.gpu_stall_multiplier =
                            cfg.asio_dsp_rsb.gpu_stall_multiplier.clamp(1.00, 1.30);
                        if !cfg.asio_dsp_rsb.adaptive_extra_ms.is_finite() || cfg.asio_dsp_rsb.adaptive_extra_ms < 0.0 {
                            cfg.asio_dsp_rsb.adaptive_extra_ms = 25.0;
                        }
                        cfg.asio_dsp_rsb.adaptive_extra_ms = cfg.asio_dsp_rsb.adaptive_extra_ms.clamp(0.0, 250.0);
                        // 4-Point Catmull-Rom Cubic Drift Synchronization selalu aktif secara otomatis di background
                        cfg.asio_dsp_rsb.micro_stretch_rescue = true;

                        if cfg.asio_dsp_rsb.reported_in_ms <= 0.0
                            || (cfg.asio_dsp_rsb.adaptive_reported_latency
                                && cfg.asio_dsp_rsb.reported_in_ms >= 480.0)
                        {
                            cfg.asio_dsp_rsb.reported_in_ms = 304.0;
                        }
                        if cfg.asio_dsp_rsb.reported_out_ms <= 0.0
                            || (cfg.asio_dsp_rsb.adaptive_reported_latency
                                && cfg.asio_dsp_rsb.reported_out_ms >= 180.0)
                        {
                            cfg.asio_dsp_rsb.reported_out_ms = 16.0;
                        }
                        // Normalisasi nilai warisan 2.0 (dari satuan ms lama) pada rsb_asio & asio_dsp_rsb
                        // agar tidak pernah terbaca sebagai 2.0 detik (2000.0 ms)!
                        if !cfg.rsb_asio.in_latency_val.is_finite()
                            || cfg.rsb_asio.in_latency_val < 0.0
                            || cfg.rsb_asio.in_latency_val >= 0.5
                        {
                            cfg.rsb_asio.in_latency_val = 0.0;
                        }
                        if !cfg.rsb_asio.out_latency_val.is_finite()
                            || cfg.rsb_asio.out_latency_val < 0.0
                            || cfg.rsb_asio.out_latency_val >= 0.5
                        {
                            cfg.rsb_asio.out_latency_val = 0.0;
                        }
                        if !cfg.asio_dsp_rsb.in_latency_val.is_finite()
                            || cfg.asio_dsp_rsb.in_latency_val < 0.0
                            || (!cfg.asio_dsp_rsb.wasapi_in_exclusive && cfg.asio_dsp_rsb.in_latency_val <= 2.05)
                        {
                            cfg.asio_dsp_rsb.set_in_latency_enabled = false;
                            cfg.asio_dsp_rsb.in_latency_val = 0.0;
                        }
                        if !cfg.asio_dsp_rsb.out_latency_val.is_finite()
                            || cfg.asio_dsp_rsb.out_latency_val < 0.0
                            || (!cfg.asio_dsp_rsb.wasapi_out_exclusive && cfg.asio_dsp_rsb.out_latency_val <= 2.05)
                        {
                            cfg.asio_dsp_rsb.set_out_latency_enabled = false;
                            cfg.asio_dsp_rsb.out_latency_val = 0.0;
                        }
                        return Some(cfg);
                    }
                    Err(e) => {
                        log_error(format!("Gagal parse config {:?}: {}", path.file_name(), e));
                    }
                },
                Err(e) => {
                    log_error(format!("Gagal membaca config {:?}: {}", path.file_name(), e));
                }
            }
        }
        None
    }

    pub fn save(&self) {
        let cfg_path = get_config_path();
        let tmp_path = cfg_path.with_extension("json.tmp");
        let bak_path = cfg_path.with_extension("json.bak");

        let json_str = match serde_json::to_string_pretty(self) {
            Ok(s) => s,
            Err(e) => {
                log_error(format!("Error serialisasi config: {}", e));
                return;
            }
        };

        if let Err(e) = fs::write(&tmp_path, json_str.as_bytes()) {
            log_error(format!("Error menulis config tmp: {}", e));
            return;
        }

        if cfg_path.exists() {
            if let Ok(old_text) = fs::read_to_string(&cfg_path) {
                if serde_json::from_str::<serde_json::Value>(&old_text).is_ok() {
                    let _ = fs::copy(&cfg_path, &bak_path);
                }
            }
        }

        if let Err(e) = fs::rename(&tmp_path, &cfg_path) {
            log_error(format!("Error mengganti file config: {}", e));
            let _ = fs::remove_file(&tmp_path);
        }
    }
}


pub const DEFAULT_ASIO_NOTES_TEXT: &str = "⚠️ PENTING / RECOMMENDED: Gunakan aplikasi pihak ke-3 (seperti MSI Afterburner atau EVGA Precision X1) untuk melakukan Lock GPU Boost Clock dibanding fitur Built-in GPU Boost Lock di atas; fitur Built-in Lock hanya disediakan sebagai cadangan jika diperlukan.\n\
💡 PERFORMANCE & LOW-LATENCY TIP (AI Voice Changer / Vonovox / RVC):\n\
Mengunci frekuensi GPU Boost Clock (mis. melalui MSI Afterburner / EVGA Precision X1) mencegah penurunan frekuensi (P-state downclocking) di sela-sela chunk suara 200-300 ms, sehingga GPU Delay tetap rata/stabil dan kualitas suara real-time 100% bebas stutter.";

pub fn get_asio_notes_txt_path() -> PathBuf {
    get_savedata_dir().join("asio_performance_notes.txt")
}

pub fn load_or_create_asio_notes_txt() -> String {
    let path = get_asio_notes_txt_path();
    if let Ok(text) = fs::read_to_string(&path) {
        let trimmed = text.trim();
        if !trimmed.is_empty() && !trimmed.starts_with("💡 PERFORMANCE & LOW-LATENCY TIP") {
            return text;
        }
    }
    let default_txt = DEFAULT_ASIO_NOTES_TEXT.to_string();
    let _ = fs::write(&path, default_txt.as_bytes());
    default_txt
}

pub fn save_asio_notes_txt(content: &str) {
    let path = get_asio_notes_txt_path();
    let _ = fs::write(&path, content.as_bytes());
}
