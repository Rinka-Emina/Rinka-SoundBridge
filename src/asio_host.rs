//! Native Windows ASIO Driver Host & Built-in FlexASIO Bridge (`src/asio_host.rs`)
//! Mengimplementasikan arsitektur 3-ASIO Mandiri yang tersinkronisasi penuh ke 1 setting terpadu:
//! 1. ASIO Mandiri 1 (`Line A` di dalam Rinka SoundBridge): `Input Line A -> VST Line A -> Output Line A`
//! 2. ASIO Mandiri 2 (`Line B` di dalam Rinka SoundBridge): `Input Line B -> VST Line B -> Output Line B`
//! 3. ASIO Mandiri 3 (`Rinka SoundBridge ASIO` untuk External VC / VC):
//!    Menggabungkan `Output Line A` (mis. `SSL Output 1/2`) dan `Input Line B` (mis. `Loopback 2`)
//!    menjadi instance FlexASIO tersendiri bernama `"Rinka SoundBridge ASIO"`, lengkap dengan:
//!    - Delegasi COM 1:1 ke mesin `FlexASIO.dll` asli + sinkronisasi otomatis `%USERPROFILE%\FlexASIO.toml`
//!    - Mesin fallback WASAPI Flex-ASIO (`WasapiFlexAsioPair` + `AudioFIFO`) mandiri di dalam DLL
//!    - Pemeriksa & Penginstal Otomatis Requirement ASIO (`FlexASIO` + `VC++ Redistributable`) saat `.exe` dijalankan!

use crate::config::{DspRsbAsioConfig, FlexAsioProfileConfig, RsbAsioConfig};
use crate::logger::{log_info, log_warn};
use crate::wasapi_exclusive::{FlexAsioEndpointOptions, WasapiEndpointTarget, WasapiFlexAsioPair};
use crate::win32::{boost_audio_thread, boost_virtual_asio_pump_thread, boost_virtual_asio_pump_thread_with_cores, ffi, set_asio_cpu_cores, sleep_high_res, to_wide};
use parking_lot::Mutex;
use std::ffi::{c_char, c_void};
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug)]
pub struct AsioDriverInfo {
    pub name: String,
    pub clsid_str: String,
    pub clsid: ffi::GUID,
}

/// Pindai semua driver ASIO yang terdaftar di `HKEY_LOCAL_MACHINE\SOFTWARE\ASIO` dan `HKEY_CURRENT_USER\SOFTWARE\ASIO`.
pub fn scan_asio_drivers() -> Vec<AsioDriverInfo> {
    let mut drivers = Vec::new();
    #[cfg(windows)]
    unsafe {
        for root_hive in [ffi::HKEY_LOCAL_MACHINE, ffi::HKEY_CURRENT_USER] {
            let subkey = to_wide("SOFTWARE\\ASIO");
            let mut hkey: ffi::HKEY = 0;
            if ffi::RegOpenKeyExW(root_hive, subkey.as_ptr(), 0, ffi::KEY_READ, &mut hkey)
                != ffi::ERROR_SUCCESS
            {
                continue;
            }

            let mut idx: u32 = 0;
            loop {
                let mut key_name = [0u16; 256];
                let mut key_len = 256u32;
                let mut ft = ffi::FILETIME::default();
                let res = ffi::RegEnumKeyExW(
                    hkey,
                    idx,
                    key_name.as_mut_ptr(),
                    &mut key_len,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    &mut ft,
                );
                if res != ffi::ERROR_SUCCESS {
                    break;
                }
                idx += 1;
                let drv_key_name = String::from_utf16_lossy(&key_name[..key_len as usize]);

                let mut sub_hkey: ffi::HKEY = 0;
                if ffi::RegOpenKeyExW(
                    hkey,
                    key_name.as_ptr(),
                    0,
                    ffi::KEY_READ,
                    &mut sub_hkey,
                ) == ffi::ERROR_SUCCESS
                {
                    let clsid_str = read_reg_sz(sub_hkey, "CLSID");
                    let desc_str = read_reg_sz(sub_hkey, "Description");
                    ffi::RegCloseKey(sub_hkey);

                    if let Some(c_str) = clsid_str {
                        let mut guid = ffi::GUID::default();
                        let w_clsid = to_wide(&c_str);
                        if ffi::CLSIDFromString(w_clsid.as_ptr(), &mut guid) >= 0 {
                            let display_name = desc_str
                                .filter(|s| !s.trim().is_empty())
                                .unwrap_or(drv_key_name);
                            drivers.push(AsioDriverInfo {
                                name: display_name,
                                clsid_str: c_str,
                                clsid: guid,
                            });
                        }
                    }
                }
            }

            ffi::RegCloseKey(hkey);
        }
    }
    drivers.dedup_by(|a, b| a.name == b.name);
    drivers
}

#[cfg(windows)]
unsafe fn read_reg_sz(hkey: ffi::HKEY, val_name: &str) -> Option<String> {
    let wname = to_wide(val_name);
    let name_ptr = if val_name.is_empty() {
        ptr::null()
    } else {
        wname.as_ptr()
    };
    let mut data_type: u32 = 0;
    let mut buf = [0u16; 512];
    let mut byte_len = (buf.len() * 2) as u32;
    let res = ffi::RegQueryValueExW(
        hkey,
        name_ptr,
        ptr::null_mut(),
        &mut data_type,
        buf.as_mut_ptr() as *mut u8,
        &mut byte_len,
    );
    if res == ffi::ERROR_SUCCESS && (data_type == ffi::REG_SZ || data_type == 2) && byte_len >= 2 {
        let u16_len = (byte_len as usize / 2).saturating_sub(1);
        let actual_len = buf[..u16_len].iter().position(|&c| c == 0).unwrap_or(u16_len);
        Some(String::from_utf16_lossy(&buf[..actual_len]))
    } else {
        None
    }
}

#[derive(Clone, Debug)]
struct SavedRinkaAsioRegistryConfig {
    sample_rate: u32,
    preferred_smp: u32,
    in_lat_samples: u32,
    out_lat_samples: u32,
    vc_profile: FlexAsioProfileConfig,
}

#[cfg(windows)]
unsafe fn read_saved_asio_reg_config() -> SavedRinkaAsioRegistryConfig {
    let subkey = to_wide(&format!("SOFTWARE\\ASIO\\{}", RINKA_ASIO_DRIVER_NAME));
    for hive in [ffi::HKEY_CURRENT_USER, ffi::HKEY_LOCAL_MACHINE] {
        let mut hkey: ffi::HKEY = 0;
        if ffi::RegOpenKeyExW(hive, subkey.as_ptr(), 0, ffi::KEY_READ, &mut hkey)
            == ffi::ERROR_SUCCESS
        {
            let sr = read_reg_sz(hkey, "SampleRate")
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(48000)
                .clamp(44100, 192000);
            let smp = read_reg_sz(hkey, "PreferredBufferSize")
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(256)
                .clamp(32, 2048);
            let in_lat = read_reg_sz(hkey, "InLatencySamples")
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(smp)
                .max(32);
            let out_lat = read_reg_sz(hkey, "OutLatencySamples")
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(smp)
                .max(32);
            let set_in_en = read_reg_sz(hkey, "SetInEnabled")
                .map(|s| s == "1" || s.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
            let set_in_val = read_reg_sz(hkey, "SetInLatency")
                .and_then(|s| s.parse::<f32>().ok())
                .unwrap_or(0.0);
            let set_out_en = read_reg_sz(hkey, "SetOutEnabled")
                .map(|s| s == "1" || s.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
            let set_out_val = read_reg_sz(hkey, "SetOutLatency")
                .and_then(|s| s.parse::<f32>().ok())
                .unwrap_or(0.0);

            let vc_in = read_reg_sz(hkey, "VcInputDevice").unwrap_or_default();
            let vc_out = read_reg_sz(hkey, "VcOutputDevice").unwrap_or_default();

            let wasapi_in_set = read_reg_sz(hkey, "WasapiInSet")
                .map(|s| s != "0" && !s.eq_ignore_ascii_case("false"))
                .unwrap_or(true);
            let wasapi_in_excl = read_reg_sz(hkey, "WasapiInExclusive")
                .map(|s| s == "1" || s.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
            let wasapi_in_auto = read_reg_sz(hkey, "WasapiInAutoConvert")
                .map(|s| s != "0" && !s.eq_ignore_ascii_case("false"))
                .unwrap_or(true);
            let in_ch = read_reg_sz(hkey, "InChannels")
                .and_then(|s| s.parse::<u16>().ok())
                .unwrap_or(0);

            let wasapi_out_set = read_reg_sz(hkey, "WasapiOutSet")
                .map(|s| s != "0" && !s.eq_ignore_ascii_case("false"))
                .unwrap_or(true);
            let wasapi_out_excl = read_reg_sz(hkey, "WasapiOutExclusive")
                .map(|s| s == "1" || s.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
            let wasapi_out_auto = read_reg_sz(hkey, "WasapiOutAutoConvert")
                .map(|s| s != "0" && !s.eq_ignore_ascii_case("false"))
                .unwrap_or(true);
            let out_ch = read_reg_sz(hkey, "OutChannels")
                .and_then(|s| s.parse::<u16>().ok())
                .unwrap_or(0);

            ffi::RegCloseKey(hkey);
            return SavedRinkaAsioRegistryConfig {
                sample_rate: sr,
                preferred_smp: smp,
                in_lat_samples: in_lat,
                out_lat_samples: out_lat,
                vc_profile: FlexAsioProfileConfig {
                    input_device: vc_in,
                    output_device: vc_out,
                    buffer_size: smp,
                    set_in_latency_enabled: set_in_en,
                    in_latency_val: set_in_val,
                    set_out_latency_enabled: set_out_en,
                    out_latency_val: set_out_val,
                    wasapi_in_set,
                    wasapi_in_exclusive: wasapi_in_excl,
                    wasapi_in_autoconvert: wasapi_in_auto,
                    wasapi_out_set,
                    wasapi_out_exclusive: wasapi_out_excl,
                    wasapi_out_autoconvert: wasapi_out_auto,
                    in_channels: in_ch,
                    out_channels: out_ch,
                    auto_reset_enabled: true,
                    auto_reset_interval_mins: 60,
                    auto_reset_cooldown_mins: 10,
                },
            };
        }
    }
    SavedRinkaAsioRegistryConfig {
        sample_rate: 48000,
        preferred_smp: 256,
        in_lat_samples: 256,
        out_lat_samples: 256,
        vc_profile: FlexAsioProfileConfig::default(),
    }
}

#[cfg(windows)]
unsafe fn read_saved_asio_reg_defaults() -> (u32, u32, u32, u32) {
    let c = read_saved_asio_reg_config();
    (c.sample_rate, c.preferred_smp, c.in_lat_samples, c.out_lat_samples)
}

#[derive(Clone, Copy, Debug)]
struct SavedDspRsbRegistryConfig {
    sample_rate: u32,
    preferred_smp: u32,
    adaptive_cushion: bool,
    adaptive_mode: u8,
    adaptive_extra_ms: f32,
    cushion_samples: u32,
    gpu_stall_mult_pct: u32,
    stall_guard_ms: f32,
    adaptive_reported: bool,
    reported_in_ms: f32,
    reported_out_ms: f32,
    cpu_cores: u16,
}

/// Menghitung `(in_lat_samples, out_lat_samples)` murni standar FlexASIO untuk `IASIO::getLatencies`
/// pada driver **"RSB ASIO" (Built-in FlexASIO GUI)**:
/// - `in_lat_ms` & `out_lat_ms` dalam satuan milidetik (`0.0..=1000.0 ms`, selaras 1:1 dengan `config.rs`).
pub fn compute_rsb_flexasio_reported_latency_samples(
    sr: u32,
    smp: u32,
    set_in_enabled: bool,
    in_lat_ms: f32,
    set_out_enabled: bool,
    out_lat_ms: f32,
) -> (u32, u32) {
    let sr_f = sr.max(44100) as f64;
    let smp_u = smp.clamp(32, 4096);
    let wasapi_10ms_smp = (0.010 * sr_f).round() as u32;
    let extra_in = if set_in_enabled && in_lat_ms > 0.0 {
        (((in_lat_ms.clamp(0.0, 1000.0) as f64) / 1000.0) * sr_f).round() as u32
    } else {
        0
    };
    let extra_out = if set_out_enabled && out_lat_ms > 0.0 {
        (((out_lat_ms.clamp(0.0, 1000.0) as f64) / 1000.0) * sr_f).round() as u32
    } else {
        0
    };
    let in_smp = (wasapi_10ms_smp + smp_u + extra_in).max(32);
    let out_smp = (wasapi_10ms_smp + smp_u * 2 + extra_out).max(32);
    (in_smp, out_smp)
}

#[derive(Clone, Debug)]
struct SavedRsbFlexAsioRegistryConfig {
    sample_rate: u32,
    preferred_smp: u32,
    input_device: String,
    output_device: String,
    set_in_enabled: bool,
    in_latency_ms: f32,
    set_out_enabled: bool,
    out_latency_ms: f32,
    wasapi_in_set: bool,
    wasapi_in_exclusive: bool,
    wasapi_in_autoconvert: bool,
    in_channels: u16,
    wasapi_out_set: bool,
    wasapi_out_exclusive: bool,
    wasapi_out_autoconvert: bool,
    out_channels: u16,
}

#[cfg(windows)]
unsafe fn read_saved_rsb_flex_asio_reg_config() -> SavedRsbFlexAsioRegistryConfig {
    let subkey = to_wide(&format!("SOFTWARE\\ASIO\\{}", RSB_ASIO_DRIVER_NAME));
    for hive in [ffi::HKEY_CURRENT_USER, ffi::HKEY_LOCAL_MACHINE] {
        let mut hkey: ffi::HKEY = 0;
        if ffi::RegOpenKeyExW(hive, subkey.as_ptr(), 0, ffi::KEY_READ, &mut hkey)
            == ffi::ERROR_SUCCESS
        {
            let sr = read_reg_sz(hkey, "SampleRate")
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(48000)
                .clamp(44100, 192000);
            let smp = read_reg_sz(hkey, "PreferredBufferSize")
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(256)
                .clamp(32, 2048);
            let input_device = read_reg_sz(hkey, "InputDevice").unwrap_or_default();
            let output_device = read_reg_sz(hkey, "OutputDevice").unwrap_or_default();
            let set_in_enabled = read_reg_sz(hkey, "SetInEnabled")
                .map(|s| s != "0" && !s.eq_ignore_ascii_case("false"))
                .unwrap_or(true);
            let raw_in_ms = read_reg_sz(hkey, "SetInLatency")
                .and_then(|s| s.parse::<f32>().ok())
                .unwrap_or(0.0);
            let in_latency_ms = if !raw_in_ms.is_finite() || raw_in_ms < 0.0 {
                0.0
            } else {
                raw_in_ms.clamp(0.0, 1000.0)
            };
            let set_out_enabled = read_reg_sz(hkey, "SetOutEnabled")
                .map(|s| s != "0" && !s.eq_ignore_ascii_case("false"))
                .unwrap_or(true);
            let raw_out_ms = read_reg_sz(hkey, "SetOutLatency")
                .and_then(|s| s.parse::<f32>().ok())
                .unwrap_or(0.0);
            let out_latency_ms = if !raw_out_ms.is_finite() || raw_out_ms < 0.0 {
                0.0
            } else {
                raw_out_ms.clamp(0.0, 1000.0)
            };
            let wasapi_in_set = read_reg_sz(hkey, "WasapiInSet")
                .map(|s| s != "0" && !s.eq_ignore_ascii_case("false"))
                .unwrap_or(true);
            let wasapi_in_exclusive = read_reg_sz(hkey, "WasapiInExclusive")
                .map(|s| s == "1" || s.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
            let wasapi_in_autoconvert = read_reg_sz(hkey, "WasapiInAutoConvert")
                .map(|s| s != "0" && !s.eq_ignore_ascii_case("false"))
                .unwrap_or(true);
            let in_channels = read_reg_sz(hkey, "InChannels")
                .and_then(|s| s.parse::<u16>().ok())
                .unwrap_or(0)
                .min(16);
            let wasapi_out_set = read_reg_sz(hkey, "WasapiOutSet")
                .map(|s| s != "0" && !s.eq_ignore_ascii_case("false"))
                .unwrap_or(true);
            let wasapi_out_exclusive = read_reg_sz(hkey, "WasapiOutExclusive")
                .map(|s| s == "1" || s.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
            let wasapi_out_autoconvert = read_reg_sz(hkey, "WasapiOutAutoConvert")
                .map(|s| s != "0" && !s.eq_ignore_ascii_case("false"))
                .unwrap_or(true);
            let out_channels = read_reg_sz(hkey, "OutChannels")
                .and_then(|s| s.parse::<u16>().ok())
                .unwrap_or(0)
                .min(16);
            ffi::RegCloseKey(hkey);
            return SavedRsbFlexAsioRegistryConfig {
                sample_rate: sr,
                preferred_smp: smp,
                input_device,
                output_device,
                set_in_enabled,
                in_latency_ms,
                set_out_enabled,
                out_latency_ms,
                wasapi_in_set,
                wasapi_in_exclusive,
                wasapi_in_autoconvert,
                in_channels,
                wasapi_out_set,
                wasapi_out_exclusive,
                wasapi_out_autoconvert,
                out_channels,
            };
        }
    }
    SavedRsbFlexAsioRegistryConfig {
        sample_rate: 48000,
        preferred_smp: 256,
        input_device: String::new(),
        output_device: String::new(),
        set_in_enabled: true,
        in_latency_ms: 0.0,
        set_out_enabled: true,
        out_latency_ms: 0.0,
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

/// Menghitung nilai sampel `IASIO::getLatencies(&input_lat, &output_lat)` untuk Low-Latency ASIO:
/// - Jika mode Adaptive aktif, melaporkan latensi ASIO murni (`smp` untuk Input dan `max(20ms, gpu_stall + 2*smp)` untuk Output)
///   tanpa tambahan 200ms WASAPI!
pub fn compute_asio_reported_latency_samples(
    sr: u32,
    smp: u32,
    reported_in_ms: f32,
    reported_out_ms: f32,
) -> (u32, u32) {
    let sr_f = sr.max(44100) as f64;
    let smp_u = smp.clamp(32, 4096);
    let is_low_lat_adaptive = reported_in_ms <= 0.0
        || ((reported_in_ms - 304.0).abs() < 6.0 && (reported_out_ms - 16.0).abs() < 6.0)
        || ((reported_in_ms - 504.0).abs() < 1.5 && (reported_out_ms - 210.7).abs() < 1.5);

    if is_low_lat_adaptive {
        let base_low_lat_smp = ((0.020 * sr_f).round() as u32).max(smp_u * 2);
        let measured_stall = get_dsp_rsb_bus_b_in().get_effective_gpu_stall_samples();
        let in_smp = smp_u.max(32);
        let out_smp = base_low_lat_smp.max(measured_stall + smp_u * 2).max(32);
        return (in_smp, out_smp);
    }

    let block_300ms_smp = (0.300 * sr_f).round() as u32;
    let pa_adapter_smp = (block_300ms_smp / smp_u) * smp_u;
    let pa_adapter_ms = (pa_adapter_smp as f64 / sr_f) * 1000.0;

    let target_in_ms = reported_in_ms.max(1.0) as f64;
    let eff_in_ms = if target_in_ms > pa_adapter_ms + 1.0 {
        target_in_ms - pa_adapter_ms
    } else {
        target_in_ms
    };
    let in_smp = (((eff_in_ms / 1000.0) * sr_f).round() as u32).max(32);
    let out_smp = ((((reported_out_ms.max(1.0) as f64) / 1000.0) * sr_f).round() as u32).max(32);
    (in_smp, out_smp)
}

#[cfg(windows)]
unsafe fn read_saved_dsp_rsb_asio_reg_config() -> SavedDspRsbRegistryConfig {
    let subkey = to_wide(&format!("SOFTWARE\\ASIO\\{}", DSP_RSB_ASIO_DRIVER_NAME));
    for hive in [ffi::HKEY_CURRENT_USER, ffi::HKEY_LOCAL_MACHINE] {
        let mut hkey: ffi::HKEY = 0;
        if ffi::RegOpenKeyExW(hive, subkey.as_ptr(), 0, ffi::KEY_READ, &mut hkey)
            == ffi::ERROR_SUCCESS
        {
            let sr = read_reg_sz(hkey, "SampleRate")
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(48000)
                .clamp(44100, 192000);
            let smp = read_reg_sz(hkey, "PreferredBufferSize")
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(256)
                .clamp(32, 2048);
            let adaptive_cush = read_reg_sz(hkey, "AdaptiveCushion")
                .map(|s| s != "0" && !s.eq_ignore_ascii_case("false"))
                .unwrap_or(true);
            let adaptive_rep = read_reg_sz(hkey, "AdaptiveReported")
                .map(|s| s != "0" && !s.eq_ignore_ascii_case("false"))
                .unwrap_or(true);
            let safe_default_cush = ((0.060 * (sr as f64)).round() as u32).clamp(960, 9600);
            let raw_cush = read_reg_sz(hkey, "CushionSamples")
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(safe_default_cush);
            let cush = raw_cush.min(24000);
            let gpu_stall_mult_pct = read_reg_sz(hkey, "GpuStallMultPct")
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(115)
                .clamp(100, 130);
            let stall_guard_ms = read_reg_sz(hkey, "StallGuardMs")
                .and_then(|s| s.parse::<f32>().ok())
                .unwrap_or(15.0)
                .clamp(0.0, 150.0);
            let adaptive_mode = read_reg_sz(hkey, "AdaptiveMode")
                .and_then(|s| s.parse::<u8>().ok())
                .unwrap_or(3)
                .min(5);
            let adaptive_extra_ms = read_reg_sz(hkey, "AdaptiveExtraMs")
                .and_then(|s| s.parse::<f32>().ok())
                .unwrap_or(25.0)
                .clamp(0.0, 250.0);
            let rep_in_ms = if adaptive_rep {
                304.0
            } else {
                read_reg_sz(hkey, "ReportedInMs")
                    .and_then(|s| s.parse::<f32>().ok())
                    .unwrap_or(304.0)
                    .clamp(1.0, 3000.0)
            };
            let rep_out_ms = if adaptive_rep {
                16.0
            } else {
                read_reg_sz(hkey, "ReportedOutMs")
                    .and_then(|s| s.parse::<f32>().ok())
                    .unwrap_or(16.0)
                    .clamp(1.0, 3000.0)
            };
            let cpu_cores = read_reg_sz(hkey, "AsioCpuCores")
                .and_then(|s| s.parse::<u16>().ok())
                .unwrap_or(2)
                .min(64);
            ffi::RegCloseKey(hkey);
            return SavedDspRsbRegistryConfig {
                sample_rate: sr,
                preferred_smp: smp,
                adaptive_cushion: adaptive_cush,
                adaptive_mode,
                adaptive_extra_ms,
                cushion_samples: cush,
                gpu_stall_mult_pct,
                stall_guard_ms,
                adaptive_reported: adaptive_rep,
                reported_in_ms: rep_in_ms,
                reported_out_ms: rep_out_ms,
                cpu_cores,
            };
        }
    }
    SavedDspRsbRegistryConfig {
        sample_rate: 48000,
        preferred_smp: 256,
        adaptive_cushion: true,
        adaptive_mode: 3,
        adaptive_extra_ms: 25.0,
        cushion_samples: 2880,
        gpu_stall_mult_pct: 115,
        stall_guard_ms: 15.0,
        adaptive_reported: true,
        reported_in_ms: 304.0,
        reported_out_ms: 16.0,
        cpu_cores: 2,
    }
}

// ==============================================================================
// PEMERIKSA & PENGINSTAL OTOMATIS REQUIREMENT ASIO (FLEXASIO + VC++ REDIST)
// ==============================================================================

static REQ_CHECK_STARTED: AtomicBool = AtomicBool::new(false);

/// Cari lokasi `FlexASIO.dll` dan `CLSID` miliknya di sistem Windows.
#[cfg(windows)]
pub unsafe fn find_flexasio_com_info() -> Option<(ffi::GUID, String)> {
    // 1. Cek di Registry HKLM\SOFTWARE\ASIO\FlexASIO atau HKCU\SOFTWARE\ASIO\FlexASIO
    let subkey = to_wide("SOFTWARE\\ASIO\\FlexASIO");
    for hive in [ffi::HKEY_LOCAL_MACHINE, ffi::HKEY_CURRENT_USER] {
        let mut hkey: ffi::HKEY = 0;
        if ffi::RegOpenKeyExW(hive, subkey.as_ptr(), 0, ffi::KEY_READ, &mut hkey)
            == ffi::ERROR_SUCCESS
        {
            let clsid_str = read_reg_sz(hkey, "CLSID");
            ffi::RegCloseKey(hkey);
            if let Some(c_str) = clsid_str {
                let mut guid = ffi::GUID::default();
                let w_clsid = to_wide(&c_str);
                if ffi::CLSIDFromString(w_clsid.as_ptr(), &mut guid) >= 0 {
                    // Cari path DLL di SOFTWARE\Classes\CLSID\<c_str>\InprocServer32
                    let inproc_sub = to_wide(&format!(
                        "SOFTWARE\\Classes\\CLSID\\{}\\InprocServer32",
                        c_str
                    ));
                    for cls_hive in [ffi::HKEY_LOCAL_MACHINE, ffi::HKEY_CURRENT_USER] {
                        let mut h_inproc: ffi::HKEY = 0;
                        if ffi::RegOpenKeyExW(
                            cls_hive,
                            inproc_sub.as_ptr(),
                            0,
                            ffi::KEY_READ,
                            &mut h_inproc,
                        ) == ffi::ERROR_SUCCESS
                        {
                            let dll_p = read_reg_sz(h_inproc, "");
                            ffi::RegCloseKey(h_inproc);
                            if let Some(p) = dll_p {
                                if std::path::Path::new(&p).exists() {
                                    return Some((guid, p));
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // 2. Cek di path instalasi standar FlexASIO (CLSID resmi FlexASIO: {1915226D-043D-4859-A92F-29F592252A34})
    let default_clsid = ffi::GUID {
        data1: 0x1915226D,
        data2: 0x043D,
        data3: 0x4859,
        data4: [0xA9, 0x2F, 0x29, 0xF5, 0x92, 0x25, 0x2A, 0x34],
    };
    let mut candidates = vec![
        PathBuf::from("C:\\Program Files\\FlexASIO\\x64\\FlexASIO.dll"),
        PathBuf::from("C:\\Program Files (x86)\\FlexASIO\\x64\\FlexASIO.dll"),
    ];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("Modules").join("FlexASIO.dll"));
            candidates.push(dir.join("FlexASIO.dll"));
            candidates.push(dir.join("redist").join("FlexASIO.dll"));
        }
    }
    for cand in candidates {
        if cand.exists() {
            return Some((default_clsid, cand.to_string_lossy().to_string()));
        }
    }
    None
}

/// Periksa ketersediaan requirement ASIO (`VC++ Redistributable x64` dan `FlexASIO Engine`)
/// saat `.exe` dijalankan; jika belum terinstal di PC user, instalkan secara otomatis!
pub fn ensure_asio_requirements_installed(run_in_background: bool) {
    if REQ_CHECK_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    if run_in_background {
        let _ = std::thread::Builder::new()
            .name("rinka-asio-req-installer".into())
            .spawn(|| {
                let _ = check_and_install_requirements_sync();
            });
    } else {
        let _ = check_and_install_requirements_sync();
    }
}

fn check_and_install_requirements_sync() -> Result<String, String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;

        let vc_redist_ok =
            std::path::Path::new("C:\\Windows\\System32\\vcruntime140.dll").exists();

        // Jangan pernah menyentuh instalasi atau file konfigurasi FlexASIO eksternal milik user!
        if vc_redist_ok {
            log_info("[ASIO Requirement] VC++ Runtime tersedia — langsung membuka aplikasi.");
            return Ok("VC++ Runtime is already installed.".to_string());
        }

        let mut notes = Vec::new();

        // 1. Jika VC++ Redistributable x64 belum ada, unduh & instalkan secara otomatis dengan verifikasi Authenticode
        {
            log_warn("[ASIO Requirement] vcruntime140.dll belum ditemukan, mengunduh & memverifikasi Microsoft VC++ Redistributable x64...");
            // Hanya jalankan winget.exe dari lokasi absolut WindowsApps (cegah CWD/PATH hijacking)
            if let Ok(local_app) = std::env::var("LOCALAPPDATA") {
                let winget_abs = std::path::PathBuf::from(local_app)
                    .join("Microsoft")
                    .join("WindowsApps")
                    .join("winget.exe");
                if winget_abs.exists() {
                    let _ = std::process::Command::new(&winget_abs)
                        .args([
                            "install",
                            "--id",
                            "Microsoft.VCRedist.2015+.x64",
                            "-e",
                            "--accept-source-agreements",
                            "--accept-package-agreements",
                            "--silent",
                        ])
                        .creation_flags(CREATE_NO_WINDOW)
                        .status();
                }
            }

            if !std::path::Path::new(r"C:\Windows\System32\vcruntime140.dll").exists() {
                let ps_abs = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";
                if std::path::Path::new(ps_abs).exists() {
                    let vc_dst = std::env::temp_dir().join(format!(
                        "rsb_vc_redist_{}.x64.exe",
                        std::process::id()
                    ));
                    // Hentikan bila unduhan gagal, wajibkan tanda tangan Authenticode Valid dari Microsoft Corporation,
                    // dan hapus file sementara setelah selesai atau gagal verifikasi!
                    let ps_vc = "$ErrorActionPreference='Stop'; \
                        [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12; \
                        $dst = $env:RSB_VC_DST; \
                        if (Test-Path -LiteralPath $dst) { Remove-Item -LiteralPath $dst -Force }; \
                        Invoke-WebRequest -UseBasicParsing -Uri 'https://aka.ms/vs/17/release/vc_redist.x64.exe' -OutFile $dst; \
                        $sig = Get-AuthenticodeSignature -LiteralPath $dst; \
                        if ($sig.Status -ne 'Valid' -or -not ($sig.SignerCertificate.Subject -like '*CN=Microsoft Corporation*')) { \
                            Remove-Item -LiteralPath $dst -Force -ErrorAction SilentlyContinue; \
                            throw 'Authenticode signature verification failed for vc_redist.x64.exe'; \
                        }; \
                        try { Start-Process -FilePath $dst -ArgumentList '/install','/quiet','/norestart' -Wait } finally { Remove-Item -LiteralPath $dst -Force -ErrorAction SilentlyContinue }";
                    let _ = std::process::Command::new(ps_abs)
                        .env("RSB_VC_DST", &vc_dst)
                        .args([
                            "-NoProfile",
                            "-NonInteractive",
                            "-ExecutionPolicy",
                            "Bypass",
                            "-WindowStyle",
                            "Hidden",
                            "-Command",
                            ps_vc,
                        ])
                        .creation_flags(CREATE_NO_WINDOW)
                        .status();
                } else {
                    log_warn("[ASIO Requirement] Trusted System32 PowerShell not found; skipping vc_redist download.");
                }
            }
            notes.push("VC++ Redistributable: Checked/Verified".to_string());
        }

        Ok(notes.join(" | "))
    }
    #[cfg(not(windows))]
    {
        Ok("Non-Windows".to_string())
    }
}

/// Membaca nilai `device = "..."` dari `%USERPROFILE%\FlexASIO.toml` jika sudah ada.
pub fn read_user_flexasio_toml_devices() -> (Option<String>, Option<String>) {
    let Some(user_profile) = std::env::var_os("USERPROFILE").map(PathBuf::from) else {
        return (None, None);
    };
    read_existing_flexasio_devices(&user_profile.join("FlexASIO.toml"))
}

fn read_existing_flexasio_devices(toml_path: &std::path::Path) -> (Option<String>, Option<String>) {
    let Ok(content) = std::fs::read_to_string(toml_path) else {
        return (None, None);
    };
    let mut section = "";
    let mut in_dev = None;
    let mut out_dev = None;
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = &line[1..line.len() - 1];
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            if k.trim() == "device" {
                let val = v.trim().trim_matches('"').trim_matches('\'').to_string();
                if !val.is_empty() && val != "(None)" {
                    if section.eq_ignore_ascii_case("input") {
                        in_dev = Some(val);
                    } else if section.eq_ignore_ascii_case("output") {
                        out_dev = Some(val);
                    }
                }
            }
        }
    }
    (in_dev, out_dev)
}

// ==============================================================================
// DEFINISI COM VTABLE STEINBERG IASIO (WINDOWS 64-BIT)
// ==============================================================================

pub type ASIOError = i32;
pub const ASE_OK: ASIOError = 0;
pub const ASE_SUCCESS: ASIOError = 0x3f4847a0;
pub const ASE_NO_CLOCK: ASIOError = -995;

pub type ASIOSampleType = i32;
pub const ASIOST_INT16_LSB: ASIOSampleType = 16;
pub const ASIOST_INT24_LSB: ASIOSampleType = 17;
pub const ASIOST_INT32_LSB: ASIOSampleType = 18;
pub const ASIOST_FLOAT32_LSB: ASIOSampleType = 19;
pub const ASIOST_FLOAT64_LSB: ASIOSampleType = 20;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ASIOChannelInfo {
    pub channel: i32,
    pub is_input: i32,
    pub is_active: i32,
    pub channel_group: i32,
    pub sample_type: ASIOSampleType,
    pub name: [c_char; 32],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ASIOBufferInfo {
    pub is_input: i32,
    pub channel_num: i32,
    pub buffers: [*mut c_void; 2],
}

unsafe impl Send for ASIOBufferInfo {}
unsafe impl Sync for ASIOBufferInfo {}

unsafe impl Send for ASIOCallbacks {}
unsafe impl Sync for ASIOCallbacks {}

unsafe impl Send for IASIOVtbl {}
unsafe impl Sync for IASIOVtbl {}

#[repr(C)]
pub struct ASIOCallbacks {
    pub buffer_switch: unsafe extern "C" fn(double_buffer_index: i32, direct_process: i32),
    pub sample_rate_did_change: unsafe extern "C" fn(s_rate: f64),
    pub asio_message: unsafe extern "C" fn(selector: i32, value: i32, message: *mut c_void, opt: *mut f64) -> i32,
    pub buffer_switch_time_info:
        unsafe extern "C" fn(params: *mut c_void, double_buffer_index: i32, direct_process: i32) -> *mut c_void,
}

#[repr(C)]
pub struct IASIOVtbl {
    pub query_interface: unsafe extern "system" fn(this: *mut c_void, riid: *const ffi::GUID, ppv: *mut *mut c_void) -> i32,
    pub add_ref: unsafe extern "system" fn(this: *mut c_void) -> u32,
    pub release: unsafe extern "system" fn(this: *mut c_void) -> u32,

    pub init: unsafe extern "system" fn(this: *mut c_void, sys_handle: *mut c_void) -> i32,
    pub get_driver_name: unsafe extern "system" fn(this: *mut c_void, name: *mut c_char),
    pub get_driver_version: unsafe extern "system" fn(this: *mut c_void) -> i32,
    pub get_error_message: unsafe extern "system" fn(this: *mut c_void, string: *mut c_char),
    pub start: unsafe extern "system" fn(this: *mut c_void) -> ASIOError,
    pub stop: unsafe extern "system" fn(this: *mut c_void) -> ASIOError,
    pub get_channels: unsafe extern "system" fn(this: *mut c_void, num_in: *mut i32, num_out: *mut i32) -> ASIOError,
    pub get_latencies: unsafe extern "system" fn(this: *mut c_void, input_lat: *mut i32, output_lat: *mut i32) -> ASIOError,
    pub get_buffer_size: unsafe extern "system" fn(
        this: *mut c_void,
        min_size: *mut i32,
        max_size: *mut i32,
        preferred_size: *mut i32,
        granularity: *mut i32,
    ) -> ASIOError,
    pub can_sample_rate: unsafe extern "system" fn(this: *mut c_void, sample_rate: f64) -> ASIOError,
    pub get_sample_rate: unsafe extern "system" fn(this: *mut c_void, sample_rate: *mut f64) -> ASIOError,
    pub set_sample_rate: unsafe extern "system" fn(this: *mut c_void, sample_rate: f64) -> ASIOError,
    pub get_clock_sources: unsafe extern "system" fn(this: *mut c_void, clocks: *mut c_void, num_sources: *mut i32) -> ASIOError,
    pub set_clock_source: unsafe extern "system" fn(this: *mut c_void, reference: i32) -> ASIOError,
    pub get_sample_position: unsafe extern "system" fn(this: *mut c_void, s_pos: *mut i64, t_stamp: *mut i64) -> ASIOError,
    pub get_channel_info: unsafe extern "system" fn(this: *mut c_void, info: *mut ASIOChannelInfo) -> ASIOError,
    pub create_buffers: unsafe extern "system" fn(
        this: *mut c_void,
        buffer_infos: *mut ASIOBufferInfo,
        num_channels: i32,
        buffer_size: i32,
        callbacks: *const ASIOCallbacks,
    ) -> ASIOError,
    pub dispose_buffers: unsafe extern "system" fn(this: *mut c_void) -> ASIOError,
    pub control_panel: unsafe extern "system" fn(this: *mut c_void) -> ASIOError,
    pub future: unsafe extern "system" fn(this: *mut c_void, selector: i32, opt: *mut c_void) -> ASIOError,
    pub output_ready: unsafe extern "system" fn(this: *mut c_void) -> ASIOError,
}

/// Buka jendela GUI Control Panel asli milik driver ASIO (mis. Focusrite / Voicemeeter / FlexASIO).
pub fn open_asio_control_panel(driver_name: &str) -> Result<(), String> {
    let drivers = scan_asio_drivers();
    let drv = drivers
        .iter()
        .find(|d| d.name == driver_name)
        .ok_or_else(|| format!("ASIO Driver '{}' not found in Registry", driver_name))?;

    #[cfg(windows)]
    unsafe {
        let _ = ffi::CoInitializeEx(ptr::null_mut(), ffi::COINIT_APARTMENTTHREADED);
        let mut iasio: *mut c_void = ptr::null_mut();
        let hr = ffi::CoCreateInstance(
            &drv.clsid,
            ptr::null_mut(),
            ffi::CLSCTX_INPROC_SERVER,
            &drv.clsid,
            &mut iasio,
        );
        if hr < 0 || iasio.is_null() {
            return Err(format!("Failed to load ASIO COM driver '{}' (HRESULT 0x{:08X})", driver_name, hr));
        }

        let vtbl = &**(iasio as *mut *const IASIOVtbl);
        let fg_hwnd = ffi::GetForegroundWindow();
        let _ = (vtbl.init)(iasio, fg_hwnd as *mut c_void);
        let err = (vtbl.control_panel)(iasio);
        (vtbl.release)(iasio);

        if err != ASE_OK && err != ASE_SUCCESS {
            return Err(format!("ASIO Driver '{}' does not have an external Control Panel (code {})", driver_name, err));
        }
        log_info(format!("[ASIO] Control Panel '{}' dibuka.", driver_name));
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = drv;
        Err("ASIO is only supported on Windows".into())
    }
}

// ==============================================================================
// ASIO FULL-DUPLEX ACTIVE STREAM ENGINE
// ==============================================================================

type AsioProcessCallback = Box<dyn FnMut(&[f32], &mut [f32], usize) + Send>;

struct AsioCallbackContext {
    buf_infos: Vec<ASIOBufferInfo>,
    in_ch_count: usize,
    out_ch_count: usize,
    in_types: [ASIOSampleType; 2],
    out_types: [ASIOSampleType; 2],
    buffer_size: usize,
    in_interleaved: Vec<f32>,
    out_interleaved: Vec<f32>,
    iasio_ptr: *mut c_void,
    boosted: bool,
    process_cb: AsioProcessCallback,
}

unsafe impl Send for AsioCallbackContext {}
unsafe impl Sync for AsioCallbackContext {}

static GLOBAL_ASIO_CTX: Mutex<Option<AsioCallbackContext>> = Mutex::new(None);

unsafe extern "C" fn asio_buffer_switch(double_buffer_index: i32, _direct_process: i32) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let idx = (double_buffer_index.clamp(0, 1)) as usize;
        let mut guard = match GLOBAL_ASIO_CTX.try_lock() {
            Some(g) => g,
            None => return,
        };
        let Some(ctx) = guard.as_mut() else {
            return;
        };

        if !ctx.boosted {
            ctx.boosted = true;
            if let Some(how) = boost_audio_thread() {
                log_info(format!("[ASIO] Thread callback real-time dioptimalkan ({})", how));
            }
        }

        let n = ctx.buffer_size;
        let in_ch = ctx.in_ch_count.clamp(1, 2);
        let out_ch = ctx.out_ch_count.clamp(1, 2);
        // 1. Konversi 1-2 kanal Input ASIO -> Interleaved Stereo f32
        if in_ch == 1 {
            let src_ptr = ctx.buf_infos[0].buffers[idx];
            let st = ctx.in_types[0];
            for i in 0..n {
                let s = read_asio_sample(src_ptr, st, i);
                ctx.in_interleaved[i * 2] = s;
                ctx.in_interleaved[i * 2 + 1] = s;
            }
        } else {
            for ch in 0..2 {
                let src_ptr = ctx.buf_infos[ch].buffers[idx];
                let st = ctx.in_types[ch];
                for i in 0..n {
                    ctx.in_interleaved[i * 2 + ch] = read_asio_sample(src_ptr, st, i);
                }
            }
        }

        // 2. Jalankan Pipeline DSP + VST3 + FIFO
        let in_ptr = ctx.in_interleaved.as_ptr();
        let out_ptr = ctx.out_interleaved.as_mut_ptr();
        let in_slice = std::slice::from_raw_parts(in_ptr, n * 2);
        let out_slice = std::slice::from_raw_parts_mut(out_ptr, n * 2);
        (ctx.process_cb)(in_slice, out_slice, n);

        // 3. Konversi Interleaved Stereo f32 -> 1-2 kanal Output ASIO
        let out_base = in_ch;
        if out_ch == 1 {
            let dst_ptr = ctx.buf_infos[out_base].buffers[idx];
            let st = ctx.out_types[0];
            for i in 0..n {
                let mono = 0.5 * (ctx.out_interleaved[i * 2] + ctx.out_interleaved[i * 2 + 1]);
                write_asio_sample(dst_ptr, st, i, mono);
            }
        } else {
            for ch in 0..2 {
                let dst_ptr = ctx.buf_infos[out_base + ch].buffers[idx];
                let st = ctx.out_types[ch];
                for i in 0..n {
                    write_asio_sample(dst_ptr, st, i, ctx.out_interleaved[i * 2 + ch]);
                }
            }
        }

        if !ctx.iasio_ptr.is_null() {
            let vtbl = &**(ctx.iasio_ptr as *mut *const IASIOVtbl);
            let _ = (vtbl.output_ready)(ctx.iasio_ptr);
        }
    }));
}

unsafe extern "C" fn asio_sample_rate_changed(s_rate: f64) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        log_warn(format!("[ASIO] Driver melaporkan perubahan sample rate ke {} Hz", s_rate));
    }));
}

unsafe extern "C" fn asio_message_cb(selector: i32, _value: i32, _message: *mut c_void, _opt: *mut f64) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match selector {
        1 => 1,      // kAsioSelectorSupported
        2 => 2,      // kAsioEngineVersion -> v2
        3 => 1,      // kAsioResetRequest
        _ => 0,
    }))
    .unwrap_or(0)
}

unsafe extern "C" fn asio_buffer_switch_time_info(
    params: *mut c_void,
    double_buffer_index: i32,
    direct_process: i32,
) -> *mut c_void {
    asio_buffer_switch(double_buffer_index, direct_process);
    params
}

static ASIO_CALLBACKS: ASIOCallbacks = ASIOCallbacks {
    buffer_switch: asio_buffer_switch,
    sample_rate_did_change: asio_sample_rate_changed,
    asio_message: asio_message_cb,
    buffer_switch_time_info: asio_buffer_switch_time_info,
};

#[inline]
unsafe fn read_asio_sample(buf: *mut c_void, st: ASIOSampleType, idx: usize) -> f32 {
    if buf.is_null() {
        return 0.0;
    }
    match st {
        ASIOST_INT16_LSB => {
            let v = *(buf as *const i16).add(idx);
            (v as f32) / 32768.0
        }
        ASIOST_INT24_LSB => {
            let p = (buf as *const u8).add(idx * 3);
            let b0 = *p as i32;
            let b1 = *p.add(1) as i32;
            let b2 = (*p.add(2) as i8) as i32;
            let val = b0 | (b1 << 8) | (b2 << 16);
            (val as f32) / 8388608.0
        }
        ASIOST_INT32_LSB => {
            let v = *(buf as *const i32).add(idx);
            ((v as f64) / 2147483648.0) as f32
        }
        ASIOST_FLOAT32_LSB => *(buf as *const f32).add(idx),
        ASIOST_FLOAT64_LSB => (*(buf as *const f64).add(idx)) as f32,
        _ => 0.0,
    }
}

#[inline]
unsafe fn write_asio_sample(buf: *mut c_void, st: ASIOSampleType, idx: usize, sample: f32) {
    if buf.is_null() {
        return;
    }
    let s = sample.clamp(-1.0, 1.0);
    match st {
        ASIOST_INT16_LSB => {
            *(buf as *mut i16).add(idx) = (s * 32767.0) as i16;
        }
        ASIOST_INT24_LSB => {
            let val = (s * 8388607.0) as i32;
            let p = (buf as *mut u8).add(idx * 3);
            *p = (val & 0xFF) as u8;
            *p.add(1) = ((val >> 8) & 0xFF) as u8;
            *p.add(2) = ((val >> 16) & 0xFF) as u8;
        }
        ASIOST_INT32_LSB => {
            *(buf as *mut i32).add(idx) = ((s as f64) * 2147483647.0) as i32;
        }
        ASIOST_FLOAT32_LSB => {
            *(buf as *mut f32).add(idx) = s;
        }
        ASIOST_FLOAT64_LSB => {
            *(buf as *mut f64).add(idx) = s as f64;
        }
        _ => {}
    }
}

pub struct AsioStreamHandle {
    iasio: *mut c_void,
    pub active_sample_rate: u32,
    pub active_buffer_size: u32,
    pub driver_in_latency_samples: u32,
    pub driver_out_latency_samples: u32,
}

unsafe impl Send for AsioStreamHandle {}
unsafe impl Sync for AsioStreamHandle {}

impl AsioStreamHandle {
    pub fn start(
        driver_name: &str,
        requested_sr: u32,
        requested_buf: u32,
        process_cb: AsioProcessCallback,
    ) -> Result<Self, String> {
        // Steinberg ASIO callbacks carry no user-data pointer, so only 1 native ASIO instance can run at a time.
        if GLOBAL_ASIO_CTX.lock().is_some() {
            return Err("Another ASIO Native stream is already active (ASIO ABI supports one active native driver per process)".into());
        }
        let drivers = scan_asio_drivers();
        let drv = drivers
            .iter()
            .find(|d| d.name == driver_name)
            .ok_or_else(|| format!("ASIO Driver '{}' not found", driver_name))?;

        #[cfg(windows)]
        unsafe {
            let _ = ffi::CoInitializeEx(ptr::null_mut(), ffi::COINIT_APARTMENTTHREADED);
            let mut iasio: *mut c_void = ptr::null_mut();
            let hr = ffi::CoCreateInstance(
                &drv.clsid,
                ptr::null_mut(),
                ffi::CLSCTX_INPROC_SERVER,
                &drv.clsid,
                &mut iasio,
            );
            if hr < 0 || iasio.is_null() {
                return Err(format!("ASIO CoCreateInstance failed (0x{:08X})", hr));
            }

            let vtbl = &**(iasio as *mut *const IASIOVtbl);
            let fg_hwnd = ffi::GetForegroundWindow();
            if (vtbl.init)(iasio, fg_hwnd as *mut c_void) == 0 {
                (vtbl.release)(iasio);
                return Err(format!("IASIO::init failed on driver '{}'", driver_name));
            }

            let mut num_in = 0i32;
            let mut num_out = 0i32;
            if (vtbl.get_channels)(iasio, &mut num_in, &mut num_out) != ASE_OK || num_in < 1 || num_out < 1 {
                (vtbl.release)(iasio);
                return Err(format!("Insufficient ASIO channels (In={}, Out={})", num_in, num_out));
            }

            if (vtbl.can_sample_rate)(iasio, requested_sr as f64) == ASE_OK {
                let _ = (vtbl.set_sample_rate)(iasio, requested_sr as f64);
            }
            let mut cur_sr = requested_sr as f64;
            let _ = (vtbl.get_sample_rate)(iasio, &mut cur_sr);
            let active_sr = (cur_sr.round() as u32).max(44100);

            let mut min_sz = 64i32;
            let mut max_sz = 2048i32;
            let mut pref_sz = requested_buf as i32;
            let mut gran = 0i32;
            let _ = (vtbl.get_buffer_size)(iasio, &mut min_sz, &mut max_sz, &mut pref_sz, &mut gran);
            let active_buf = if requested_buf as i32 >= min_sz && requested_buf as i32 <= max_sz {
                requested_buf as usize
            } else {
                pref_sz.max(64) as usize
            };

            let in_ch_count = (num_in as usize).min(2);
            let out_ch_count = (num_out as usize).min(2);
            let mut buf_infos = Vec::with_capacity(in_ch_count + out_ch_count);
            for ch in 0..in_ch_count {
                buf_infos.push(ASIOBufferInfo {
                    is_input: 1,
                    channel_num: ch as i32,
                    buffers: [ptr::null_mut(); 2],
                });
            }
            for ch in 0..out_ch_count {
                buf_infos.push(ASIOBufferInfo {
                    is_input: 0,
                    channel_num: ch as i32,
                    buffers: [ptr::null_mut(); 2],
                });
            }

            let mut in_types = [ASIOST_INT32_LSB; 2];
            let mut out_types = [ASIOST_INT32_LSB; 2];
            for ch in 0..in_ch_count {
                let mut cinfo: ASIOChannelInfo = std::mem::zeroed();
                cinfo.channel = ch as i32;
                cinfo.is_input = 1;
                if (vtbl.get_channel_info)(iasio, &mut cinfo) == ASE_OK {
                    in_types[ch] = cinfo.sample_type;
                }
            }
            for ch in 0..out_ch_count {
                let mut oinfo: ASIOChannelInfo = std::mem::zeroed();
                oinfo.channel = ch as i32;
                oinfo.is_input = 0;
                if (vtbl.get_channel_info)(iasio, &mut oinfo) == ASE_OK {
                    out_types[ch] = oinfo.sample_type;
                }
            }

            if (vtbl.create_buffers)(
                iasio,
                buf_infos.as_mut_ptr(),
                buf_infos.len() as i32,
                active_buf as i32,
                &ASIO_CALLBACKS,
            ) != ASE_OK
            {
                (vtbl.release)(iasio);
                return Err("IASIO::createBuffers failed".into());
            }

            let mut drv_in_lat: i32 = 0;
            let mut drv_out_lat: i32 = 0;
            let _ = (vtbl.get_latencies)(iasio, &mut drv_in_lat, &mut drv_out_lat);
            let driver_in_latency_samples = (drv_in_lat.max(0) as u32).max(active_buf as u32);
            let driver_out_latency_samples = (drv_out_lat.max(0) as u32).max(active_buf as u32);

            {
                let mut g = GLOBAL_ASIO_CTX.lock();
                if g.is_some() {
                    let _ = (vtbl.dispose_buffers)(iasio);
                    (vtbl.release)(iasio);
                    return Err("Another ASIO Native stream became active concurrently".into());
                }
                *g = Some(AsioCallbackContext {
                    buf_infos,
                    in_ch_count,
                    out_ch_count,
                    in_types,
                    out_types,
                    buffer_size: active_buf,
                    in_interleaved: vec![0.0; active_buf * 2],
                    out_interleaved: vec![0.0; active_buf * 2],
                    iasio_ptr: iasio,
                    boosted: false,
                    process_cb,
                });
            }

            if (vtbl.start)(iasio) != ASE_OK {
                *GLOBAL_ASIO_CTX.lock() = None;
                let _ = (vtbl.dispose_buffers)(iasio);
                (vtbl.release)(iasio);
                return Err("IASIO::start failed".into());
            }

            Ok(Self {
                iasio,
                active_sample_rate: active_sr,
                active_buffer_size: active_buf as u32,
                driver_in_latency_samples,
                driver_out_latency_samples,
            })
        }
        #[cfg(not(windows))]
        {
            let _ = (drv, requested_sr, requested_buf, process_cb);
            Err("ASIO is only available on Windows".into())
        }
    }
}

impl Drop for AsioStreamHandle {
    fn drop(&mut self) {
        #[cfg(windows)]
        unsafe {
            if !self.iasio.is_null() {
                let vtbl = &**(self.iasio as *mut *const IASIOVtbl);
                let _ = (vtbl.stop)(self.iasio);
                {
                    let mut g = GLOBAL_ASIO_CTX.lock();
                    if g.as_ref().map(|c| c.iasio_ptr == self.iasio).unwrap_or(false) {
                        *g = None;
                    }
                }
                let _ = (vtbl.dispose_buffers)(self.iasio);
                (vtbl.release)(self.iasio);
                self.iasio = ptr::null_mut();
            }
        }
    }
}

// ==============================================================================
// RINKA SOUNDBRIDGE ASIO - STANDALONE FLEXASIO ENGINE ("Rinka SoundBridge ASIO")
// ==============================================================================

pub const RINKA_ASIO_DRIVER_NAME: &str = "Rinka SoundBridge ASIO";
pub const DSP_RSB_ASIO_DRIVER_NAME: &str = "Rinka SoundBridge ASIO";
/// Built-in FlexASIO Driver bernama `"RSB ASIO"` (CLSID mandiri agar tidak pernah bentrok/menimpa FlexASIO eksternal)
pub const RSB_ASIO_DRIVER_NAME: &str = "RSB ASIO";

pub const CLSID_RINKA_ASIO: ffi::GUID = ffi::GUID {
    data1: 0xD8E4A971,
    data2: 0x42B1,
    data3: 0x4C9A,
    data4: [0x9F, 0x31, 0x8A, 0x7B, 0x6C, 0x5D, 0x4E, 0x01],
};
pub const CLSID_RINKA_ASIO_STR: &str = "{D8E4A971-42B1-4C9A-9F31-8A7B6C5D4E01}";
pub const CLSID_RINKA_ASIO_LEGACY_B_STR: &str = "{D8E4A972-42B1-4C9A-9F31-8A7B6C5D4E02}";

pub const CLSID_DSP_RSB_ASIO: ffi::GUID = ffi::GUID {
    data1: 0xD8E4A973,
    data2: 0x42B1,
    data3: 0x4C9A,
    data4: [0x9F, 0x31, 0x8A, 0x7B, 0x6C, 0x5D, 0x4E, 0x03],
};
pub const CLSID_DSP_RSB_ASIO_STR: &str = "{D8E4A973-42B1-4C9A-9F31-8A7B6C5D4E03}";

pub const CLSID_RSB_ASIO: ffi::GUID = ffi::GUID {
    data1: 0xD8E4A975,
    data2: 0x42B1,
    data3: 0x4C9A,
    data4: [0x9F, 0x31, 0x8A, 0x7B, 0x6C, 0x5D, 0x4E, 0x05],
};
pub const CLSID_RSB_ASIO_STR: &str = "{D8E4A975-42B1-4C9A-9F31-8A7B6C5D4E05}";

pub const IID_IUNKNOWN: ffi::GUID = ffi::GUID {
    data1: 0x00000000,
    data2: 0x0000,
    data3: 0x0000,
    data4: [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
};

pub const IID_ICLASSFACTORY: ffi::GUID = ffi::GUID {
    data1: 0x00000001,
    data2: 0x0000,
    data3: 0x0000,
    data4: [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
};

/// Parameter setting terbaru yang disinkronkan ke 3 engine ASIO (Line A, Line B, Rinka SoundBridge ASIO)
/// serta engine Pure Internal DSP ("DSP RSB Exp ASIO").
#[derive(Clone, Debug)]
pub struct AsioRegistrationSettings {
    pub sample_rate: u32,
    pub line_a: FlexAsioProfileConfig,
    pub line_b: FlexAsioProfileConfig,
    pub vc: FlexAsioProfileConfig,
    pub dsp_rsb: DspRsbAsioConfig,
    pub rsb_asio: RsbAsioConfig,
}

const SHM_RING_FRAMES: usize = 65536;
const SHM_RING_MASK: usize = SHM_RING_FRAMES - 1;
const SHM_MAGIC: u32 = 0x524E_4B38; // "RNK8"
const SHM_MAGIC_INITING: u32 = 0x524E_4B00;

/// Canonical Cross-Process `adaptive_flags` Bit Contract (shared 1:1 with ORION):
/// - bit  0      (`0x0000_0001`): `adaptive_cushion`
/// - bit  1      (`0x0000_0002`): `adaptive_reported_latency`
/// - bit  2      (`0x0000_0004`): `direct_py_probe_active`
/// - bits 8..15  (`0x0000_FF00`): `gpu_stall_mult_pct` (100..130)
/// - bit  16     (`0x0001_0000`): `pump_caught_up`
/// - bits 24..26 (`0x0700_0000`): `adaptive_mode` (0..=5)
/// - bit  27     (`0x0800_0000`): `SHM_FLAG_COORD_RESET_FROM_ORION` (ORION sets -> RSB checks & clears)
/// - bits 28..30 (`0x7000_0000`): `rsb_driver_mode` (0..=4)
/// - bit  31     (`0x8000_0000`): `SHM_FLAG_COORD_RESET_FROM_RSB` (RSB sets -> ORION checks & clears)
pub const SHM_FLAG_COORD_RESET_FROM_ORION: u32 = 1 << 27;
pub const SHM_FLAG_COORD_RESET_FROM_RSB: u32 = 1 << 31;

// ---- Spike Shield (Peak-Hold GPU Stall) -------------------------------------------------------
// External VC kadang melompat dari 30-80 ms ke 200-250 ms. Cushion HARUS sudah fisik ada sebelum lonjakan
// terjadi (tidak bisa dibuat sesudahnya), jadi nilai stall yang dipakai untuk Active Cushion adalah
// PEAK-HOLD: naik INSTAN ke durasi callback terbesar, ditahan `STALL_PEAK_HOLD_MS`, lalu turun
// perlahan `STALL_PEAK_RELEASE_MS_PER_S` ms stall per detik. Lonjakan berkelompok (GPU contention)
// jadi tertutup dari lonjakan ke-2 dan seterusnya.
const STALL_PEAK_HOLD_MS: u32 = 1_500;
const STALL_PEAK_RELEASE_MS_PER_S: u64 = 65;

#[repr(C)]
struct SharedBusMemoryLayout {
    magic: AtomicU32,
    config_gen: AtomicU32,
    sample_rate: AtomicU32,
    preferred_smp: AtomicU32,
    in_lat_samples: AtomicU32,
    out_lat_samples: AtomicU32,
    write_pos: AtomicU64,
    read_pos: AtomicU64,
    last_write_epoch_ms: AtomicU64,
    vc_connected_epoch_ms: AtomicU64,
    last_sample_l_bits: AtomicU32,
    last_sample_r_bits: AtomicU32,
    read_frac_bits: AtomicU32,
    fade_in_pending: AtomicU32,
    measured_gpu_stall_samples: AtomicU32,
    adaptive_flags: AtomicU32,
    left: [AtomicU32; SHM_RING_FRAMES],
    right: [AtomicU32; SHM_RING_FRAMES],
    // ---- Diagnostik (hanya counter; dibaca oleh `asio_diag_poll()` di RinkaSoundBridge.exe) ----
    diag_silent_frames: AtomicU64, // total frame yang di-output sebagai senyap karena underrun/hold
    diag_underruns: AtomicU32,     // jumlah kejadian bus kehabisan sampel (dropout)
    diag_jumps: AtomicU32,         // jumlah kejadian bus melompat karena lag > max_lag (audio terbuang)
    diag_cb_max_us: AtomicU32,     // durasi `buffer_switch` terlama sejak poll terakhir (= GPU delay External VC)
    diag_stall_count: AtomicU32,   // jumlah callback panjang (> 2 blok) = jumlah chunk yang diproses External VC
    diag_lag_max_us: AtomicU32,    // keterlambatan terbesar pump dari jadwal wall-clock sejak poll terakhir
    diag_clock_resets: AtomicU32,  // berapa kali pump menyerah mengejar jadwal (> 1200 ms tertinggal)
}

const _: () = assert!(std::mem::size_of::<SharedBusMemoryLayout>() == 524_400);

#[cfg(windows)]
unsafe fn mapped_view_has_min_size(view: *const c_void, min_bytes: usize) -> bool {
    if view.is_null() {
        return false;
    }
    let mut mbi: ffi::MEMORY_BASIC_INFORMATION = std::mem::zeroed();
    let ret = ffi::VirtualQuery(
        view,
        &mut mbi,
        std::mem::size_of::<ffi::MEMORY_BASIC_INFORMATION>(),
    );
    ret >= std::mem::size_of::<ffi::MEMORY_BASIC_INFORMATION>() && mbi.RegionSize >= min_bytes
}

pub struct SharedAsioBus {
    #[cfg(windows)]
    _h_map: ffi::HANDLE,
    #[cfg(windows)]
    h_event: ffi::HANDLE,
    ptr: *mut SharedBusMemoryLayout,
    _fallback: Option<Box<SharedBusMemoryLayout>>,
}

unsafe impl Send for SharedAsioBus {}
unsafe impl Sync for SharedAsioBus {}

impl Drop for SharedAsioBus {
    fn drop(&mut self) {
        #[cfg(windows)]
        unsafe {
            // Only unmap if `ptr` came from `MapViewOfFile` (i.e. not owned by `_fallback: Some(Box)`).
            if self._fallback.is_none() && !self.ptr.is_null() {
                let _ = ffi::UnmapViewOfFile(self.ptr as *const c_void);
                self.ptr = ptr::null_mut();
            }
            if !self._h_map.is_null() {
                let _ = ffi::CloseHandle(self._h_map);
                self._h_map = ptr::null_mut();
            }
            if !self.h_event.is_null() {
                let _ = ffi::CloseHandle(self.h_event);
                self.h_event = ptr::null_mut();
            }
        }
    }
}

#[inline]
fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(1)
}

#[inline]
fn now_epoch_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1)
}

pub const ORION_TELEMETRY_MAGIC: u32 = 0x4F52_494F; // "ORIO"
pub const ORION_TELEMETRY_SHM_NAME: &str = "Local\\ORION_GPU_TELEMETRY_V1";
pub const ORION_TELEMETRY_FILE_NAME: &str = "orion_gpu_telemetry.bin";

/// 80-Byte Binary Layout (`#[repr(C)]`, Little-Endian) dari ORION Real-Time GPU Telemetry IPC (`Local\ORION_GPU_TELEMETRY_V1`
/// dan `%TEMP%\orion_gpu_telemetry.bin`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct OrionGpuTelemetryPacket {
    pub magic: u32,
    pub version: u32,
    pub sequence: u64,
    pub timestamp_us: u64,
    pub gpu_delay_ms: f32,
    pub ema_gpu_delay_ms: f32,
    pub min_gpu_delay_ms: f32,
    pub max_gpu_delay_ms: f32,
    pub cushion_target_ms: f32,
    pub e2e_delay_ms: f32,
    pub hubert_ms: f32,
    pub rmvpe_ms: f32,
    pub synth_ms: f32,
    pub wsola_dsp_ms: f32,
    pub sample_rate: u32,
    pub block_frames: u32,
    pub is_streaming: u32,
    pub checksum: u32,
}

impl OrionGpuTelemetryPacket {
    #[inline]
    pub fn is_valid(&self) -> bool {
        self.magic == ORION_TELEMETRY_MAGIC
            && self.version == 1
            && self.checksum == (self.magic ^ (self.sequence as u32) ^ self.is_streaming)
    }

    #[inline]
    pub fn is_fresh_and_active(&self) -> bool {
        if !self.is_valid() || self.is_streaming == 0 {
            return false;
        }
        if self.timestamp_us > 1_000_000_000_000_000 {
            let now_us = now_epoch_ns() / 1_000;
            // Synchronized 2.5s epoch freshness window (ORION v5.5+ broadcasts every 100ms + every audio callback)
            if now_us > self.timestamp_us && now_us.saturating_sub(self.timestamp_us) > 2_500_000 {
                return false;
            }
        } else {
            // Sequence-progression watchdog fallback if an older ORION build uses monotonic perf_counter()
            let now_ms = now_epoch_ms();
            let prev_seq = LAST_ORION_SEQ.load(Ordering::Relaxed);
            if self.sequence != prev_seq {
                LAST_ORION_SEQ.store(self.sequence, Ordering::Relaxed);
                LAST_ORION_SEQ_EPOCH_MS.store(now_ms, Ordering::Relaxed);
            } else {
                let last_ms = LAST_ORION_SEQ_EPOCH_MS.load(Ordering::Relaxed);
                if last_ms > 0 && now_ms.saturating_sub(last_ms) > 2500 {
                    return false;
                }
            }
        }
        self.gpu_delay_ms.is_finite() && self.gpu_delay_ms > 0.0 && self.gpu_delay_ms <= 2000.0
    }
}

static ORION_SHM_VIEW_PTR: AtomicUsize = AtomicUsize::new(0);
static LAST_ORION_POLL_MS: AtomicU64 = AtomicU64::new(0);
static LAST_ORION_DISK_FALLBACK_MS: AtomicU64 = AtomicU64::new(0);
static LAST_ORION_SEQ: AtomicU64 = AtomicU64::new(0);
static LAST_ORION_SEQ_EPOCH_MS: AtomicU64 = AtomicU64::new(0);
static ORION_WAS_STREAMING: AtomicBool = AtomicBool::new(false);

#[inline]
fn get_orion_shm_ptr() -> *const OrionGpuTelemetryPacket {
    let cached = ORION_SHM_VIEW_PTR.load(Ordering::Acquire);
    if cached != 0 {
        return cached as *const OrionGpuTelemetryPacket;
    }
    #[cfg(windows)]
    unsafe {
        let wname = to_wide(ORION_TELEMETRY_SHM_NAME);
        let byte_size = std::mem::size_of::<OrionGpuTelemetryPacket>() as u32;
        let h_map = ffi::CreateFileMappingW(
            ffi::INVALID_HANDLE_VALUE,
            ptr::null_mut(),
            ffi::PAGE_READWRITE,
            0,
            byte_size,
            wname.as_ptr(),
        );
        if !h_map.is_null() {
            let view = ffi::MapViewOfFile(
                h_map,
                ffi::FILE_MAP_ALL_ACCESS,
                0,
                0,
                byte_size as usize,
            );
            if !view.is_null() {
                if !mapped_view_has_min_size(view, byte_size as usize) {
                    let _ = ffi::UnmapViewOfFile(view);
                    ffi::CloseHandle(h_map);
                    return ptr::null();
                }
                let addr = view as usize;
                match ORION_SHM_VIEW_PTR.compare_exchange(0, addr, Ordering::AcqRel, Ordering::Acquire) {
                    Ok(_) => return addr as *const OrionGpuTelemetryPacket,
                    Err(existing) => {
                        let _ = ffi::UnmapViewOfFile(view);
                        ffi::CloseHandle(h_map);
                        return existing as *const OrionGpuTelemetryPacket;
                    }
                }
            }
            ffi::CloseHandle(h_map);
        }
    }
    ptr::null()
}

/// Membaca snapshot konsisten dari Shared Memory dengan double-read byte-for-byte (maks 4x)
/// sehingga seluruh 80 byte (termasuk 10 field float melintasi batas cache line 64B) bebas torn-read.
#[cfg(windows)]
#[inline]
unsafe fn read_orion_shm_consistent(shm_ptr: *const OrionGpuTelemetryPacket) -> Option<OrionGpuTelemetryPacket> {
    const N: usize = std::mem::size_of::<OrionGpuTelemetryPacket>();
    let byte_ptr = shm_ptr as *const u8;
    for _ in 0..4 {
        let mut b1 = [0u8; N];
        for i in 0..N {
            b1[i] = ptr::read_volatile(byte_ptr.add(i));
        }
        std::sync::atomic::fence(Ordering::Acquire);
        let mut b2 = [0u8; N];
        for i in 0..N {
            b2[i] = ptr::read_volatile(byte_ptr.add(i));
        }
        if b1 == b2 {
            let pkt: OrionGpuTelemetryPacket = ptr::read_unaligned(b1.as_ptr() as *const OrionGpuTelemetryPacket);
            if pkt.is_valid() {
                return Some(pkt);
            }
        }
    }
    None
}

/// Membaca paket telemetri GPU ORION khusus dari Shared Memory (`< 50 ns`, 100% Zero Disk I/O)
/// untuk dipanggil dengan aman di dalam real-time ASIO audio callback (`update_mode6_orion_sw_fusion`).
#[inline]
pub fn read_orion_gpu_telemetry_shm_only() -> Option<OrionGpuTelemetryPacket> {
    #[cfg(windows)]
    unsafe {
        let shm_ptr = get_orion_shm_ptr();
        if !shm_ptr.is_null() {
            if let Some(pkt) = read_orion_shm_consistent(shm_ptr) {
                if pkt.is_fresh_and_active() {
                    return Some(pkt);
                }
            }
        }
    }
    None
}

pub const AUTORESET_SYNC_SHM_NAME: &str = "Local\\RinkaOrionAutoResetSync_v1";
pub const AUTORESET_SYNC_MAGIC: u32 = 0x5253_4241; // "RSBA"

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct OrionAutoResetSyncPacket {
    pub magic: u32,
    pub version: u32,
    pub config_seq: u32,
    pub enabled: u32,
    pub interval_mins: u32,
    pub cooldown_mins: u32,
    pub trigger_seq: u32,
    pub reset_count: u32,
    pub last_reset_epoch_ms: u64,
    pub writer_id: u32, // 1 = RSB, 2 = ORION
    pub reserved: [u32; 5],
}

static AUTORESET_SYNC_SHM_PTR: AtomicUsize = AtomicUsize::new(0);

#[inline]
fn get_autoreset_sync_shm_ptr() -> *mut OrionAutoResetSyncPacket {
    let cached = AUTORESET_SYNC_SHM_PTR.load(Ordering::Acquire);
    if cached != 0 {
        return cached as *mut OrionAutoResetSyncPacket;
    }
    #[cfg(windows)]
    unsafe {
        let wide: Vec<u16> = AUTORESET_SYNC_SHM_NAME
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let h_map = ffi::CreateFileMappingW(
            ffi::INVALID_HANDLE_VALUE,
            ptr::null_mut(),
            ffi::PAGE_READWRITE,
            0,
            64,
            wide.as_ptr(),
        );
        if !h_map.is_null() {
            let view = ffi::MapViewOfFile(h_map, ffi::FILE_MAP_ALL_ACCESS, 0, 0, 64);
            if !view.is_null() {
                if !mapped_view_has_min_size(view, std::mem::size_of::<OrionAutoResetSyncPacket>()) {
                    let _ = ffi::UnmapViewOfFile(view);
                    ffi::CloseHandle(h_map);
                    return ptr::null_mut();
                }
                let addr = view as usize;
                match AUTORESET_SYNC_SHM_PTR.compare_exchange(0, addr, Ordering::AcqRel, Ordering::Acquire) {
                    Ok(_) => {
                        let pkt_ptr = view as *mut OrionAutoResetSyncPacket;
                        let magic_atomic = &*(pkt_ptr as *const AtomicU32);
                        if magic_atomic.load(Ordering::Acquire) != AUTORESET_SYNC_MAGIC {
                            // Tulis seluruh payload terlebih dahulu, lalu publikasikan `magic` TERAKHIR dengan Release!
                            ptr::write_volatile(&mut (*pkt_ptr).version, 1);
                            ptr::write_volatile(&mut (*pkt_ptr).config_seq, 1);
                            ptr::write_volatile(&mut (*pkt_ptr).enabled, 1);
                            ptr::write_volatile(&mut (*pkt_ptr).interval_mins, 60);
                            ptr::write_volatile(&mut (*pkt_ptr).cooldown_mins, 10);
                            ptr::write_volatile(&mut (*pkt_ptr).trigger_seq, 0);
                            ptr::write_volatile(&mut (*pkt_ptr).reset_count, 0);
                            ptr::write_volatile(&mut (*pkt_ptr).last_reset_epoch_ms, now_epoch_ms());
                            ptr::write_volatile(&mut (*pkt_ptr).writer_id, 1);
                            ptr::write_volatile(&mut (*pkt_ptr).reserved, [0; 5]);
                            std::sync::atomic::fence(Ordering::Release);
                            magic_atomic.store(AUTORESET_SYNC_MAGIC, Ordering::Release);
                        }
                        return addr as *mut OrionAutoResetSyncPacket;
                    }
                    Err(existing) => {
                        let _ = ffi::UnmapViewOfFile(view);
                        ffi::CloseHandle(h_map);
                        return existing as *mut OrionAutoResetSyncPacket;
                    }
                }
            }
            ffi::CloseHandle(h_map);
        }
    }
    ptr::null_mut()
}

fn write_autoreset_sync_json_mirror(pkt: &OrionAutoResetSyncPacket) {
    let path = std::env::temp_dir().join("rinka_orion_autoreset_sync.json");
    let json_str = format!(
        "{{\"config_seq\":{},\"enabled\":{},\"interval_mins\":{},\"cooldown_mins\":{},\"trigger_seq\":{},\"reset_count\":{},\"last_reset_epoch_ms\":{},\"writer_id\":{}}}",
        pkt.config_seq,
        pkt.enabled != 0,
        pkt.interval_mins.clamp(1, 240),
        pkt.cooldown_mins.clamp(1, 240),
        pkt.trigger_seq,
        pkt.reset_count,
        pkt.last_reset_epoch_ms,
        pkt.writer_id
    );
    let tmp_path = std::env::temp_dir().join("rinka_orion_autoreset_sync.json.tmp");
    if std::fs::write(&tmp_path, &json_str).is_ok() {
        if std::fs::rename(&tmp_path, &path).is_err() {
            let _ = std::fs::write(&path, &json_str);
            let _ = std::fs::remove_file(&tmp_path);
        }
    } else {
        let _ = std::fs::write(&path, &json_str);
    }
}

pub fn read_autoreset_sync_shm() -> Option<OrionAutoResetSyncPacket> {
    #[cfg(windows)]
    unsafe {
        let p = get_autoreset_sync_shm_ptr();
        if !p.is_null() {
            const N: usize = std::mem::size_of::<OrionAutoResetSyncPacket>();
            let byte_ptr = p as *const u8;
            for _ in 0..4 {
                let mut b1 = [0u8; N];
                for i in 0..N {
                    b1[i] = ptr::read_volatile(byte_ptr.add(i));
                }
                std::sync::atomic::fence(Ordering::Acquire);
                let mut b2 = [0u8; N];
                for i in 0..N {
                    b2[i] = ptr::read_volatile(byte_ptr.add(i));
                }
                if b1 == b2 {
                    let pkt: OrionAutoResetSyncPacket =
                        ptr::read_unaligned(b1.as_ptr() as *const OrionAutoResetSyncPacket);
                    if pkt.magic == AUTORESET_SYNC_MAGIC {
                        return Some(pkt);
                    }
                }
            }
        }
    }
    None
}

pub fn publish_rsb_autoreset_config_shm(enabled: bool, interval_mins: u32, cooldown_mins: u32) -> u32 {
    #[cfg(windows)]
    unsafe {
        let p = get_autoreset_sync_shm_ptr();
        if !p.is_null() {
            let mut pkt = ptr::read_volatile(p);
            if pkt.magic != AUTORESET_SYNC_MAGIC {
                pkt.magic = AUTORESET_SYNC_MAGIC;
                pkt.version = 1;
                pkt.last_reset_epoch_ms = now_epoch_ms();
            }
            pkt.config_seq = pkt.config_seq.wrapping_add(1);
            pkt.enabled = if enabled { 1 } else { 0 };
            pkt.interval_mins = interval_mins.clamp(1, 240);
            pkt.cooldown_mins = cooldown_mins.clamp(1, 240);
            pkt.writer_id = 1; // 1 = RSB
            ptr::write_volatile(p, pkt);
            std::sync::atomic::fence(Ordering::Release);
            write_autoreset_sync_json_mirror(&pkt);
            return pkt.config_seq;
        }
    }
    0
}

pub fn sync_rsb_autoreset_epoch_ms_shm(rsb_reset_epoch_ms: u64) {
    #[cfg(windows)]
    unsafe {
        let p = get_autoreset_sync_shm_ptr();
        if !p.is_null() {
            let mut pkt = ptr::read_volatile(p);
            if pkt.magic == AUTORESET_SYNC_MAGIC && rsb_reset_epoch_ms > 0 {
                pkt.last_reset_epoch_ms = rsb_reset_epoch_ms;
                ptr::write_volatile(p, pkt);
                std::sync::atomic::fence(Ordering::Release);
                write_autoreset_sync_json_mirror(&pkt);
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = rsb_reset_epoch_ms;
    }
}

pub fn get_current_epoch_ms() -> u64 {
    now_epoch_ms()
}

pub fn publish_rsb_autoreset_trigger_shm(reset_count: u32) -> u32 {
    #[cfg(windows)]
    unsafe {
        let p = get_autoreset_sync_shm_ptr();
        if !p.is_null() {
            let mut pkt = ptr::read_volatile(p);
            if pkt.magic != AUTORESET_SYNC_MAGIC {
                pkt.magic = AUTORESET_SYNC_MAGIC;
                pkt.version = 1;
                pkt.enabled = 1;
                pkt.interval_mins = 60;
                pkt.cooldown_mins = 10;
            }
            pkt.trigger_seq = pkt.trigger_seq.wrapping_add(1);
            pkt.reset_count = reset_count;
            pkt.last_reset_epoch_ms = now_epoch_ms();
            pkt.writer_id = 1; // 1 = RSB
            ptr::write_volatile(p, pkt);
            std::sync::atomic::fence(Ordering::Release);
            write_autoreset_sync_json_mirror(&pkt);
            return pkt.trigger_seq;
        }
    }
    0
}

/// Membaca paket telemetri GPU ORION secara lock-free:
/// 1. Prioritas Utama (< 50 ns, Zero Disk I/O): Windows Named Shared Memory `Local\ORION_GPU_TELEMETRY_V1`
///    (Jika Shared Memory valid dan `is_streaming == 0`, langsung return `None` tanpa menyentuh disk!)
/// 2. Fallback Otomatis (dibatasi maks 1x tiap 250ms jika SHM belum terinisialisasi): `%TEMP%\orion_gpu_telemetry.bin`
#[inline]
pub fn read_orion_gpu_telemetry() -> Option<OrionGpuTelemetryPacket> {
    #[cfg(windows)]
    unsafe {
        let shm_ptr = get_orion_shm_ptr();
        if !shm_ptr.is_null() {
            if let Some(pkt) = read_orion_shm_consistent(shm_ptr) {
                if pkt.is_valid() {
                    return if pkt.is_fresh_and_active() { Some(pkt) } else { None };
                }
            }
        }
    }
    let now = now_epoch_ms();
    let prev_disk = LAST_ORION_DISK_FALLBACK_MS.load(Ordering::Relaxed);
    if now.saturating_sub(prev_disk) < 250 {
        return None;
    }
    LAST_ORION_DISK_FALLBACK_MS.store(now, Ordering::Relaxed);
    let bin_path = std::env::temp_dir().join(ORION_TELEMETRY_FILE_NAME);
    if let Ok(bytes) = std::fs::read(&bin_path) {
        if bytes.len() >= std::mem::size_of::<OrionGpuTelemetryPacket>() {
            let pkt: OrionGpuTelemetryPacket =
                unsafe { ptr::read_unaligned(bytes.as_ptr() as *const OrionGpuTelemetryPacket) };
            if pkt.is_fresh_and_active() {
                return Some(pkt);
            }
        }
    }
    None
}

impl SharedAsioBus {
    pub fn open_named(mapping_name: &str) -> Self {
        #[cfg(windows)]
        let (def_sr, def_smp, def_in_lat, def_out_lat) = unsafe { read_saved_asio_reg_defaults() };
        #[cfg(not(windows))]
        let (def_sr, def_smp, def_in_lat, def_out_lat) = (48000u32, 256u32, 352u32, 352u32);

        #[cfg(windows)]
        unsafe {
            let evt_name = format!("{}_Evt", mapping_name);
            let wevt = to_wide(&evt_name);
            let h_event = ffi::CreateEventW(ptr::null_mut(), 0, 0, wevt.as_ptr());

            let wname = to_wide(mapping_name);
            let byte_size = std::mem::size_of::<SharedBusMemoryLayout>() as u32;
            let h_map = ffi::CreateFileMappingW(
                ffi::INVALID_HANDLE_VALUE,
                ptr::null_mut(),
                ffi::PAGE_READWRITE,
                0,
                byte_size,
                wname.as_ptr(),
            );
            if !h_map.is_null() {
                let view = ffi::MapViewOfFile(
                    h_map,
                    ffi::FILE_MAP_ALL_ACCESS,
                    0,
                    0,
                    byte_size as usize,
                );
                if !view.is_null() {
                    if mapped_view_has_min_size(view, byte_size as usize) {
                        let layout = view as *mut SharedBusMemoryLayout;
                        // Gunakan atomic CAS sentinel agar dua pembuka pertama secara bersamaan tidak saling menimpa `write_pos`/`read_pos`!
                        if (*layout)
                            .magic
                            .compare_exchange(0, SHM_MAGIC_INITING, Ordering::AcqRel, Ordering::Acquire)
                            .is_ok()
                        {
                            (*layout).config_gen.store(1, Ordering::Relaxed);
                            (*layout).sample_rate.store(def_sr, Ordering::Relaxed);
                            (*layout).preferred_smp.store(def_smp, Ordering::Relaxed);
                            (*layout).in_lat_samples.store(def_in_lat, Ordering::Relaxed);
                            (*layout).out_lat_samples.store(def_out_lat, Ordering::Relaxed);
                            (*layout).write_pos.store(0, Ordering::Relaxed);
                            (*layout).read_pos.store(0, Ordering::Relaxed);
                            (*layout).last_write_epoch_ms.store(0, Ordering::Relaxed);
                            (*layout).vc_connected_epoch_ms.store(0, Ordering::Relaxed);
                            (*layout).last_sample_l_bits.store(0, Ordering::Relaxed);
                            (*layout).last_sample_r_bits.store(0, Ordering::Relaxed);
                            (*layout).read_frac_bits.store(0, Ordering::Relaxed);
                            (*layout).fade_in_pending.store(1, Ordering::Relaxed);
                            (*layout).measured_gpu_stall_samples.store(0, Ordering::Relaxed);
                            (*layout).adaptive_flags.store(3 | (115 << 8), Ordering::Relaxed);
                            (*layout).magic.store(SHM_MAGIC, Ordering::Release);
                        } else {
                            // Tunggu singkat jika thread/proses lain sedang menyelesaikan inisialisasi perdana
                            for _ in 0..200 {
                                if (*layout).magic.load(Ordering::Acquire) == SHM_MAGIC {
                                    break;
                                }
                                std::hint::spin_loop();
                            }
                        }
                        return Self {
                            _h_map: h_map,
                            h_event,
                            ptr: layout,
                            _fallback: None,
                        };
                    }
                    let _ = ffi::UnmapViewOfFile(view);
                }
                ffi::CloseHandle(h_map);
            }
            if !h_event.is_null() {
                ffi::CloseHandle(h_event);
            }
        }

        // Alokasi langsung di heap (alloc_zeroed) agar tidak pernah menaruh struct >524 KB di stack!
        let mut boxed: Box<SharedBusMemoryLayout> = unsafe {
            let layout = std::alloc::Layout::new::<SharedBusMemoryLayout>();
            let raw = std::alloc::alloc_zeroed(layout) as *mut SharedBusMemoryLayout;
            if raw.is_null() {
                std::alloc::handle_alloc_error(layout);
            }
            Box::from_raw(raw)
        };
        boxed.magic.store(SHM_MAGIC, Ordering::Relaxed);
        boxed.config_gen.store(1, Ordering::Relaxed);
        boxed.sample_rate.store(def_sr, Ordering::Relaxed);
        boxed.preferred_smp.store(def_smp, Ordering::Relaxed);
        boxed.in_lat_samples.store(def_in_lat, Ordering::Relaxed);
        boxed.out_lat_samples.store(def_out_lat, Ordering::Relaxed);
        boxed.adaptive_flags.store(3 | (115 << 8), Ordering::Relaxed);
        let p = &mut *boxed as *mut SharedBusMemoryLayout;
        Self {
            #[cfg(windows)]
            _h_map: ptr::null_mut(),
            #[cfg(windows)]
            h_event: ptr::null_mut(),
            ptr: p,
            _fallback: Some(boxed),
        }
    }

    #[inline]
    fn layout(&self) -> &SharedBusMemoryLayout {
        unsafe { &*self.ptr }
    }

    #[inline]
    pub fn wait_for_write(&self, timeout_ms: u32) {
        #[cfg(windows)]
        unsafe {
            if !self.h_event.is_null() {
                let _ = ffi::WaitForSingleObject(self.h_event, timeout_ms);
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(timeout_ms.max(1) as u64));
    }

    pub fn sync_config(&self, sample_rate: u32, preferred_smp: u32, in_lat: u32, out_lat: u32) {
        let l = self.layout();
        l.sample_rate.store(sample_rate.max(44100), Ordering::Relaxed);
        l.preferred_smp.store(preferred_smp.clamp(32, 2048), Ordering::Relaxed);
        l.in_lat_samples.store(in_lat, Ordering::Relaxed);
        l.out_lat_samples.store(out_lat, Ordering::Relaxed);
        l.config_gen.fetch_add(1, Ordering::Release);
    }

    pub fn get_config(&self) -> (u32, u32, u32, u32) {
        let l = self.layout();
        (
            l.sample_rate.load(Ordering::Relaxed).max(44100),
            l.preferred_smp.load(Ordering::Relaxed).clamp(32, 2048),
            l.in_lat_samples.load(Ordering::Relaxed).max(32),
            l.out_lat_samples.load(Ordering::Relaxed),
        )
    }

    #[inline]
    pub fn config_generation(&self) -> u32 {
        self.layout().config_gen.load(Ordering::Acquire)
    }

    pub fn mark_vc_heartbeat(&self, active: bool) {
        let l = self.layout();
        if active {
            l.vc_connected_epoch_ms.store(now_epoch_ms(), Ordering::Release);
        } else {
            l.vc_connected_epoch_ms.store(0, Ordering::Release);
        }
    }

    #[inline]
    pub fn is_vc_active(&self) -> bool {
        let l = self.layout();
        let hb = l.vc_connected_epoch_ms.load(Ordering::Acquire);
        let now = now_epoch_ms();
        hb > 0 && now.saturating_sub(hb) <= 600
    }

    #[inline]
    pub fn is_writer_active(&self) -> bool {
        let l = self.layout();
        let lw = l.last_write_epoch_ms.load(Ordering::Acquire);
        let now = now_epoch_ms();
        lw > 0 && now.saturating_sub(lw) <= 450
    }

    pub fn mark_writer_stopped(&self) {
        let l = self.layout();
        l.last_write_epoch_ms.store(0, Ordering::Release);
        #[cfg(windows)]
        unsafe {
            if !self.h_event.is_null() {
                let _ = ffi::SetEvent(self.h_event);
            }
        }
    }

    /// Checks and atomically clears bit 27 (`SHM_FLAG_COORD_RESET_FROM_ORION`: ORION -> RSB).
    /// Never collides with `rsb_driver_mode` (bits 28..30) and never self-consumes RSB's outgoing bit 31.
    pub fn check_and_clear_coord_reset_flag(&self) -> bool {
        let l = self.layout();
        let prev = l
            .adaptive_flags
            .fetch_and(!SHM_FLAG_COORD_RESET_FROM_ORION, Ordering::AcqRel);
        (prev & SHM_FLAG_COORD_RESET_FROM_ORION) != 0
    }

    /// Atomically sets bit 31 (`SHM_FLAG_COORD_RESET_FROM_RSB`: RSB -> ORION).
    pub fn trigger_coord_reset_flag(&self) {
        let l = self.layout();
        l.adaptive_flags
            .fetch_or(SHM_FLAG_COORD_RESET_FROM_RSB, Ordering::AcqRel);
    }

    /// Helper for tests / diagnostics: reads the raw `adaptive_flags` bitmask atomically.
    #[inline]
    pub fn raw_adaptive_flags(&self) -> u32 {
        self.layout().adaptive_flags.load(Ordering::Acquire)
    }

    /// Helper for integration tests: simulates ORION setting bit 27 (`SHM_FLAG_COORD_RESET_FROM_ORION`).
    #[inline]
    pub fn set_orion_coord_reset_flag_for_test(&self) {
        self.layout()
            .adaptive_flags
            .fetch_or(SHM_FLAG_COORD_RESET_FROM_ORION, Ordering::AcqRel);
    }

    /// Helper for integration tests: simulates ORION consuming bit 31 (`SHM_FLAG_COORD_RESET_FROM_RSB`).
    #[inline]
    pub fn clear_rsb_coord_reset_flag_for_test(&self) -> bool {
        (self
            .layout()
            .adaptive_flags
            .fetch_and(!SHM_FLAG_COORD_RESET_FROM_RSB, Ordering::AcqRel)
            & SHM_FLAG_COORD_RESET_FROM_RSB)
            != 0
    }

    /// Allocates an isolated in-memory `SharedAsioBus` (not backed by a global Windows named mapping)
    /// for deterministic unit testing of SHM flag atomics and ring operations.
    pub fn new_in_memory() -> Self {
        let mut boxed: Box<SharedBusMemoryLayout> = unsafe {
            let layout = std::alloc::Layout::new::<SharedBusMemoryLayout>();
            let raw = std::alloc::alloc_zeroed(layout) as *mut SharedBusMemoryLayout;
            if raw.is_null() {
                std::alloc::handle_alloc_error(layout);
            }
            Box::from_raw(raw)
        };
        boxed.magic.store(SHM_MAGIC, Ordering::Relaxed);
        boxed.config_gen.store(1, Ordering::Relaxed);
        boxed.sample_rate.store(48000, Ordering::Relaxed);
        boxed.preferred_smp.store(256, Ordering::Relaxed);
        boxed.in_lat_samples.store(1024, Ordering::Relaxed);
        boxed.out_lat_samples.store(1024, Ordering::Relaxed);
        boxed.adaptive_flags.store(3 | (115 << 8), Ordering::Relaxed);
        let p = &mut *boxed as *mut SharedBusMemoryLayout;
        Self {
            #[cfg(windows)]
            _h_map: ptr::null_mut(),
            #[cfg(windows)]
            h_event: ptr::null_mut(),
            ptr: p,
            _fallback: Some(boxed),
        }
    }

    #[inline]
    pub fn available_to_read(&self) -> usize {
        let l = self.layout();
        let wp = l.write_pos.load(Ordering::Acquire);
        let rp = l.read_pos.load(Ordering::Acquire);
        wp.saturating_sub(rp) as usize
    }

    pub fn reset(&self) {
        let l = self.layout();
        let wp = l.write_pos.load(Ordering::Relaxed);
        l.read_pos.store(wp, Ordering::Release);
        l.last_sample_l_bits.store(0, Ordering::Relaxed);
        l.last_sample_r_bits.store(0, Ordering::Relaxed);
        l.read_frac_bits.store(0, Ordering::Relaxed);
        l.fade_in_pending.store(1, Ordering::Relaxed);
    }

    pub fn reset_with_cushion(&self, cushion_samples: usize) {
        let l = self.layout();
        l.last_write_epoch_ms.store(0, Ordering::Release);
        let wp = l.write_pos.load(Ordering::Relaxed);
        let c = cushion_samples.min(24000);
        for i in 0..c {
            let idx = ((wp as usize) + i) & SHM_RING_MASK;
            l.left[idx].store(0, Ordering::Relaxed);
            l.right[idx].store(0, Ordering::Relaxed);
        }
        std::sync::atomic::fence(Ordering::Release);
        l.read_pos.store(wp, Ordering::Release);
        l.write_pos
            .store(wp.wrapping_add(c as u64), Ordering::Release);
        l.last_sample_l_bits.store(0, Ordering::Relaxed);
        l.last_sample_r_bits.store(0, Ordering::Relaxed);
        l.read_frac_bits.store(0, Ordering::Relaxed);
        l.fade_in_pending
            .store(if c > 0 { 2 } else { 1 }, Ordering::Relaxed);
    }

    /// Pastikan `BUS_DSP_A_OUT` selalu memiliki minimal `min_cushion` sampel tanpa pernah
    /// menghapus sampel mikrofon yang sedang mengalir saat Line A atau External VC direstart!
    pub fn ensure_min_cushion(&self, min_cushion: usize) {
        let avail = self.available_to_read();
        if !self.is_writer_active() || avail < min_cushion / 2 {
            self.reset_with_cushion(min_cushion);
        }
    }

    pub fn reset_reader_with_cushion(&self, cushion_samples: usize) {
        if self.is_writer_active() {
            let l = self.layout();
            let wp = l.write_pos.load(Ordering::Acquire);
            let c = (cushion_samples.min(24000) as u64).min(wp);
            l.read_pos.store(wp.saturating_sub(c), Ordering::Release);
            l.last_sample_l_bits.store(0, Ordering::Relaxed);
            l.last_sample_r_bits.store(0, Ordering::Relaxed);
            l.read_frac_bits.store(0, Ordering::Relaxed);
            l.fade_in_pending
                .store(if cushion_samples > 0 { 2 } else { 1 }, Ordering::Relaxed);
        } else {
            self.reset_with_cushion(cushion_samples);
        }
    }

    #[inline]
    pub fn set_adaptive_flags_with_mult(
        &self,
        adaptive_cush: bool,
        adaptive_rep: bool,
        gpu_stall_mult: f32,
    ) {
        let l = self.layout();
        let mult_pct = ((gpu_stall_mult.clamp(1.00, 1.30) * 100.0).round() as u32).clamp(100, 130);
        let mut set_bits = (mult_pct & 0xFF) << 8;
        if adaptive_cush {
            set_bits |= 1;
        }
        if adaptive_rep {
            set_bits |= 2;
        }
        // Preserve bits 2, 16, 24..26 (adaptive_mode), 27 (coord_reset_from_orion),
        // 28..30 (rsb_driver_mode), and 31 (coord_reset_from_rsb) via atomic CAS!
        let mut cur = l.adaptive_flags.load(Ordering::Acquire);
        loop {
            let next = (cur & 0xFF01_0004) | set_bits;
            match l
                .adaptive_flags
                .compare_exchange_weak(cur, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => break,
                Err(actual) => cur = actual,
            }
        }
    }

    #[inline]
    pub fn get_adaptive_mode(&self) -> u8 {
        ((self.layout().adaptive_flags.load(Ordering::Acquire) >> 24) & 0x07) as u8
    }

    #[inline]
    pub fn set_adaptive_mode(&self, mode: u8) {
        let l = self.layout();
        let m = (mode & 0x07) as u32;
        let mut cur = l.adaptive_flags.load(Ordering::Acquire);
        let prev_mode = loop {
            let pm = ((cur >> 24) & 0x07) as u8;
            let mut cleaned = cur & 0xF8FF_FFFF;
            if mode != 0 && mode != 3 && mode != 4 && mode != 5 {
                cleaned &= !4;
            }
            let next = cleaned | (m << 24);
            match l
                .adaptive_flags
                .compare_exchange_weak(cur, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => break pm,
                Err(actual) => cur = actual,
            }
        };
        if prev_mode != mode {
            get_spike_shield_atomic().store(0, Ordering::Release);
        }
    }

    #[inline]
    pub fn set_direct_py_probe_active(&self, active: bool) {
        let l = self.layout();
        if active {
            l.adaptive_flags.fetch_or(4, Ordering::Release);
        } else {
            l.adaptive_flags.fetch_and(!4, Ordering::Release);
        }
    }

    #[inline]
    pub fn is_direct_py_probe_active(&self) -> bool {
        (self.layout().adaptive_flags.load(Ordering::Acquire) & 4) != 0
    }

    #[inline]
    pub fn set_pump_caught_up(&self, caught_up: bool) {
        let l = self.layout();
        if caught_up {
            l.adaptive_flags.fetch_or(0x1_0000, Ordering::Release);
        } else {
            l.adaptive_flags.fetch_and(!0x1_0000, Ordering::Release);
        }
    }

    /// Menyimpan nilai `GPU Delay` (dalam ms) yang dibaca langsung dari `AudioCallbackProcessor.current_inference_time`
    /// atau widget `audio_section.infer_value` di dalam proses Python External VC (`python312.dll`).
    #[inline]
    pub fn set_direct_external_vc_gpu_delay_ms(&self, gpu_delay_ms: f32) {
        let l = self.layout();
        let sr = l.sample_rate.load(Ordering::Relaxed).max(44100) as f32;
        let exact_smp = ((gpu_delay_ms.clamp(0.5, 500.0) / 1000.0) * sr).round() as u32;
        l.measured_gpu_stall_samples
            .store(exact_smp.clamp(24, 24000), Ordering::Release);
    }

    /// Sinkronisasi 1:1 dengan `GPU Delay: XX ms` di aplikasi External VC:
    /// - Mendeteksi setiap callback inferensi chunk Python External VC (`>= 1.5 ms`) pada semua ukuran Buffer Size
    ///   (termasuk `512` dan `1024` sampel di mana `23 ms < blk_len * 2`).
    /// - Jika `instant_snap == true` (pada chunk ke-2/ke-3 seusai cold-start warmup `Start conversion`),
    ///   langsung mengganti angka spike warmup pertama (mis. `217 ms`) dengan `GPU Delay` aktual External VC (mis. `23 ms`).
    /// - Saat nilai `GPU Delay` External VC turun di tengah sesi, turun cepat 50% per chunk (min 5ms) sehingga
    ///   dalam 2-3 chunk (< 1 detik) angka di Rinka SoundBridge selalu identik 1:1 dengan `GPU Delay` di External VC!
    #[inline]
    pub fn update_measured_external_vc_gpu_delay(
        &self,
        stall_samples: u32,
        is_voiced_chunk: bool,
        instant_snap: bool,
    ) {
        let l = self.layout();
        let flags = l.adaptive_flags.load(Ordering::Acquire);
        let mode = ((flags >> 24) & 0x07) as u8;
        // Hanya abaikan di Mode 0 (PySync) ketika probe Python aktif;
        // Pada Mode 1 (Stopwatch+Outlier) dan Mode 2 (Decoupled P95), SELALU proses hasil pengukuran!
        if mode == 0 && (flags & 4) != 0 {
            return;
        }
        let clamped = stall_samples.min(24000);
        if clamped < 48 {
            return;
        }
        let prev = l.measured_gpu_stall_samples.load(Ordering::Acquire);
        if !is_voiced_chunk && clamped < 144 && prev > 0 {
            return;
        }
        let next = if prev == 0 || instant_snap {
            clamped
        } else if clamped <= prev {
            // Saat beban GPU turun kembali normal, langsung turun cepat (65% menuju nilai baru per chunk)
            // agar Cushion tidak pernah nyangkut di angka tinggi!
            let diff = prev - clamped;
            if diff <= 192 {
                clamped
            } else {
                prev.saturating_sub((diff * 2) / 3).max(clamped)
            }
        } else {
            // Saat naik, redam outlier 1-chunk (> 2.0x prev) agar spike 1 blok tidak langsung membuat Cushion bengkak
            if clamped > prev.saturating_mul(2) && prev >= 480 {
                prev + ((clamped - prev) / 4).min(720)
            } else {
                prev + ((clamped - prev) / 2).max(1)
            }
        };
        l.measured_gpu_stall_samples.store(next, Ordering::Release);
    }

    #[inline]
    pub fn reset_measured_gpu_stall(&self) {
        let l = self.layout();
        l.measured_gpu_stall_samples.store(0, Ordering::Release);
        let t = get_hybrid_telemetry();
        t.peak_packed.store(0, Ordering::Release);
        t.py_sensor_smp.store(0, Ordering::Release);
        t.sw_sensor_smp.store(0, Ordering::Release);
        t.hybrid_extra_smp.store(0, Ordering::Release);
        t.spike_boost_smp.store(0, Ordering::Release);
        t.last_reanchor_wp_lo.store(0, Ordering::Release);
        t.in_flight_cb_start_ms.store(0, Ordering::Release);
        t.trend_velocity_smp.store(0, Ordering::Release);
    }

    /// Peak stall (sampel) setelah memperhitungkan hold + release lambat (Spike Memory).
    #[inline]
    fn stall_peak_effective_at(&self, now_ms: u64) -> u32 {
        let l = self.layout();
        let packed = get_spike_shield_atomic().load(Ordering::Relaxed);
        let peak = (packed >> 32) as u32;
        if peak == 0 {
            return 0;
        }
        let age_ms = (now_ms as u32).wrapping_sub(packed as u32);
        if age_ms > 0x7FFF_FFFF || age_ms <= STALL_PEAK_HOLD_MS {
            return peak;
        }
        let sr = l.sample_rate.load(Ordering::Relaxed).max(44100) as u64;
        let over_ms = (age_ms - STALL_PEAK_HOLD_MS) as u64;
        let dec = over_ms * STALL_PEAK_RELEASE_MS_PER_S * sr / 1_000_000;
        (peak as u64).saturating_sub(dec) as u32
    }

    #[inline]
    pub fn get_stall_peak_samples(&self) -> u32 {
        self.stall_peak_effective_at(now_epoch_ms())
    }

    /// Spike Shield Fleksibel (Auto-Drop Saat Stabil):
    /// - Naik melindungi saat terjadi spike, namun JIKA chunk berikutnya sudah kembali stabil rendah
    ///   (`s < cur_peak * 80%`), langsung turunkan `peak` sebesar 45% selisihnya setiap chunk!
    /// - Dengan demikian, begitu GPU kembali stabil selama 2-3 chunk (~0.5 detik), `Peak` dan `Active Cushion`
    ///   langsung turun kembali mengikuti angka External VC (tidak pernah nyangkut di 182ms / 224ms!).
    #[inline]
    pub fn note_stall_peak(&self, stall_samples: u32) {
        let now = now_epoch_ms();
        let s = stall_samples.min(24000);
        if s == 0 {
            return;
        }
        let cur_peak = self.stall_peak_effective_at(now);
        let base_py = self.layout().measured_gpu_stall_samples.load(Ordering::Relaxed);
        // Batasi lonjakan 1-chunk tunggal agar tidak melebihi `max(s, base_py)` secara ekstrem
        let damped_s = if base_py > 0 && s > base_py.saturating_mul(2) {
            base_py + (s - base_py) / 2
        } else {
            s
        };
        if cur_peak == 0 || damped_s >= cur_peak {
            get_spike_shield_atomic()
                .store(((damped_s as u64) << 32) | (now as u32 as u64), Ordering::Release);
        } else if s < (cur_peak * 4) / 5 {
            // Chunk ini sudah stabil di bawah 80% peak: langsung turunkan peak secara aktif!
            let target_floor = base_py.max(s);
            let diff = cur_peak.saturating_sub(target_floor);
            let next_peak = if diff <= 240 {
                target_floor
            } else {
                cur_peak.saturating_sub((diff * 45) / 100).max(target_floor)
            };
            get_spike_shield_atomic()
                .store(((next_peak as u64) << 32) | (now as u32 as u64), Ordering::Release);
        }
    }

    /// Dipanggil pump seusai satu callback chunk External VC pada Mode 1.
    #[inline]
    pub fn pump_chunk_done(&self, cb_smp: u32, voiced: bool, chunk_seq: u64) {
        if chunk_seq < 2 {
            return;
        }
        let s = cb_smp.min(24000);
        // Abaikan spike inisialisasi awal pada chunk ke-2 dan ke-3 agar Spike Shield tidak terkunci di ~182ms saat baru Start!
        if chunk_seq >= 4 {
            self.note_stall_peak(s);
        }
        if !self.is_direct_py_probe_active() {
            let instant_snap = chunk_seq == 2 || chunk_seq == 3;
            self.update_measured_external_vc_gpu_delay(s, voiced, instant_snap);
        }
    }

    /// Mereset spike `measured_gpu_stall_samples` dan langsung memangkas antrean fisik `BUS_DSP_B_IN`
    /// kembali ke `Active Cushion` saat user menekan `RESTART APP`, `REFRESH AUDIO DEVICE`, atau `Reset ms`.
    pub fn reset_spike_and_resync_cushion(&self) {
        let l = self.layout();
        let t = get_hybrid_telemetry();
        t.peak_packed.store(0, Ordering::Release);
        t.spike_boost_smp.store(0, Ordering::Release);
        t.last_reanchor_wp_lo.store(0, Ordering::Release);
        let vc_live = self.is_vc_active() || self.is_writer_active();
        if !vc_live {
            l.measured_gpu_stall_samples.store(0, Ordering::Release);
            t.py_sensor_smp.store(0, Ordering::Release);
            t.sw_sensor_smp.store(0, Ordering::Release);
            t.hybrid_extra_smp.store(0, Ordering::Release);
        } else {
            let mode = self.get_adaptive_mode();
            if mode == 5 {
                let sr = l.sample_rate.load(Ordering::Relaxed).max(44100);
                let sw_s = t.sw_sensor_smp.load(Ordering::Acquire);
                let med = if sw_s > 0 { sw_s } else { t.py_sensor_smp.load(Ordering::Acquire) };
                self.update_mode6_orion_sw_fusion(med, med, med, 0, sr, 4);
            } else if mode == 3 || mode == 4 {
                let py_s = t.py_sensor_smp.load(Ordering::Acquire);
                let sw_s = t.sw_sensor_smp.load(Ordering::Acquire);
                if py_s > 0 || sw_s > 0 {
                    let sr = l.sample_rate.load(Ordering::Relaxed).max(44100);
                    let med = if sw_s > 0 { sw_s } else { py_s };
                    self.update_mode4_hybrid_dual_fusion(med, med, 0, sr, 4);
                }
            } else if mode != 0 || (l.adaptive_flags.load(Ordering::Acquire) & 4) == 0 {
                l.measured_gpu_stall_samples.store(0, Ordering::Release);
            }
        }
        let (c_target, _) = self.get_dsp_b_params();
        let wp = l.write_pos.load(Ordering::Acquire);
        let rp = l.read_pos.load(Ordering::Acquire);
        let avail = wp.saturating_sub(rp) as usize;
        if avail > c_target {
            let target_rp = wp.saturating_sub(c_target as u64);
            l.read_pos.fetch_max(target_rp, Ordering::AcqRel);
            l.read_frac_bits.store(0, Ordering::Relaxed);
            l.fade_in_pending.store(1, Ordering::Relaxed);
        }
    }

    #[inline]
    pub fn set_user_extra_buffer_ms(&self, extra_ms: f32) {
        let sr = self.layout().sample_rate.load(Ordering::Relaxed).max(44100) as f64;
        let extra_smp = (((extra_ms.clamp(0.0, 250.0) as f64) / 1000.0) * sr).round() as u32;
        get_hybrid_telemetry()
            .user_extra_buf_smp
            .store(extra_smp.min(12000), Ordering::Release);
    }

    #[inline]
    pub fn get_user_extra_buffer_samples(&self) -> u32 {
        get_hybrid_telemetry()
            .user_extra_buf_smp
            .load(Ordering::Relaxed)
            .min(12000)
    }

    #[inline]
    pub fn set_user_stall_guard_ms(&self, guard_ms: f32) {
        let sr = self.layout().sample_rate.load(Ordering::Relaxed).max(44100) as f64;
        let guard_smp = (((guard_ms.clamp(0.0, 150.0) as f64) / 1000.0) * sr).round() as u32;
        get_hybrid_telemetry()
            .stall_guard_smp
            .store(guard_smp.min(12000), Ordering::Release);
    }

    #[inline]
    pub fn get_user_stall_guard_samples(&self) -> u32 {
        get_hybrid_telemetry()
            .stall_guard_smp
            .load(Ordering::Relaxed)
            .min(12000)
    }

    /// Mengembalikan nilai stall/cushion efektif yang benar-benar dieksekusi (`sensor_stall * mult + stall_guard`),
    /// konsisten 1:1 dengan `get_dsp_b_params()` dan `read_stereo_dsp_b_into()` agar `IASIO::getLatencies`
    /// dan banner UI mencerminkan eksekusi DSP sebenarnya (tanpa menambahkan `adaptive_extra_ms` yang tidak aktif di ring).
    #[inline]
    pub fn get_effective_gpu_stall_samples(&self) -> u32 {
        let l = self.layout();
        let flags = l.adaptive_flags.load(Ordering::Relaxed);
        let mode = ((flags >> 24) & 0x07) as u8;
        let raw_stall = l.measured_gpu_stall_samples.load(Ordering::Relaxed).min(24000);
        let sensor_stall = if mode == 0 {
            raw_stall.max(self.get_stall_peak_samples())
        } else {
            raw_stall
        };
        if sensor_stall > 0 {
            let raw_mult = (flags >> 8) & 0xFF;
            let mult_pct = if (100..=130).contains(&raw_mult) {
                raw_mult
            } else {
                115
            };
            let guard_smp = self.get_user_stall_guard_samples();
            (((sensor_stall as u64 * mult_pct as u64) / 100) as u32)
                .saturating_add(guard_smp)
                .min(14400)
        } else {
            0
        }
    }

    /// Menjamin konfigurasi `BUS_DSP_B_IN` di Shared Memory selalu terkunci 100% pada nilai yang diinput user
    /// di `RinkaSoundBridge.exe` (tidak pernah tertimpa kembali ke 90ms/100ms saat External VC di-stop/start!).
    #[inline]
    #[allow(clippy::too_many_arguments)]
    pub fn enforce_rsb_dsp_b_config(
        &self,
        sample_rate: u32,
        preferred_smp: u32,
        cushion_samples: u32,
        adaptive_cush: bool,
        adaptive_rep: bool,
        gpu_stall_mult: f32,
        adaptive_mode: u8,
        adaptive_extra_ms: f32,
        stall_guard_ms: f32,
    ) {
        let l = self.layout();
        let sr = sample_rate.max(44100);
        let smp = preferred_smp.clamp(32, 2048);
        let cush = cushion_samples.min(24000);
        let extra_smp = (((adaptive_extra_ms.clamp(0.0, 250.0) as f64) / 1000.0) * (sr as f64)).round() as u32;
        let guard_smp = (((stall_guard_ms.clamp(0.0, 150.0) as f64) / 1000.0) * (sr as f64)).round() as u32;
        let t_hyb = get_hybrid_telemetry();
        t_hyb.user_extra_buf_smp.store(extra_smp.min(12000), Ordering::Release);
        t_hyb.stall_guard_smp.store(guard_smp.min(12000), Ordering::Release);

        let cur_flags = l.adaptive_flags.load(Ordering::Acquire);
        let prev_mode = ((cur_flags >> 24) & 0x07) as u8;
        if prev_mode != adaptive_mode {
            let t = get_hybrid_telemetry();
            t.peak_packed.store(0, Ordering::Release);
            t.spike_boost_smp.store(0, Ordering::Release);
        }
        if adaptive_mode == 5 {
            self.poll_orion_telemetry_realtime(sr);
        }
        let keep_py_bit = adaptive_mode == 0 || adaptive_mode == 3 || adaptive_mode == 4 || adaptive_mode == 5;
        let mult_pct = ((gpu_stall_mult.clamp(1.00, 1.30) * 100.0).round() as u32).clamp(100, 130);
        let mut set_bits = ((mult_pct & 0xFF) << 8) | (((adaptive_mode as u32) & 0x07) << 24);
        if adaptive_cush {
            set_bits |= 1;
        }
        if adaptive_rep {
            set_bits |= 2;
        }
        // Preserve bits 16, 27 (coord_reset_from_orion), 28..30 (rsb_driver_mode), 31 (coord_reset_from_rsb),
        // and optionally bit 2 (direct_py_probe_active), atomically via CAS!
        let preserve_mask: u32 = if keep_py_bit { 0xF801_0004 } else { 0xF801_0000 };
        let mut cur = l.adaptive_flags.load(Ordering::Acquire);
        let flags_changed = loop {
            let next = (cur & preserve_mask) | set_bits;
            match l
                .adaptive_flags
                .compare_exchange_weak(cur, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => break next != cur,
                Err(actual) => cur = actual,
            }
        };

        let cur_sr = l.sample_rate.load(Ordering::Relaxed);
        let cur_smp = l.preferred_smp.load(Ordering::Relaxed);
        let cur_cush = l.out_lat_samples.load(Ordering::Acquire);

        if cur_sr != sr || cur_smp != smp || cur_cush != cush || flags_changed {
            l.sample_rate.store(sr, Ordering::Release);
            l.preferred_smp.store(smp, Ordering::Release);
            l.in_lat_samples.store(1, Ordering::Release);
            l.out_lat_samples.store(cush, Ordering::Release);
            l.config_gen.fetch_add(1, Ordering::Release);
        }
    }

    /// Menyimpan kode DriverBackend aktif RSB (`1 = AsioDspRsb`, `2 = RsbAsio`, `3 = OrionDirect`, `4 = Other/Isolated`)
    /// di bit `28..30` pada `adaptive_flags` agar driver lain yang tidak dipilih di RSB 100% terisolasi (mute).
    #[inline]
    pub fn set_rsb_driver_mode(&self, mode: u8) {
        let l = self.layout();
        let m = (mode & 0x07) as u32;
        let mut cur = l.adaptive_flags.load(Ordering::Acquire);
        loop {
            let next = (cur & !0x7000_0000) | (m << 28);
            match l
                .adaptive_flags
                .compare_exchange_weak(cur, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => break,
                Err(actual) => cur = actual,
            }
        }
    }

    #[inline]
    pub fn get_rsb_driver_mode(&self) -> u8 {
        ((self.layout().adaptive_flags.load(Ordering::Acquire) >> 28) & 0x07) as u8
    }

    #[inline]
    pub fn get_measured_gpu_stall_samples(&self) -> u32 {
        let l = self.layout();
        let raw = l.measured_gpu_stall_samples.load(Ordering::Relaxed).min(24000);
        if raw > 0 {
            return raw;
        }
        if (self.get_adaptive_mode() == 3 || self.get_adaptive_mode() == 4 || self.get_adaptive_mode() == 5) && (self.is_vc_active() || self.is_writer_active()) {
            let t = get_hybrid_telemetry();
            let py_s = t.py_sensor_smp.load(Ordering::Relaxed);
            let sw_s = t.sw_sensor_smp.load(Ordering::Relaxed);
            if py_s >= 48 && sw_s >= 48 {
                let delta = sw_s.saturating_sub(py_s);
                return (py_s + (delta * 3) / 4).min(24000);
            }
            return py_s.max(sw_s).min(24000);
        }
        0
    }

    #[inline]
    pub fn get_measured_gpu_stall_ms(&self) -> f32 {
        let l = self.layout();
        let sr = l.sample_rate.load(Ordering::Relaxed).max(44100) as f32;
        let smp = self.get_measured_gpu_stall_samples() as f32;
        (smp / sr) * 1000.0
    }

    /// Mengambil bacaan sensor ganda `(py_ms, sw_ms, extra_ms)` khusus untuk tampilan telemetri Mode 4.
    #[inline]
    pub fn get_hybrid_dual_sensor_ms(&self) -> (f32, f32, f32) {
        if !self.is_vc_active() && !self.is_writer_active() && self.layout().measured_gpu_stall_samples.load(Ordering::Relaxed) == 0 {
            return (0.0, 0.0, 0.0);
        }
        let l = self.layout();
        let sr = l.sample_rate.load(Ordering::Relaxed).max(44100) as f32;
        let t = get_hybrid_telemetry();
        let py_ms = (t.py_sensor_smp.load(Ordering::Relaxed) as f32 / sr) * 1000.0;
        let sw_ms = (t.sw_sensor_smp.load(Ordering::Relaxed) as f32 / sr) * 1000.0;
        let ex_ms = (t.hybrid_extra_smp.load(Ordering::Relaxed) as f32 / sr) * 1000.0;
        (py_ms, sw_ms, ex_ms)
    }

    /// Proses Unik Mode 4 — **Dual-Sensor Cross-Validated Kalman/Median Fusion**:
    /// Menggabungkan data real-time dari:
    /// 1. `py_smp` (Mode 1: In-Process Python `current_inference_time`, murni waktu inferensi GPU RVC), dan
    /// 2. `sw_med` + `sw_jitter` + `cb_smp` (Mode 2: Stopwatch callback `buffer_switch` ASIO + deviasi jitter).
    ///
    /// Cara kerja fusi kedua sensor:
    /// - Mengekstrak **Overhead Nyata Non-GPU** (`delta_vst = max(0, sw_med - py_smp)`), yaitu waktu aktual yang
    ///   dihabiskan oleh Clarity Vx Mono VST3 + Pedalboard + PortAudio/GIL di luar perhitungan GPU External VC.
    /// - Menghitung **Dynamic Jitter Margin** (`jitter_guard = sw_jitter * 1.4`) yang otomatis melebar hanya saat
    ///   waktu komputasi GPU sedang berfluktuasi dan menyempit ke ~0ms saat GPU stabil.
    /// - Mendeteksi **Cross-Validated Spike** (`spike_boost`): jika `cb_smp` melonjak di atas `base_fused + 10ms`,
    ///   langsung memberikan dorongan pengaman instan yang otomatis luruh 50% setiap chunk stabil (~0.4 detik).
    #[inline]
    pub fn update_mode4_hybrid_dual_fusion(
        &self,
        cb_smp: u32,
        sw_med: u32,
        sw_jitter: u32,
        vc_sr: u32,
        chunk_seq: u64,
    ) {
        let l = self.layout();
        let t = get_hybrid_telemetry();
        t.sw_sensor_smp.store(sw_med.min(24000), Ordering::Release);

        let py_smp = t.py_sensor_smp.load(Ordering::Acquire);
        let max_vst_overhead = (vc_sr * 35) / 1000; // batas wajar overhead VST/Pedalboard = 35ms
        let max_jitter_guard = (vc_sr * 22) / 1000; // batas wajar jitter margin = 22ms

        let (core_stall, delta_vst) = if py_smp >= 48 && sw_med >= 48 {
            // Kedua sensor aktif: pisahkan komponen GPU murni (py_smp) dan komponen VST/Pedalboard (sw_med - py_smp)
            let raw_delta = sw_med.saturating_sub(py_smp).min(max_vst_overhead);
            // Gunakan 70% bobot sensor presisi Python + 30% bobot stopwatch median + overhead VST terukur
            let fused_base = py_smp + ((raw_delta * 3) / 4);
            (fused_base, raw_delta)
        } else if py_smp >= 48 {
            (py_smp + (vc_sr * 6) / 1000, (vc_sr * 6) / 1000)
        } else {
            (sw_med, 0)
        };

        let jitter_guard = ((sw_jitter * 7) / 5).min(max_jitter_guard);
        let steady_target = core_stall + jitter_guard;

        // Cross-Validated Spike Pre-empt dengan Auto-Decay cepat (50% per chunk stabil):
        let prev_boost = t.spike_boost_smp.load(Ordering::Relaxed);
        let spike_thresh = steady_target + ((vc_sr * 10) / 1000);
        let next_boost = if chunk_seq >= 3 && cb_smp > spike_thresh {
            let raw_excess = cb_smp.saturating_sub(steady_target);
            // Jika hanya 1-chunk glitch (sw_med masih rendah), ambil 60% excess; jika sw_med ikut naik, ambil 90%
            let weight_num = if sw_med > spike_thresh { 9 } else { 6 };
            ((raw_excess * weight_num) / 10)
                .max(prev_boost)
                .min((vc_sr * 90) / 1000)
        } else {
            // Begitu chunk kembali stabil (<= spike_thresh), langsung pangkas boost 55% setiap chunk!
            let decayed = (prev_boost * 45) / 100;
            if decayed < (vc_sr * 2) / 1000 { 0 } else { decayed }
        };
        t.spike_boost_smp.store(next_boost, Ordering::Release);
        t.hybrid_extra_smp
            .store(delta_vst + jitter_guard + next_boost, Ordering::Release);

        let final_hybrid_smp = (steady_target + next_boost).clamp(72, 24000);
        l.measured_gpu_stall_samples
            .store(final_hybrid_smp, Ordering::Release);
    }

    /// Mengembalikan jumlah sampel fisik yang saat ini tersedia di `BUS_DSP_B_IN` dalam milidetik (`Fill: XX ms`).
    #[inline]
    pub fn get_physical_fill_ms(&self) -> f32 {
        let l = self.layout();
        let sr = l.sample_rate.load(Ordering::Relaxed).max(44100) as f32;
        let avail = self.available_to_read().min(48000) as f32;
        (avail / sr) * 1000.0
    }

    /// Proses Unik Mode 5 (`PredictivePreemptivePlc = 4` — v2.0 Flagship):
    /// Mengatasi akar masalah "cushion telat" dengan menggabungkan:
    /// 1. **Dual-Sensor Fusion (`PySync` + `Stopwatch Median`)** untuk mendapatkan waktu GPU murni + overhead VST/GIL.
    /// 2. **Ahead-of-Time Trend-Velocity Lookahead (`trend_velocity_smp`)**: menghitung turunan pertama (percepatan)
    ///    durasi callback $\Delta v = \max(0, T_k - T_{k-1})$. Saat beban GPU mulai menanjak (mis. dari 35ms ke 48ms),
    ///    Mode 5 langsung memprediksi lonjakan berikutnya (`+1.35 * \Delta v`) 1 chunk SEBELUM lonjakan terjadi!
    /// 3. **In-Flight Mid-Callback Stall Escalation**: bekerja bersama `in_flight_cb_start_ms` di `read_stereo_dsp_b_into`
    ///    agar jika callback yang sedang berjalan melampaui prediksi, cushion dan rem servo langsung aktif di tengah
    ///    jalan tanpa menunggu `buffer_switch` selesai!
    #[inline]
    pub fn update_mode5_predictive_preemptive_plc(
        &self,
        cb_smp: u32,
        prev_cb_smp: u32,
        sw_med: u32,
        sw_jitter: u32,
        vc_sr: u32,
        chunk_seq: u64,
    ) {
        let l = self.layout();
        let t = get_hybrid_telemetry();
        t.sw_sensor_smp.store(sw_med.min(24000), Ordering::Release);

        let py_smp = t.py_sensor_smp.load(Ordering::Acquire);
        let max_vst_overhead = (vc_sr * 38) / 1000;
        let max_jitter_guard = (vc_sr * 24) / 1000;

        let (core_stall, delta_vst) = if py_smp >= 48 && sw_med >= 48 {
            let raw_delta = sw_med.saturating_sub(py_smp).min(max_vst_overhead);
            let fused_base = py_smp.max(sw_med.saturating_sub((vc_sr * 4) / 1000)) + (raw_delta / 2);
            (fused_base, raw_delta)
        } else if py_smp >= 48 {
            (py_smp + (vc_sr * 8) / 1000, (vc_sr * 8) / 1000)
        } else {
            (sw_med, 0)
        };

        // Hitung Trend-Velocity Lookahead (1st Derivative Acceleration):
        let prev_vel = t.trend_velocity_smp.load(Ordering::Relaxed);
        let raw_accel = if chunk_seq >= 3 && prev_cb_smp >= 48 && cb_smp > prev_cb_smp + ((vc_sr * 3) / 1000) {
            cb_smp - prev_cb_smp
        } else {
            0
        };
        let next_vel = if raw_accel > 0 {
            ((raw_accel * 135) / 100).max((prev_vel * 7) / 10).min((vc_sr * 55) / 1000)
        } else {
            // Luruh cepat (50% per chunk) saat beban GPU sudah datar/turun
            let dec = (prev_vel * 50) / 100;
            if dec < (vc_sr * 2) / 1000 { 0 } else { dec }
        };
        t.trend_velocity_smp.store(next_vel, Ordering::Release);

        let jitter_guard = ((sw_jitter * 8) / 5).min(max_jitter_guard);
        let steady_target = core_stall + jitter_guard + next_vel;

        // Cross-Validated Spike Pre-Empt:
        let prev_boost = t.spike_boost_smp.load(Ordering::Relaxed);
        let spike_thresh = core_stall + ((vc_sr * 8) / 1000);
        let next_boost = if chunk_seq >= 3 && cb_smp > spike_thresh {
            let raw_excess = cb_smp.saturating_sub(core_stall);
            ((raw_excess * 85) / 100)
                .max(prev_boost)
                .min((vc_sr * 95) / 1000)
        } else {
            let decayed = (prev_boost * 45) / 100;
            if decayed < (vc_sr * 2) / 1000 { 0 } else { decayed }
        };
        t.spike_boost_smp.store(next_boost, Ordering::Release);
        t.hybrid_extra_smp
            .store(delta_vst + jitter_guard + next_vel + next_boost, Ordering::Release);

        let final_pred_smp = (steady_target + next_boost).clamp(72, 24000);
        l.measured_gpu_stall_samples
            .store(final_pred_smp, Ordering::Release);
    }

    /// Polling real-time (< 18 ms sekali) untuk membaca telemetri `Local\ORION_GPU_TELEMETRY_V1` / `orion_gpu_telemetry.bin`
    /// pada Rinka SoundBridge v2.3 (NEON) saat Mode 6 (`OrionHybridFusion = 5`) aktif:
    /// - Tersinkronisasi langsung dengan detak jantung 100ms & setiap blok GPU dari ORION.
    /// - Langsung memperbarui `py_sensor_smp` dan `measured_gpu_stall_samples` bahkan di sela-sela chunk 300ms
    ///   tanpa menggandakan safety margin (`Zero Double-Counting`), dengan peredam spike 1-blok saat live toggle.
    /// - Begitu ORION di-Stop (`is_streaming == 0`) atau ditutup, langsung mereset sensor Mode 6 ke `0 ms`.
    #[inline]
    pub fn poll_orion_telemetry_realtime(&self, fallback_sr: u32) {
        let now = now_epoch_ms();
        let prev = LAST_ORION_POLL_MS.load(Ordering::Relaxed);
        if now.saturating_sub(prev) < 18 {
            return;
        }
        LAST_ORION_POLL_MS.store(now, Ordering::Relaxed);
        let l = self.layout();
        let t = get_hybrid_telemetry();
        if let Some(pkt) = read_orion_gpu_telemetry() {
            ORION_WAS_STREAMING.store(true, Ordering::Relaxed);
            let sr = l.sample_rate.load(Ordering::Relaxed).max(fallback_sr).max(44100);
            let orion_inst_ms = pkt.gpu_delay_ms.clamp(0.5, 500.0);
            let orion_ema_ms = if pkt.ema_gpu_delay_ms.is_finite() && pkt.ema_gpu_delay_ms > 0.0 {
                pkt.ema_gpu_delay_ms.clamp(0.5, 500.0)
            } else {
                orion_inst_ms
            };
            // Bug #2 hardening: damp 1-block mid-conversion setting-toggle stall spikes before blending.
            let orion_eff_ms = blend_orion_telemetry_ms(orion_inst_ms, orion_ema_ms);
            let orion_smp = (((orion_eff_ms as f64) / 1000.0) * (sr as f64)).round() as u32;
            t.py_sensor_smp.store(orion_smp.clamp(24, 24000), Ordering::Release);
            self.set_direct_py_probe_active(true);

            let sw_med = t.sw_sensor_smp.load(Ordering::Acquire);
            let max_pa_delta = (sr * 8) / 1000;
            let delta_sw = if sw_med > orion_smp {
                (sw_med - orion_smp).min(max_pa_delta) / 2
            } else {
                0
            };
            let orion_jitter_ms = if pkt.max_gpu_delay_ms.is_finite() && pkt.max_gpu_delay_ms > orion_ema_ms {
                (pkt.max_gpu_delay_ms - orion_ema_ms).clamp(0.0, 12.0) * 0.45
            } else {
                0.0
            };
            let jitter_smp = (((orion_jitter_ms as f64) / 1000.0) * (sr as f64)).round() as u32;
            let target_stall = (orion_smp + delta_sw + jitter_smp).clamp(48, 24000);
            t.hybrid_extra_smp.store(delta_sw + jitter_smp, Ordering::Release);
            l.measured_gpu_stall_samples.store(target_stall, Ordering::Release);
        } else if ORION_WAS_STREAMING.swap(false, Ordering::Relaxed) {
            // ORION baru saja mengirim sinyal Stop (`is_streaming = 0`) atau ditutup: langsung bersihkan sensor Mode 6!
            self.set_direct_py_probe_active(false);
            t.py_sensor_smp.store(0, Ordering::Release);
            t.hybrid_extra_smp.store(0, Ordering::Release);
            t.spike_boost_smp.store(0, Ordering::Release);
            t.trend_velocity_smp.store(0, Ordering::Release);
            if !self.is_vc_active() && !self.is_writer_active() {
                l.measured_gpu_stall_samples.store(0, Ordering::Release);
            }
        }
    }

    /// Proses Unik Mode 6 v2.1 (`OrionHybridFusion = 5` — Synchronized ORION IPC Telemetry + ASIO Stopwatch):
    /// Menggabungkan telemetri perangkat keras CUDA Event ORION (`Local\ORION_GPU_TELEMETRY_V1`)
    /// secara lock-free (`< 50 ns`, Zero Disk I/O di thread audio) dengan sensor `ASIO Stopwatch (SW)`:
    /// 1. **Direct ORION Hardware CUDA Telemetry (`gpu_delay_ms`, `ema_gpu_delay_ms`, `max_gpu_delay_ms`, `cushion_target_ms`)**:
    ///    Membaca waktu konversi utuh `audio_callback` ORION (`HuBERT + RMVPE/CREPE + Synthesizer + WSOLA + Enhancer + Mastering`)
    ///    dengan presisi sub-milidetik tanpa perlu menebak dari luar.
    /// 2. **Zero Double-Counting Fusion (`orion_smp + 0.5 * delta_transport + jitter_guard`)**:
    ///    Karena `gpu_delay_ms` dari ORION sudah mencakup seluruh eksekusi callback Python, selisih `sw_med - orion_smp`
    ///    dibatasi hanya pada transport CFFI/PortAudio murni (`<= 8 ms`), menghilangkan pembengkakan cushion ganda!
    /// 3. **Instant Downward Tracking**:
    ///    Begitu beban GPU ORION turun seusai warmup, cushion langsung turun mengikuti angka ORION dalam 1 chunk.
    #[inline]
    pub fn update_mode6_orion_sw_fusion(
        &self,
        cb_smp: u32,
        prev_cb_smp: u32,
        sw_med: u32,
        sw_jitter: u32,
        vc_sr: u32,
        chunk_seq: u64,
    ) {
        let l = self.layout();
        let t = get_hybrid_telemetry();
        let sr = vc_sr.max(44100);
        t.sw_sensor_smp.store(sw_med.min(24000), Ordering::Release);

        if let Some(pkt) = read_orion_gpu_telemetry_shm_only() {
            ORION_WAS_STREAMING.store(true, Ordering::Relaxed);
            let orion_inst_ms = pkt.gpu_delay_ms.clamp(0.5, 500.0);
            let orion_ema_ms = if pkt.ema_gpu_delay_ms.is_finite() && pkt.ema_gpu_delay_ms > 0.0 {
                pkt.ema_gpu_delay_ms.clamp(0.5, 500.0)
            } else {
                orion_inst_ms
            };
            let orion_eff_ms = blend_orion_telemetry_ms(orion_inst_ms, orion_ema_ms);
            let orion_smp = (((orion_eff_ms as f64) / 1000.0) * (sr as f64)).round() as u32;
            t.py_sensor_smp.store(orion_smp.clamp(24, 24000), Ordering::Release);
            self.set_direct_py_probe_active(true);

            // Ketika telemetri IPC ORION aktif, gunakan presisi sub-ms ORION sebagai jangkar utama:
            let max_pa_delta = (sr * 8) / 1000;
            let delta_sw = if sw_med > orion_smp {
                (sw_med - orion_smp).min(max_pa_delta) / 2
            } else {
                0
            };
            let orion_jitter_ms = if pkt.max_gpu_delay_ms.is_finite() && pkt.max_gpu_delay_ms > orion_ema_ms {
                (pkt.max_gpu_delay_ms - orion_ema_ms).clamp(0.0, 12.0) * 0.45
            } else {
                0.0
            };
            let orion_jitter_smp = (((orion_jitter_ms as f64) / 1000.0) * (sr as f64)).round() as u32;
            let sw_jitter_smp = (sw_jitter / 2).min((sr * 6) / 1000);
            let jitter_guard = orion_jitter_smp.max(sw_jitter_smp);

            // Hanya aktifkan spike boost jika terjadi lonjakan nyata di luar warmup chunk 1-3:
            let prev_boost = t.spike_boost_smp.load(Ordering::Relaxed);
            let spike_thresh = orion_smp + delta_sw + ((sr * 14) / 1000);
            let next_boost = if chunk_seq >= 4 && cb_smp > spike_thresh {
                let raw_excess = cb_smp.saturating_sub(orion_smp + delta_sw);
                ((raw_excess * 55) / 100)
                    .max(prev_boost)
                    .min((sr * 45) / 1000)
            } else {
                let decayed = (prev_boost * 35) / 100;
                if decayed < (sr * 2) / 1000 { 0 } else { decayed }
            };
            t.trend_velocity_smp.store(0, Ordering::Release);
            t.spike_boost_smp.store(next_boost, Ordering::Release);
            t.hybrid_extra_smp.store(delta_sw + jitter_guard + next_boost, Ordering::Release);

            let final_orion_smp = (orion_smp + delta_sw + jitter_guard + next_boost).clamp(48, 24000);
            l.measured_gpu_stall_samples.store(final_orion_smp, Ordering::Release);
            return;
        }

        // Fallback jika aplikasi Voice Changer belum menyiarkan SHM ORION:
        let orion_smp = t.py_sensor_smp.load(Ordering::Acquire);
        let max_pa_overhead = (sr * 18) / 1000;
        let max_jitter_guard = (sr * 14) / 1000;

        let (core_stall, delta_sw) = if orion_smp >= 24 && sw_med >= 24 {
            let raw_delta = sw_med.saturating_sub(orion_smp).min(max_pa_overhead);
            let fused_base = orion_smp + (raw_delta / 2);
            (fused_base, raw_delta / 2)
        } else if orion_smp >= 24 {
            (orion_smp, 0)
        } else {
            (sw_med, 0)
        };

        let prev_vel = t.trend_velocity_smp.load(Ordering::Relaxed);
        let raw_accel = if chunk_seq >= 3 && prev_cb_smp >= 48 && cb_smp > prev_cb_smp + ((sr * 4) / 1000) {
            cb_smp - prev_cb_smp
        } else {
            0
        };
        let next_vel = if raw_accel > 0 {
            raw_accel.max((prev_vel * 5) / 10).min((sr * 35) / 1000)
        } else {
            let dec = (prev_vel * 40) / 100;
            if dec < (sr * 2) / 1000 { 0 } else { dec }
        };
        t.trend_velocity_smp.store(next_vel, Ordering::Release);

        let jitter_guard = sw_jitter.min(max_jitter_guard);
        let steady_target = core_stall + jitter_guard + next_vel;

        let prev_boost = t.spike_boost_smp.load(Ordering::Relaxed);
        let spike_thresh = core_stall + ((sr * 10) / 1000);
        let next_boost = if chunk_seq >= 4 && cb_smp > spike_thresh {
            let raw_excess = cb_smp.saturating_sub(core_stall);
            ((raw_excess * 65) / 100)
                .max(prev_boost)
                .min((sr * 60) / 1000)
        } else {
            let decayed = (prev_boost * 40) / 100;
            if decayed < (sr * 2) / 1000 { 0 } else { decayed }
        };
        t.spike_boost_smp.store(next_boost, Ordering::Release);
        t.hybrid_extra_smp
            .store(delta_sw + jitter_guard + next_vel + next_boost, Ordering::Release);

        let final_orion_smp = (steady_target + next_boost).clamp(48, 24000);
        l.measured_gpu_stall_samples
            .store(final_orion_smp, Ordering::Release);
    }

    /// Diagnostik pump (dipanggil dari thread pump di dalam proses External VC).
    #[inline]
    pub fn note_pump_callback_us(&self, us: u32, is_stall: bool) {
        let l = self.layout();
        l.diag_cb_max_us.fetch_max(us, Ordering::Relaxed);
        if is_stall {
            l.diag_stall_count.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline]
    pub fn note_pump_lag_us(&self, us: u32) {
        self.layout().diag_lag_max_us.fetch_max(us, Ordering::Relaxed);
    }

    #[inline]
    pub fn note_clock_reset(&self) {
        self.layout().diag_clock_resets.fetch_add(1, Ordering::Relaxed);
    }

    /// Menghitung `(Active Cushion Samples, Micro-Stretch Enabled)`:
    /// - Pada mode **Adaptive Cushion**, menghormati `Base Cushion Floor` pilihan manual user
    ///   (mis. 60ms, 70ms, 90ms, 100ms, dst. tanpa paksaan 90ms!) dan menjaga `Active Cushion` sebesar:
    ///   `max(User Base Cushion Floor, Peak GPU Stall * Multiplier + 15 ms)`
    ///   di mana `Multiplier` dapat diatur via slider `1.01x – 1.30x` (default `1.15x`).
    #[inline]
    pub fn get_dsp_b_params(&self) -> (usize, bool) {
        let l = self.layout();
        let raw_base = (l.out_lat_samples.load(Ordering::Relaxed) as usize).min(24000);
        let _extra_smp = self.get_user_extra_buffer_samples() as usize;
        let flags = l.adaptive_flags.load(Ordering::Relaxed);
        let adaptive_cush = (flags & 1) != 0;
        let raw_mult = (flags >> 8) & 0xFF;
        let mult_pct = if (100..=130).contains(&raw_mult) {
            raw_mult as usize
        } else {
            115
        };
        let eff_cush = if adaptive_cush {
            let base_cush = raw_base;
            let mode = ((flags >> 24) & 0x07) as u8;
            let raw_stall = l.measured_gpu_stall_samples.load(Ordering::Relaxed) as usize;
            let sensor_stall = if mode == 0 {
                raw_stall.max(self.get_stall_peak_samples() as usize)
            } else {
                raw_stall
            };
            let guard_smp = self.get_user_stall_guard_samples() as usize;
            let stall_req = if sensor_stall > 0 {
                ((sensor_stall * mult_pct) / 100) + guard_smp
            } else {
                base_cush
            };
            base_cush.max(stall_req).min(14400)
        } else {
            raw_base.min(14400)
        };
        (eff_cush, true)
    }

    pub fn write_stereo(&self, left: &[f32], right: &[f32], _max_lead: usize) {
        let n = left.len().min(right.len());
        if n == 0 {
            return;
        }
        let l = self.layout();
        let wp = l.write_pos.load(Ordering::Acquire);

        for i in 0..n {
            let idx = ((wp as usize) + i) & SHM_RING_MASK;
            l.left[idx].store(left[i].to_bits(), Ordering::Relaxed);
            l.right[idx].store(right[i].to_bits(), Ordering::Relaxed);
        }
        std::sync::atomic::fence(Ordering::Release);
        l.write_pos.store(wp.wrapping_add(n as u64), Ordering::Release);
        l.last_write_epoch_ms.store(now_epoch_ms(), Ordering::Release);

        #[cfg(windows)]
        unsafe {
            if !self.h_event.is_null() {
                let _ = ffi::SetEvent(self.h_event);
            }
        }
    }

    /// Pembacaan Zero-Allocation langsung ke slice `out_l` dan `out_r` dengan Catmull-Rom Servo.
    pub fn read_stereo_into(&self, out_l: &mut [f32], out_r: &mut [f32]) {
        self.read_stereo_dsp_b_into(out_l, out_r, 4080, true);
    }

    /// Membaca `frames` sampel secara 100% Bit-Exact 1:1 dari `BUS_DSP_A_OUT` (Line A WASAPI Out -> External VC ASIO Input):
    /// - Bebas dari linear-interpolation step (`consumed = frames + 1/2`) yang sebelumnya memicu suara statis/bzzz
    ///   dan menguras `BUS_DSP_A_OUT` di sela-sela paket WASAPI 10ms (480 sampel).
    /// - `max_lag = 48.000` sampel (`1.000 ms` @ 48kHz) sehingga selama GPU External VC memproses chunk 300ms,
    ///   tidak ada satu pun sampel mikrofon yang terpotong!
    pub fn read_stereo_asio_in_into_ex(
        &self,
        out_l: &mut [f32],
        out_r: &mut [f32],
        target_cushion: usize,
        _is_catching_up: bool,
    ) {
        let frames = out_l.len().min(out_r.len());
        out_l[..frames].fill(0.0);
        out_r[..frames].fill(0.0);
        if frames == 0 {
            return;
        }
        let l = self.layout();
        let last_ms = l.last_write_epoch_ms.load(Ordering::Acquire);
        if last_ms == 0 || now_epoch_ms().saturating_sub(last_ms) > 900 {
            l.last_sample_l_bits.store(0, Ordering::Relaxed);
            l.last_sample_r_bits.store(0, Ordering::Relaxed);
            l.fade_in_pending.store(2, Ordering::Relaxed);
            return;
        }
        let wp = l.write_pos.load(Ordering::Acquire);
        let rp = l.read_pos.load(Ordering::Acquire);
        std::sync::atomic::fence(Ordering::Acquire);
        let avail = wp.saturating_sub(rp) as usize;
        let prev_l = f32::from_bits(l.last_sample_l_bits.load(Ordering::Relaxed));
        let prev_r = f32::from_bits(l.last_sample_r_bits.load(Ordering::Relaxed));
        let snap_target = target_cushion.max(frames * 2).clamp(480, 14400);

        if avail < frames {
            if avail > 0 {
                for i in 0..avail {
                    let idx = ((rp as usize) + i) & SHM_RING_MASK;
                    out_l[i] = f32::from_bits(l.left[idx].load(Ordering::Relaxed));
                    out_r[i] = f32::from_bits(l.right[idx].load(Ordering::Relaxed));
                }
                let last_l = out_l[avail - 1];
                let last_r = out_r[avail - 1];
                let rem = frames - avail;
                for i in 0..rem {
                    let fade = ((rem - i) as f32) / ((rem + 1) as f32);
                    out_l[avail + i] = last_l * fade;
                    out_r[avail + i] = last_r * fade;
                }
                l.last_sample_l_bits
                    .store(out_l[frames - 1].to_bits(), Ordering::Relaxed);
                l.last_sample_r_bits
                    .store(out_r[frames - 1].to_bits(), Ordering::Relaxed);
                l.read_pos
                    .store(rp.wrapping_add(avail as u64), Ordering::Release);
            } else {
                let fade_n = frames.min(64);
                for i in 0..fade_n {
                    let t = (i + 1) as f32 / (fade_n as f32);
                    let fade = 0.5 * (1.0 + (std::f32::consts::PI * t).cos());
                    out_l[i] = prev_l * fade;
                    out_r[i] = prev_r * fade;
                }
                l.last_sample_l_bits.store(0, Ordering::Relaxed);
                l.last_sample_r_bits.store(0, Ordering::Relaxed);
            }
            l.fade_in_pending.store(1, Ordering::Relaxed);
            l.diag_underruns.fetch_add(1, Ordering::Relaxed);
            return;
        }

        let mut jumped = false;
        let max_in_lag = (snap_target + 36000).max(38400);
        let eff_rp = if avail > max_in_lag {
            jumped = true;
            l.diag_jumps.fetch_add(1, Ordering::Relaxed);
            wp.saturating_sub(snap_target as u64)
        } else {
            rp
        };

        for i in 0..frames {
            let idx = ((eff_rp as usize) + i) & SHM_RING_MASK;
            out_l[i] = f32::from_bits(l.left[idx].load(Ordering::Relaxed));
            out_r[i] = f32::from_bits(l.right[idx].load(Ordering::Relaxed));
        }

        let need_fade = l.fade_in_pending.swap(0, Ordering::Relaxed) != 0 || jumped;
        if need_fade {
            let fade_n = frames.min(64);
            for i in 0..fade_n {
                let t = (i + 1) as f32 / (fade_n as f32);
                let w = 0.5 * (1.0 - (std::f32::consts::PI * t).cos());
                out_l[i] = prev_l * (1.0 - w) + out_l[i] * w;
                out_r[i] = prev_r * (1.0 - w) + out_r[i] * w;
            }
        }
        l.last_sample_l_bits
            .store(out_l[frames - 1].to_bits(), Ordering::Relaxed);
        l.last_sample_r_bits
            .store(out_r[frames - 1].to_bits(), Ordering::Relaxed);
        l.read_pos
            .store(eff_rp.wrapping_add(frames as u64), Ordering::Release);
    }

    /// Menulis hasil konversi VC dari `"Rinka SoundBridge ASIO"` ke `BUS_DSP_B_IN` secara SPSC Murni:
    /// Thread Writer HANYA memodifikasi `write_pos` (tidak pernah menyentuh `read_pos`!) sehingga bebas race condition!
    pub fn write_stereo_dsp_b(&self, left: &[f32], right: &[f32], _cushion_target: usize) {
        let n = left.len().min(right.len());
        if n == 0 {
            return;
        }
        let l = self.layout();
        let wp = l.write_pos.load(Ordering::Acquire);
        for i in 0..n {
            let idx = ((wp as usize) + i) & SHM_RING_MASK;
            l.left[idx].store(left[i].to_bits(), Ordering::Relaxed);
            l.right[idx].store(right[i].to_bits(), Ordering::Relaxed);
        }
        std::sync::atomic::fence(Ordering::Release);
        l.write_pos
            .store(wp.wrapping_add(n as u64), Ordering::Release);
        l.last_write_epoch_ms
            .store(now_epoch_ms(), Ordering::Release);

        #[cfg(windows)]
        unsafe {
            if !self.h_event.is_null() {
                let _ = ffi::SetEvent(self.h_event);
            }
        }
    }

    /// Membaca dari `BUS_DSP_B_IN` di Line B (`ASIO DSP RSB`) secara Zero-Allocation & SPSC Murni:
    /// - Menggunakan **100% Bit-Exact 1:1 Copy** selama `cur_avail >= frames` (tanpa pengereman paksa `pre_brake_reserve`
    ///   di tengah callback GPU dan tanpa servo pitch-shifting Catmull-Rom pada suara vokal aktif) sehingga pitch dan
    ///   fase vokal 100% murni identik dengan sumbernya (bebas distorsi warble/FM setiap 200ms).
    /// - Dilengkapi **True-Silence Cushion Builder & Excess Trim** pada ambang keheningan murni (`< 0.0012` = -58.4 dBFS)
    ///   dengan S-curve crossfade penuh (tidak pernah memotong lembah konsonan/suku kata aktif).
    /// - Dilengkapi **Zero-Echo Pitch-Synchronous Formant PLC** hanya saat benar-benar kehabisan sampel (`avail < frames`)
    ///   tanpa memundurkan `read_pos` ke sampel lama yang sudah diputar.
    pub fn read_stereo_dsp_b_into(
        &self,
        out_l: &mut [f32],
        out_r: &mut [f32],
        cushion_target: usize,
        _micro_stretch: bool,
    ) {
        let frames = out_l.len().min(out_r.len());
        out_l[..frames].fill(0.0);
        out_r[..frames].fill(0.0);
        if frames == 0 {
            return;
        }
        let l = self.layout();
        let last_ms = l.last_write_epoch_ms.load(Ordering::Acquire);
        let idle_ms = if last_ms == 0 {
            u64::MAX
        } else {
            now_epoch_ms().saturating_sub(last_ms)
        };
        if idle_ms > 1500 {
            l.last_sample_l_bits.store(0, Ordering::Relaxed);
            l.last_sample_r_bits.store(0, Ordering::Relaxed);
            l.read_frac_bits.store(0, Ordering::Relaxed);
            l.fade_in_pending.store(2, Ordering::Relaxed);
            return;
        }

        let wp = l.write_pos.load(Ordering::Acquire);
        let rp = l.read_pos.load(Ordering::Acquire);
        std::sync::atomic::fence(Ordering::Acquire);
        let avail = wp.saturating_sub(rp) as usize;
        let t_shm = get_hybrid_telemetry();
        t_shm.last_avail_smp.store(avail.min(48000) as u32, Ordering::Relaxed);

        let prev_l = f32::from_bits(l.last_sample_l_bits.load(Ordering::Relaxed));
        let prev_r = f32::from_bits(l.last_sample_r_bits.load(Ordering::Relaxed));
        let sr = l.sample_rate.load(Ordering::Relaxed).max(44100) as usize;
        let flags = l.adaptive_flags.load(Ordering::Relaxed);
        let adaptive_mode = ((flags >> 24) & 0x07) as u8;
        let raw_measured_stall = l.measured_gpu_stall_samples.load(Ordering::Relaxed) as usize;
        let _extra_smp = self.get_user_extra_buffer_samples() as usize;

        // IN-FLIGHT MID-CALLBACK STALL ESCALATION:
        // Pantau apakah thread ASIO pump saat ini sedang tertahan di dalam `buffer_switch` GPU External VC.
        // Jika durasi callback yang sedang berjalan (`in_flight_smp`) melampaui `raw_measured_stall`,
        // naikkan `sensor_stall` agar target cushion (`c_target`) langsung menyesuaikan tanpa mengubah pitch suara!
        let in_flight_start = t_shm.in_flight_cb_start_ms.load(Ordering::Acquire);
        let in_flight_ms = if in_flight_start > 0 {
            now_epoch_ms().saturating_sub(in_flight_start).min(600)
        } else {
            0
        };
        let in_flight_smp = (((in_flight_ms as usize) * sr) / 1000).min(24000);
        if in_flight_smp > raw_measured_stall + ((sr * 6) / 1000) && (adaptive_mode == 4 || adaptive_mode == 3) {
            l.measured_gpu_stall_samples
                .store(in_flight_smp as u32, Ordering::Release);
        }

        let base_sensor_stall = if adaptive_mode == 0 {
            raw_measured_stall.max(self.get_stall_peak_samples() as usize)
        } else {
            raw_measured_stall
        };
        let sensor_stall = if adaptive_mode == 5 || adaptive_mode == 4 || adaptive_mode == 3 {
            base_sensor_stall.max(in_flight_smp)
        } else {
            base_sensor_stall
        };
        let measured_stall = sensor_stall;
        let adaptive_cush = (flags & 1) != 0;
        let pump_caught_up = (flags & 0x1_0000) != 0 && in_flight_ms < 4;
        // Ambang keheningan murni (-58.4 dBFS) agar pengisian/pemangkasan cushion HANYA terjadi
        // pada jeda hening antar kata/kalimat dan TIDAK PERNAH memotong konsonan atau vokal lembut!
        let silence_thr: f32 = 0.0012;
        let raw_mult = (flags >> 8) & 0xFF;
        let mult_pct = if (100..=130).contains(&raw_mult) {
            raw_mult as usize
        } else {
            115
        };
        let c_target = if adaptive_cush {
            let base_cush = cushion_target;
            let guard_smp = self.get_user_stall_guard_samples() as usize;
            let stall_req = if measured_stall > 0 {
                ((measured_stall * mult_pct) / 100) + guard_smp
            } else {
                base_cush
            };
            base_cush.max(stall_req).min(14400)
        } else {
            cushion_target.min(14400)
        };
        let fade_state = l.fade_in_pending.load(Ordering::Relaxed);

        if fade_state == 2 {
            let reprime_target = c_target
                .min((sr * 35) / 1000)
                .max(frames * 2);
            if avail < reprime_target {
                let fade_n = frames.min(64);
                for i in 0..fade_n {
                    let t = (i + 1) as f32 / (fade_n as f32);
                    let fade = 0.5 * (1.0 + (std::f32::consts::PI * t).cos());
                    out_l[i] = prev_l * fade;
                    out_r[i] = prev_r * fade;
                }
                l.last_sample_l_bits.store(0, Ordering::Relaxed);
                l.last_sample_r_bits.store(0, Ordering::Relaxed);
                l.diag_silent_frames.fetch_add(frames as u64, Ordering::Relaxed);
                return;
            }
            l.fade_in_pending.store(1, Ordering::Relaxed);
        }

        let deep_silence_thr: f32 = 0.0006; // -64.4 dBFS
        let min_pause_win: usize = (sr * 80) / 1000; // minimal 80 ms jeda hening antar-kalimat (bukan konsonan /t/ 15ms!)

        // 1. True-Silence Cushion Builder (Hanya di Jeda Antar-Kalimat >= 80 ms & Selalu Maju):
        // Check `rsb_driver_mode == 3` (OrionDirect) instead of `vc_connected_epoch_ms` so True-Silence
        // Cushion Builder & Formant PLC stay active for `AsioDspRsb` and `RsbAsio`!
        let is_orion_direct = self.get_rsb_driver_mode() == 3;
        let deficit = c_target.saturating_sub(avail);
        if !is_orion_direct && deficit >= 96 && prev_l.abs() < deep_silence_thr && prev_r.abs() < deep_silence_thr && avail >= frames + min_pause_win {
            let mut is_true_pause = true;
            for i in (0..min_pause_win).step_by(4) {
                let idx = ((rp as usize) + i) & SHM_RING_MASK;
                let sl = f32::from_bits(l.left[idx].load(Ordering::Relaxed));
                let sr_v = f32::from_bits(l.right[idx].load(Ordering::Relaxed));
                if sl.abs() > deep_silence_thr || sr_v.abs() > deep_silence_thr {
                    is_true_pause = false;
                    break;
                }
            }
            if is_true_pause {
                // Maksimal tambahkan 25% dari 1 blok per callback agar `rp` SELALU bergerak maju!
                let refill_hold = deficit.min(frames / 4).max(1);
                let advance = frames.saturating_sub(refill_hold).max(1);
                for i in 0..advance {
                    let idx = ((rp as usize) + i) & SHM_RING_MASK;
                    out_l[i] = f32::from_bits(l.left[idx].load(Ordering::Relaxed));
                    out_r[i] = f32::from_bits(l.right[idx].load(Ordering::Relaxed));
                }
                let last_l = out_l[advance - 1];
                let last_r = out_r[advance - 1];
                let next_idx = ((rp as usize) + advance) & SHM_RING_MASK;
                let next_l = f32::from_bits(l.left[next_idx].load(Ordering::Relaxed));
                let next_r = f32::from_bits(l.right[next_idx].load(Ordering::Relaxed));
                let hold_len = (frames - advance).max(1);
                for i in advance..frames {
                    let t = (i - advance + 1) as f32 / (hold_len as f32);
                    let w = 0.5 * (1.0 - (std::f32::consts::PI * t).cos());
                    out_l[i] = last_l * (1.0 - w) + next_l * w;
                    out_r[i] = last_r * (1.0 - w) + next_r * w;
                }
                l.read_pos
                    .store(rp.wrapping_add(advance as u64), Ordering::Release);
                l.last_sample_l_bits
                    .store(out_l[frames - 1].to_bits(), Ordering::Relaxed);
                l.last_sample_r_bits
                    .store(out_r[frames - 1].to_bits(), Ordering::Relaxed);
                l.read_frac_bits.store(0, Ordering::Relaxed);
                return;
            }
        }

        // 2. True-Silence Post-Catchup Excess Trim (100% Immune to Sustained-Vowel Zero-Crossings!):
        let direct_orion_ram = is_orion_direct;
        let burst_allowance = if direct_orion_ram {
            ((sr * 22) / 1000).max(frames * 2)
        } else if pump_caught_up {
            ((sr * 18) / 1000).max(frames * 2)
        } else {
            ((sr * 40) / 1000).max(frames * 3)
        };
        let jitter_tol = ((sr * 16) / 1000).max(frames * 2);
        let safe_target = (c_target + burst_allowance).min(14400);
        let excess = avail.saturating_sub(safe_target);
        let quiet_valley_thr: f32 = 0.0015; // -56.5 dBFS: handles RVC idle noise floor during True-Silence Excess Trim
        let mut cur_win_quiet = prev_l.abs() < quiet_valley_thr && prev_r.abs() < quiet_valley_thr;
        if cur_win_quiet {
            let chk_cur = frames.min(96);
            for i in (0..chk_cur).step_by(2) {
                let idx = ((rp as usize) + i) & SHM_RING_MASK;
                let sl = f32::from_bits(l.left[idx].load(Ordering::Relaxed));
                let sr_v = f32::from_bits(l.right[idx].load(Ordering::Relaxed));
                if sl.abs() > quiet_valley_thr || sr_v.abs() > quiet_valley_thr {
                    cur_win_quiet = false;
                    break;
                }
            }
        }
        if (pump_caught_up || direct_orion_ram)
            && excess >= jitter_tol
            && cur_win_quiet
            && avail >= frames + excess
        {
            let new_rp_cand = wp.saturating_sub((c_target + frames) as u64);
            if new_rp_cand > rp {
                let check_landing = (frames * 2).clamp(480, 960);
                let mut is_quiet_window = true;
                for i in (0..check_landing).step_by(2) {
                    let idx = ((new_rp_cand as usize) + i) & SHM_RING_MASK;
                    let sl = f32::from_bits(l.left[idx].load(Ordering::Relaxed));
                    let sr_v = f32::from_bits(l.right[idx].load(Ordering::Relaxed));
                    if sl.abs() > quiet_valley_thr || sr_v.abs() > quiet_valley_thr {
                        is_quiet_window = false;
                        break;
                    }
                }
                if is_quiet_window {
                    let new_rp = new_rp_cand;
                    for i in 0..frames {
                        let idx = ((new_rp as usize) + i) & SHM_RING_MASK;
                        out_l[i] = f32::from_bits(l.left[idx].load(Ordering::Relaxed));
                        out_r[i] = f32::from_bits(l.right[idx].load(Ordering::Relaxed));
                    }
                    let fade_n = frames.min(64);
                    for i in 0..fade_n {
                        let t = (i + 1) as f32 / (fade_n as f32);
                        let w = 0.5 * (1.0 - (std::f32::consts::PI * t).cos());
                        out_l[i] = prev_l * (1.0 - w) + out_l[i] * w;
                        out_r[i] = prev_r * (1.0 - w) + out_r[i] * w;
                    }
                    l.read_pos
                        .store(new_rp.wrapping_add(frames as u64), Ordering::Release);
                    l.last_sample_l_bits
                        .store(out_l[frames - 1].to_bits(), Ordering::Relaxed);
                    l.last_sample_r_bits
                        .store(out_r[frames - 1].to_bits(), Ordering::Relaxed);
                    l.read_frac_bits.store(0, Ordering::Relaxed);
                    return;
                }
            }
        }

        // 3. Penanganan Underrun Aktual (`avail < frames`):
        if avail < frames {
            for i in 0..avail {
                let idx = ((rp as usize) + i) & SHM_RING_MASK;
                out_l[i] = f32::from_bits(l.left[idx].load(Ordering::Relaxed));
                out_r[i] = f32::from_bits(l.right[idx].load(Ordering::Relaxed));
            }
            if avail > 0 {
                l.read_pos.store(wp, Ordering::Release);
            }
            let rem = frames - avail;
            let anchor_l = if avail > 0 { out_l[avail - 1] } else { prev_l };
            let anchor_r = if avail > 0 { out_r[avail - 1] } else { prev_r };

            // Cek energi RMS 96 sampel terakhir sebelum `wp` agar zero-crossing tidak pernah disangka hening
            let mut tail_energy = 0.0f32;
            if wp >= 96 {
                let start_e = wp.saturating_sub(96) as usize;
                for k in 0..96 {
                    let s_l = f32::from_bits(l.left[(start_e + k) & SHM_RING_MASK].load(Ordering::Relaxed));
                    let s_r = f32::from_bits(l.right[(start_e + k) & SHM_RING_MASK].load(Ordering::Relaxed));
                    tail_energy += s_l * s_l + s_r * s_r;
                }
                tail_energy = (tail_energy / 192.0).sqrt();
            }

            let prev_plc_cnt = if avail > 0 {
                0u32
            } else {
                l.read_frac_bits.load(Ordering::Relaxed)
            };

            if !direct_orion_ram && adaptive_cush && idle_ms < 450 && wp >= 640 && tail_energy >= silence_thr && prev_plc_cnt < (sr as u32 / 50) {
                let mut best_period: usize = (sr / 200).clamp(160, 260);
                let mut best_err: f32 = f32::MAX;
                let min_p = (sr / 500).max(88);
                let max_p = (sr / 115).min(420);
                for cand_p in (min_p..=max_p).step_by(4) {
                    let p1 = wp.saturating_sub(cand_p as u64) as usize;
                    let p0 = wp.saturating_sub((cand_p * 2) as u64) as usize;
                    let mut err = 0.0f32;
                    for k in (0..32).step_by(2) {
                        let a = f32::from_bits(l.left[(p1 + k) & SHM_RING_MASK].load(Ordering::Relaxed));
                        let b = f32::from_bits(l.left[(p0 + k) & SHM_RING_MASK].load(Ordering::Relaxed));
                        err += (a - b).abs();
                    }
                    if err < best_err {
                        best_err = err;
                        best_period = cand_p;
                    }
                }
                let plc_start = wp.saturating_sub(best_period as u64) as usize;
                let phase_base = (prev_plc_cnt as usize) % best_period;
                for i in 0..rem {
                    let total_plc_idx = (prev_plc_cnt as usize) + i;
                    let env_decay = (1.0 - (total_plc_idx as f32) / ((sr as f32) * 0.045)).clamp(0.0, 0.94);
                    let offset = (phase_base + i) % best_period;
                    let idx = (plc_start + offset) & SHM_RING_MASK;
                    out_l[avail + i] = f32::from_bits(l.left[idx].load(Ordering::Relaxed)) * env_decay;
                    out_r[avail + i] = f32::from_bits(l.right[idx].load(Ordering::Relaxed)) * env_decay;
                }
                let fade_n = rem.min(48);
                for i in 0..fade_n {
                    let t = (i + 1) as f32 / (fade_n as f32);
                    let w = 0.5 * (1.0 - (std::f32::consts::PI * t).cos());
                    out_l[avail + i] = anchor_l * (1.0 - w) + out_l[avail + i] * w;
                    out_r[avail + i] = anchor_r * (1.0 - w) + out_r[avail + i] * w;
                }
                l.last_sample_l_bits
                    .store(out_l[frames - 1].to_bits(), Ordering::Relaxed);
                l.last_sample_r_bits
                    .store(out_r[frames - 1].to_bits(), Ordering::Relaxed);
                l.read_frac_bits
                    .store(prev_plc_cnt.saturating_add(rem as u32), Ordering::Relaxed);
                l.fade_in_pending.store(1, Ordering::Relaxed);
                l.diag_underruns.fetch_add(1, Ordering::Relaxed);
                return;
            }

            let fade_n = rem.min(96);
            for i in 0..fade_n {
                let t = (i + 1) as f32 / (fade_n as f32);
                let fade = 0.5 * (1.0 + (std::f32::consts::PI * t).cos());
                out_l[avail + i] = anchor_l * fade;
                out_r[avail + i] = anchor_r * fade;
            }
            l.last_sample_l_bits.store(0, Ordering::Relaxed);
            l.last_sample_r_bits.store(0, Ordering::Relaxed);
            l.read_frac_bits
                .store(prev_plc_cnt.saturating_add(rem as u32), Ordering::Relaxed);
            l.fade_in_pending.store(1, Ordering::Relaxed);
            l.diag_underruns.fetch_add(1, Ordering::Relaxed);
            l.diag_silent_frames.fetch_add(rem as u64, Ordering::Relaxed);
            return;
        }

        // 4. Pembacaan Normal 100% Bit-Exact 1:1 (`avail >= frames`):
        let stall_allowance = measured_stall.max((sr * 40) / 1000);
        // Bug #2 hardening: tighten hard max_lag ceiling (+140ms, clamped 9600..16800) so a
        // mid-conversion toggle burst can never leave 350-600ms stale lag stuck in BUS_DSP_B_IN
        // (protected by 128-sample cosine crossfade on `jumped_lag` below).
        let max_lag = (c_target + stall_allowance + ((sr * 140) / 1000)).clamp(9600, 16800);
        let mut jumped_lag = false;
        let eff_rp = if avail > max_lag {
            jumped_lag = true;
            l.diag_jumps.fetch_add(1, Ordering::Relaxed);
            wp.saturating_sub((c_target + frames) as u64)
        } else {
            rp
        };

        for i in 0..frames {
            let idx = ((eff_rp as usize) + i) & SHM_RING_MASK;
            out_l[i] = f32::from_bits(l.left[idx].load(Ordering::Relaxed));
            out_r[i] = f32::from_bits(l.right[idx].load(Ordering::Relaxed));
        }
        l.read_pos
            .store(eff_rp.wrapping_add(frames as u64), Ordering::Release);
        l.read_frac_bits.store(0, Ordering::Relaxed);

        let need_fade_in = l.fade_in_pending.swap(0, Ordering::Relaxed) != 0 || jumped_lag;
        if need_fade_in {
            let fade_n = frames.min(128);
            for i in 0..fade_n {
                let t = (i + 1) as f32 / (fade_n as f32);
                let w = 0.5 * (1.0 - (std::f32::consts::PI * t).cos());
                out_l[i] = prev_l * (1.0 - w) + out_l[i] * w;
                out_r[i] = prev_r * (1.0 - w) + out_r[i] * w;
            }
        }

        l.last_sample_l_bits
            .store(out_l[frames - 1].to_bits(), Ordering::Relaxed);
        l.last_sample_r_bits
            .store(out_r[frames - 1].to_bits(), Ordering::Relaxed);
    }

    /// Pembacaan **Pure 1:1 Built-in FlexASIO (`"RSB ASIO"`)** bebas stutter saat Clear / Noise Suppression aktif:
    /// - Bekerja 100% murni seperti FIFO PortAudio/WASAPI di dalam FlexASIO asli:
    ///   - Menghormati penuh nilai `steady_target` (termasuk `Set Input Latency` & `Set Output Latency` hingga 1000ms)
    ///     ditambah proteksi adaptif otomatis terhadap durasi callback `Clear + RVC`.
    ///   - TIDAK PERNAH memangkas sampel (`trim_n`) saat terjadi celah hening `0.0000` akibat Noise Suppression (Clear),
    ///     sehingga cadangan sampel untuk chunk berikutnya tidak pernah terhapus!
    ///   - TIDAK PERNAH mengunci output ke `fade_in_pending = 2` di tengah kalimat.
    pub fn read_stereo_rsb_flex_b_into(
        &self,
        out_l: &mut [f32],
        out_r: &mut [f32],
        steady_target: usize,
    ) {
        let base_floor = if steady_target > 0 {
            steady_target.clamp(1440, 24000)
        } else {
            2400
        };
        self.read_stereo_dsp_b_into(out_l, out_r, base_floor, true);
    }
}

static BUS_RSB_STANDALONE_A_OUT: OnceLock<SharedAsioBus> = OnceLock::new();
static BUS_RSB_STANDALONE_B_IN: OnceLock<SharedAsioBus> = OnceLock::new();

#[inline]
pub fn get_rsb_standalone_bus_a_out() -> &'static SharedAsioBus {
    BUS_RSB_STANDALONE_A_OUT.get_or_init(|| SharedAsioBus::open_named("Local\\RinkaAsioBus_RSB_Standalone_A_v3"))
}

#[inline]
pub fn get_rsb_standalone_bus_b_in() -> &'static SharedAsioBus {
    BUS_RSB_STANDALONE_B_IN.get_or_init(|| SharedAsioBus::open_named("Local\\RinkaAsioBus_RSB_Standalone_B_v3"))
}

static BUS_LINE_A_OUT: OnceLock<SharedAsioBus> = OnceLock::new();
static BUS_LINE_B_IN: OnceLock<SharedAsioBus> = OnceLock::new();
static BUS_DSP_A_OUT: OnceLock<SharedAsioBus> = OnceLock::new();
static BUS_DSP_B_IN: OnceLock<SharedAsioBus> = OnceLock::new();

#[repr(C)]
pub struct HybridSensorTelemetryLayout {
    pub peak_packed: AtomicU64,
    pub py_sensor_smp: AtomicU32,
    pub sw_sensor_smp: AtomicU32,
    pub hybrid_extra_smp: AtomicU32,
    pub spike_boost_smp: AtomicU32,
    pub user_extra_buf_smp: AtomicU32,
    pub last_reanchor_wp_lo: AtomicU32,
    pub in_flight_cb_start_ms: AtomicU64,
    pub trend_velocity_smp: AtomicU32,
    pub last_avail_smp: AtomicU32,
    pub stall_guard_smp: AtomicU32,
    pub _pad_guard: AtomicU32,
}

struct SpikeShieldShm {
    ptr: *const HybridSensorTelemetryLayout,
    _fallback: Option<Box<HybridSensorTelemetryLayout>>,
}
unsafe impl Send for SpikeShieldShm {}
unsafe impl Sync for SpikeShieldShm {}

static SPIKE_SHIELD_SHM: OnceLock<SpikeShieldShm> = OnceLock::new();

#[inline]
pub fn get_hybrid_telemetry() -> &'static HybridSensorTelemetryLayout {
    let s = SPIKE_SHIELD_SHM.get_or_init(|| {
        #[cfg(windows)]
        unsafe {
            let wname = to_wide("Local\\RinkaAsioSpikeShield_v4");
            let h_map = ffi::CreateFileMappingW(
                ffi::INVALID_HANDLE_VALUE,
                ptr::null_mut(),
                ffi::PAGE_READWRITE,
                0,
                64,
                wname.as_ptr(),
            );
            if !h_map.is_null() {
                let view = ffi::MapViewOfFile(h_map, ffi::FILE_MAP_ALL_ACCESS, 0, 0, 64);
                if !view.is_null() {
                    return SpikeShieldShm {
                        ptr: view as *const HybridSensorTelemetryLayout,
                        _fallback: None,
                    };
                }
                ffi::CloseHandle(h_map);
            }
        }
        let b = Box::new(HybridSensorTelemetryLayout {
            peak_packed: AtomicU64::new(0),
            py_sensor_smp: AtomicU32::new(0),
            sw_sensor_smp: AtomicU32::new(0),
            hybrid_extra_smp: AtomicU32::new(0),
            spike_boost_smp: AtomicU32::new(0),
            user_extra_buf_smp: AtomicU32::new(1200),
            last_reanchor_wp_lo: AtomicU32::new(0),
            in_flight_cb_start_ms: AtomicU64::new(0),
            trend_velocity_smp: AtomicU32::new(0),
            last_avail_smp: AtomicU32::new(0),
            stall_guard_smp: AtomicU32::new(720),
            _pad_guard: AtomicU32::new(0),
        });
        let p = &*b as *const HybridSensorTelemetryLayout;
        SpikeShieldShm {
            ptr: p,
            _fallback: Some(b),
        }
    });
    unsafe { &*s.ptr }
}

#[inline]
fn get_spike_shield_atomic() -> &'static AtomicU64 {
    &get_hybrid_telemetry().peak_packed
}

#[inline]
pub fn get_asio_bus_line_a_out() -> &'static SharedAsioBus {
    BUS_LINE_A_OUT.get_or_init(|| SharedAsioBus::open_named("Local\\RinkaAsioBus_ID_LineA_Out_v3"))
}

#[inline]
pub fn get_asio_bus_line_b_in() -> &'static SharedAsioBus {
    BUS_LINE_B_IN.get_or_init(|| SharedAsioBus::open_named("Local\\RinkaAsioBus_ID_LineB_In_v3"))
}

#[inline]
pub fn get_dsp_rsb_bus_a_out() -> &'static SharedAsioBus {
    BUS_DSP_A_OUT.get_or_init(|| SharedAsioBus::open_named("Local\\RinkaAsioBus_DSP_RSB_A_Out_v3"))
}

#[inline]
pub fn get_dsp_rsb_bus_b_in() -> &'static SharedAsioBus {
    BUS_DSP_B_IN.get_or_init(|| SharedAsioBus::open_named("Local\\RinkaAsioBus_DSP_RSB_B_In_v3"))
}

static DIAG_LAST_POLL: Mutex<Option<Instant>> = Mutex::new(None);

/// Diagnostik ASIO bridge: dipanggil dari `periodic_background_tasks` (tiap 2 dtk); menulis 1 baris log
/// tiap >= 10 dtk, hanya saat pump External VC aktif. Cara baca baris log:
///   gpu_max   = durasi `buffer_switch` terlama (= GPU delay External VC). > ~300ms = External VC overload.
///   chunks    = jumlah chunk yang diproses External VC pada jendela ini (normal ~ 3.3/dtk untuk blok 300ms).
///   lag_max   = pump tertinggal dari jadwal (normal: naik saat stall lalu turun ke 0).
///   resets    = pump menyerah mengejar jadwal (harus 0; > 0 = audio terbuang).
///   B_under/B_jump/B_silent = dropout/lompatan/frame senyap di Line B; A_* idem untuk jalur mic.
pub fn asio_diag_poll() {
    {
        let mut g = DIAG_LAST_POLL.lock();
        let now = Instant::now();
        match *g {
            Some(t) if now.duration_since(t) < Duration::from_secs(10) => return,
            _ => *g = Some(now),
        }
    }
    let bus_a = get_dsp_rsb_bus_a_out();
    let bus_b = get_dsp_rsb_bus_b_in();
    if !bus_b.is_writer_active() {
        // External VC tidak aktif: buang counter lama supaya jendela berikutnya bersih.
        let lb = bus_b.layout();
        lb.diag_cb_max_us.store(0, Ordering::Relaxed);
        lb.diag_stall_count.store(0, Ordering::Relaxed);
        lb.diag_lag_max_us.store(0, Ordering::Relaxed);
        return;
    }
    let la = bus_a.layout();
    let lb = bus_b.layout();
    let gpu_max_ms = lb.diag_cb_max_us.swap(0, Ordering::Relaxed) as f32 / 1000.0;
    let chunks = lb.diag_stall_count.swap(0, Ordering::Relaxed);
    let lag_max_ms = lb.diag_lag_max_us.swap(0, Ordering::Relaxed) as f32 / 1000.0;
    let resets = lb.diag_clock_resets.swap(0, Ordering::Relaxed);
    let b_under = lb.diag_underruns.swap(0, Ordering::Relaxed);
    let b_jump = lb.diag_jumps.swap(0, Ordering::Relaxed);
    let b_silent_ms = lb.diag_silent_frames.swap(0, Ordering::Relaxed) as f64
        / lb.sample_rate.load(Ordering::Relaxed).max(44100) as f64
        * 1000.0;
    let a_under = la.diag_underruns.swap(0, Ordering::Relaxed);
    let a_jump = la.diag_jumps.swap(0, Ordering::Relaxed);
    let (ct, _) = bus_b.get_dsp_b_params();
    let sr = lb.sample_rate.load(Ordering::Relaxed).max(44100) as f32;
    log_info(format!(
        "[ASIO-DIAG] 10s: gpu_max={:.0}ms chunks={} lag_max={:.0}ms resets={} | B_under={} B_jump={} B_silent={:.0}ms | A_under={} A_jump={} | cushion={:.0}ms",
        gpu_max_ms, chunks, lag_max_ms, resets, b_under, b_jump, b_silent_ms, a_under, a_jump,
        ct as f32 / sr * 1000.0
    ));
}

/// Daftarkan Driver ASIO tunggal `Rinka SoundBridge ASIO` saat startup.
pub fn register_virtual_asio_drivers(request_admin_if_needed: bool) -> Result<String, String> {
    register_virtual_asio_drivers_with_settings(request_admin_if_needed, None)
}

/// Sinkronkan ke-3 ASIO (Line A, Line B, dan FlexASIO `Rinka SoundBridge ASIO` khusus Output Line A & Input Line B)
/// agar semuanya mengikuti 1 setting terpadu (`256 smp`, `Set In = 2.0`, `Set Out = 2.0`, `Sample Rate`).
pub fn register_virtual_asio_drivers_with_settings(
    request_admin_if_needed: bool,
    settings: Option<&AsioRegistrationSettings>,
) -> Result<String, String> {
    if request_admin_if_needed && settings.is_some() {
        ensure_asio_requirements_installed(false);
    } else {
        ensure_asio_requirements_installed(true);
    }

    let (sr, vc_prof, la_prof, lb_prof, dsp_cfg, rsb_flex_cfg) = if let Some(s) = settings {
        let sr = s.sample_rate.max(44100);
        (
            sr,
            s.vc.clone(),
            s.line_a.clone(),
            s.line_b.clone(),
            s.dsp_rsb.clone(),
            s.rsb_asio.clone(),
        )
    } else if let Some(app_cfg) = crate::config::AppConfig::load() {
        (
            app_cfg.global.sample_rate.max(44100),
            app_cfg.asio_vc,
            app_cfg.asio_line_a,
            app_cfg.asio_line_b,
            app_cfg.asio_dsp_rsb,
            app_cfg.rsb_asio,
        )
    } else {
        #[cfg(windows)]
        let (saved, saved_dsp) = unsafe {
            (
                read_saved_asio_reg_config(),
                read_saved_dsp_rsb_asio_reg_config(),
            )
        };
        #[cfg(not(windows))]
        let (saved, saved_dsp) = (
            SavedRinkaAsioRegistryConfig {
                sample_rate: 48000,
                preferred_smp: 256,
                in_lat_samples: 256,
                out_lat_samples: 256,
                vc_profile: FlexAsioProfileConfig::default(),
            },
            SavedDspRsbRegistryConfig {
                sample_rate: 48000,
                preferred_smp: 256,
                adaptive_cushion: true,
                adaptive_mode: 3,
                adaptive_extra_ms: 25.0,
                cushion_samples: 2880,
                gpu_stall_mult_pct: 115,
                stall_guard_ms: 15.0,
                micro_stretch: true,
                adaptive_reported: true,
                reported_in_ms: 304.0,
                reported_out_ms: 16.0,
                cpu_cores: 2,
            },
        );
        let mut dsp_cfg = DspRsbAsioConfig::default();
        dsp_cfg.adaptive_cushion = saved_dsp.adaptive_cushion;
        dsp_cfg.adaptive_mode = crate::config::AdaptiveCushionMode::from_u8(saved_dsp.adaptive_mode);
        dsp_cfg.adaptive_extra_ms = saved_dsp.adaptive_extra_ms;
        dsp_cfg.dsp_b_cushion_ms = ((saved_dsp.cushion_samples as f32)
            / (saved_dsp.sample_rate.max(44100) as f32))
            * 1000.0;
        dsp_cfg.gpu_stall_multiplier = (saved_dsp.gpu_stall_mult_pct as f32) / 100.0;
        dsp_cfg.stall_guard_ms = saved_dsp.stall_guard_ms;
        dsp_cfg.asio_cpu_cores = saved_dsp.cpu_cores;
        (
            saved.sample_rate,
            saved.vc_profile.clone(),
            FlexAsioProfileConfig::default(),
            FlexAsioProfileConfig::default(),
            dsp_cfg,
            RsbAsioConfig::default(),
        )
    };

    let smp = vc_prof.buffer_size.clamp(32, 2048);
    let in_extra = if vc_prof.set_in_latency_enabled {
        ((vc_prof.in_latency_val.max(0.0) as f64 / 1000.0) * sr as f64) as u32
    } else {
        0
    };
    let out_extra = if vc_prof.set_out_latency_enabled {
        ((vc_prof.out_latency_val.max(0.0) as f64 / 1000.0) * sr as f64) as u32
    } else {
        0
    };
    let in_lat_smp = smp + in_extra;
    let out_lat_smp = smp + out_extra;

    let dsp_sr = sr.clamp(44100, 192000);
    let dsp_smp = dsp_cfg.effective_buffer_size(smp);
    set_asio_cpu_cores(dsp_cfg.asio_cpu_cores);
    let eff_cush_ms = dsp_cfg.dsp_b_cushion_ms.clamp(0.0, 450.0);
    let dsp_cush_smp = (((eff_cush_ms as f64) / 1000.0) * (dsp_sr as f64)).round() as u32;
    let dsp_cush_clamped = dsp_cush_smp.min(24000);
    let dsp_stretch_flag = 1u32;
    let eff_rep_in = if dsp_cfg.adaptive_reported_latency {
        0.0
    } else {
        dsp_cfg.reported_in_ms
    };
    let eff_rep_out = if dsp_cfg.adaptive_reported_latency {
        0.0
    } else {
        dsp_cfg.reported_out_ms
    };
    let (dsp_rep_in_smp, dsp_rep_out_smp) = compute_asio_reported_latency_samples(
        dsp_sr,
        dsp_smp,
        eff_rep_in,
        eff_rep_out,
    );

    get_asio_bus_line_a_out().sync_config(sr, smp, dsp_rep_in_smp, dsp_rep_out_smp);
    get_asio_bus_line_b_in().sync_config(sr, smp, dsp_rep_in_smp, dsp_rep_out_smp);
    get_dsp_rsb_bus_a_out().sync_config(dsp_sr, dsp_smp, dsp_rep_in_smp, dsp_rep_out_smp);
    get_dsp_rsb_bus_b_in().set_adaptive_flags_with_mult(
        dsp_cfg.adaptive_cushion,
        dsp_cfg.adaptive_reported_latency,
        dsp_cfg.gpu_stall_multiplier,
    );
    get_dsp_rsb_bus_b_in().set_adaptive_mode(dsp_cfg.adaptive_mode as u8);
    get_dsp_rsb_bus_b_in().set_user_extra_buffer_ms(dsp_cfg.adaptive_extra_ms);
    get_dsp_rsb_bus_b_in().set_user_stall_guard_ms(dsp_cfg.stall_guard_ms);
    get_dsp_rsb_bus_b_in().sync_config(dsp_sr, dsp_smp, dsp_stretch_flag, dsp_cush_clamped);

    let set_in_en_str = if vc_prof.set_in_latency_enabled { "1" } else { "0" }.to_string();
    let set_in_val_str = format!("{:.2}", vc_prof.in_latency_val);
    let set_out_en_str = if vc_prof.set_out_latency_enabled { "1" } else { "0" }.to_string();
    let set_out_val_str = format!("{:.2}", vc_prof.out_latency_val);

    let wasapi_in_set_str = if vc_prof.wasapi_in_set { "1" } else { "0" }.to_string();
    let wasapi_in_excl_str = if vc_prof.wasapi_in_exclusive { "1" } else { "0" }.to_string();
    let wasapi_in_auto_str = if vc_prof.wasapi_in_autoconvert { "1" } else { "0" }.to_string();
    let in_ch_str = vc_prof.in_channels.to_string();

    let wasapi_out_set_str = if vc_prof.wasapi_out_set { "1" } else { "0" }.to_string();
    let wasapi_out_excl_str = if vc_prof.wasapi_out_exclusive { "1" } else { "0" }.to_string();
    let wasapi_out_auto_str = if vc_prof.wasapi_out_autoconvert { "1" } else { "0" }.to_string();
    let out_ch_str = vc_prof.out_channels.to_string();

    #[cfg(windows)]
    unsafe {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let modules_dir = exe_dir.join("Modules");
        let _ = std::fs::create_dir_all(&modules_dir);
        let target_dll = modules_dir.join("rinka_soundbridge_core.dll");
        let legacy_root_dll = exe_dir.join("rinka_soundbridge_core.dll");

        // Jangan pernah menyalin DLL dari direktori induk (`..\rinka_soundbridge_core.dll`) ke HKLM
        // untuk mencegah DLL planting oleh direktori luar yang tidak tepercaya.
        let fallback_sources = [
            exe_dir.join("target").join("release").join("rinka_soundbridge_core.dll"),
            legacy_root_dll.clone(),
        ];
        for src in &fallback_sources {
            if src.exists() && src != &target_dll {
                let should_copy = match (src.metadata().and_then(|m| m.modified()), target_dll.metadata().and_then(|m| m.modified())) {
                    (Ok(src_t), Ok(dst_t)) => src_t > dst_t,
                    (Ok(_), Err(_)) => true,
                    _ => !target_dll.exists(),
                };
                if should_copy {
                    if std::fs::copy(src, &target_dll).is_err() {
                        let bak = modules_dir.join("rinka_soundbridge_core.dll.old");
                        let _ = std::fs::remove_file(&bak);
                        let _ = std::fs::rename(&target_dll, &bak);
                        let _ = std::fs::copy(src, &target_dll);
                    }
                }
                break;
            }
        }

        // Bersihkan file DLL lepasan di root exe_dir (dist\) agar semua modul berada rapi di dalam dist\Modules\
        if target_dll.exists() && legacy_root_dll.exists() {
            let _ = std::fs::remove_file(&legacy_root_dll);
        }
        let legacy_root_old = exe_dir.join("rinka_soundbridge_core.dll.old");
        if legacy_root_old.exists() {
            let _ = std::fs::remove_file(&legacy_root_old);
        }

        let dll_candidates = [
            target_dll.clone(),
            exe_dir
                .join("target")
                .join("release")
                .join("rinka_soundbridge_core.dll"),
            exe_dir
                .join("..")
                .join("target")
                .join("release")
                .join("rinka_soundbridge_core.dll"),
        ];
        let dll_path = dll_candidates
            .iter()
            .find(|p| p.exists())
            .cloned()
            .unwrap_or(target_dll);
        let dll_str = dll_path.to_string_lossy().to_string();

        let write_reg_str = |hive: ffi::HKEY, subkey: &str, name: &str, val: &str| -> bool {
            let wsub = to_wide(subkey);
            let mut hkey: ffi::HKEY = 0;
            let mut disp: u32 = 0;
            if ffi::RegCreateKeyExW(
                hive,
                wsub.as_ptr(),
                0,
                ptr::null_mut(),
                0,
                ffi::KEY_WRITE,
                ptr::null_mut(),
                &mut hkey,
                &mut disp,
            ) != ffi::ERROR_SUCCESS
            {
                return false;
            }
            let wname = to_wide(name);
            let wval = to_wide(val);
            let ok = ffi::RegSetValueExW(
                hkey,
                if name.is_empty() { ptr::null() } else { wname.as_ptr() },
                0,
                ffi::REG_SZ,
                wval.as_ptr() as *const u8,
                (wval.len() * 2) as u32,
            ) == ffi::ERROR_SUCCESS;
            ffi::RegCloseKey(hkey);
            ok
        };

        let check_hklm_subkey_val = |subkey: &str, val_name: &str, expected: &str| -> bool {
            let wsub = to_wide(subkey);
            let mut hkey: ffi::HKEY = 0;
            if ffi::RegOpenKeyExW(ffi::HKEY_LOCAL_MACHINE, wsub.as_ptr(), 0, ffi::KEY_READ, &mut hkey)
                != ffi::ERROR_SUCCESS
            {
                return false;
            }
            let actual = read_reg_sz(hkey, val_name);
            ffi::RegCloseKey(hkey);
            actual.as_deref() == Some(expected)
        };

        let old_asio_names = [
            "Rinka ASIO Line A Out",
            "Rinka ASIO Line B In",
            "Rinka Line A Out (ke input VC)",
            "Rinka Line B in (dari output VC)",
            "DSP RSB Exp ASIO",
        ];
        for old_name in &old_asio_names {
            let w_old = to_wide(&format!("SOFTWARE\\ASIO\\{}", old_name));
            let _ = ffi::RegDeleteKeyW(ffi::HKEY_CURRENT_USER, w_old.as_ptr());
            let _ = ffi::RegDeleteKeyW(ffi::HKEY_LOCAL_MACHINE, w_old.as_ptr());
        }

        let sr_str = sr.to_string();
        let smp_str = smp.to_string();
        let in_lat_str = in_lat_smp.to_string();
        let out_lat_str = out_lat_smp.to_string();

        for (hive, cls_prefix) in [
            (ffi::HKEY_CURRENT_USER, "Software\\Classes\\CLSID"),
            (ffi::HKEY_LOCAL_MACHINE, "SOFTWARE\\Classes\\CLSID"),
        ] {
            let cls_key = format!("{}\\{}", cls_prefix, CLSID_RINKA_ASIO_STR);
            let inproc_key = format!("{}\\InprocServer32", cls_key);
            let _ = write_reg_str(hive, &cls_key, "", RINKA_ASIO_DRIVER_NAME);
            let _ = write_reg_str(hive, &inproc_key, "", &dll_str);
            let _ = write_reg_str(hive, &inproc_key, "ThreadingModel", "Both");

            let asio_key = format!("SOFTWARE\\ASIO\\{}", RINKA_ASIO_DRIVER_NAME);
            let _ = write_reg_str(hive, &asio_key, "CLSID", CLSID_RINKA_ASIO_STR);
            let _ = write_reg_str(hive, &asio_key, "Description", RINKA_ASIO_DRIVER_NAME);
            let _ = write_reg_str(hive, &asio_key, "SampleRate", &sr_str);
            let _ = write_reg_str(hive, &asio_key, "PreferredBufferSize", &smp_str);
            let _ = write_reg_str(hive, &asio_key, "InLatencySamples", &in_lat_str);
            let _ = write_reg_str(hive, &asio_key, "OutLatencySamples", &out_lat_str);
            let _ = write_reg_str(hive, &asio_key, "SetInEnabled", &set_in_en_str);
            let _ = write_reg_str(hive, &asio_key, "SetInLatency", &set_in_val_str);
            let _ = write_reg_str(hive, &asio_key, "SetOutEnabled", &set_out_en_str);
            let _ = write_reg_str(hive, &asio_key, "SetOutLatency", &set_out_val_str);

            let _ = write_reg_str(hive, &asio_key, "VcInputDevice", &vc_prof.input_device);
            let _ = write_reg_str(hive, &asio_key, "VcOutputDevice", &vc_prof.output_device);
            let _ = write_reg_str(hive, &asio_key, "WasapiInSet", &wasapi_in_set_str);
            let _ = write_reg_str(hive, &asio_key, "WasapiInExclusive", &wasapi_in_excl_str);
            let _ = write_reg_str(hive, &asio_key, "WasapiInAutoConvert", &wasapi_in_auto_str);
            let _ = write_reg_str(hive, &asio_key, "InChannels", &in_ch_str);

            let _ = write_reg_str(hive, &asio_key, "WasapiOutSet", &wasapi_out_set_str);
            let _ = write_reg_str(hive, &asio_key, "WasapiOutExclusive", &wasapi_out_excl_str);
            let _ = write_reg_str(hive, &asio_key, "WasapiOutAutoConvert", &wasapi_out_auto_str);
            let _ = write_reg_str(hive, &asio_key, "OutChannels", &out_ch_str);

            // Daftarkan juga Driver ASIO Internal DSP: "DSP RSB Exp ASIO"
            let dsp_cls_key = format!("{}\\{}", cls_prefix, CLSID_DSP_RSB_ASIO_STR);
            let dsp_inproc_key = format!("{}\\InprocServer32", dsp_cls_key);
            let _ = write_reg_str(hive, &dsp_cls_key, "", DSP_RSB_ASIO_DRIVER_NAME);
            let _ = write_reg_str(hive, &dsp_inproc_key, "", &dll_str);
            let _ = write_reg_str(hive, &dsp_inproc_key, "ThreadingModel", "Both");

            let dsp_sr_str = dsp_sr.to_string();
            let dsp_smp_str = dsp_smp.to_string();
            let dsp_cush_str = dsp_cush_clamped.to_string();
            let dsp_stretch_str = dsp_stretch_flag.to_string();
            let dsp_rep_in_str = format!("{:.1}", dsp_cfg.reported_in_ms);
            let dsp_rep_out_str = format!("{:.1}", dsp_cfg.reported_out_ms);
            let dsp_rep_in_smp_str = dsp_rep_in_smp.to_string();
            let dsp_rep_out_smp_str = dsp_rep_out_smp.to_string();

            let dsp_asio_key = format!("SOFTWARE\\ASIO\\{}", DSP_RSB_ASIO_DRIVER_NAME);
            let _ = write_reg_str(hive, &dsp_asio_key, "CLSID", CLSID_DSP_RSB_ASIO_STR);
            let _ = write_reg_str(hive, &dsp_asio_key, "Description", DSP_RSB_ASIO_DRIVER_NAME);
            let _ = write_reg_str(hive, &dsp_asio_key, "SampleRate", &dsp_sr_str);
            let _ = write_reg_str(hive, &dsp_asio_key, "PreferredBufferSize", &dsp_smp_str);
            let _ = write_reg_str(hive, &dsp_asio_key, "InLatencySamples", &dsp_rep_in_smp_str);
            let _ = write_reg_str(hive, &dsp_asio_key, "OutLatencySamples", &dsp_rep_out_smp_str);
            let _ = write_reg_str(hive, &dsp_asio_key, "CushionSamples", &dsp_cush_str);
            let _ = write_reg_str(hive, &dsp_asio_key, "MicroStretch", &dsp_stretch_str);
            let dsp_cores_str = dsp_cfg.asio_cpu_cores.to_string();
            let dsp_adap_cush_str = if dsp_cfg.adaptive_cushion { "1" } else { "0" };
            let dsp_adap_rep_str = if dsp_cfg.adaptive_reported_latency { "1" } else { "0" };
            let _ = write_reg_str(hive, &dsp_asio_key, "AdaptiveCushion", dsp_adap_cush_str);
            let _ = write_reg_str(hive, &dsp_asio_key, "AdaptiveReported", dsp_adap_rep_str);
            let dsp_mult_pct_str = ((dsp_cfg.gpu_stall_multiplier.clamp(1.00, 1.30) * 100.0).round() as u32).to_string();
            let _ = write_reg_str(hive, &dsp_asio_key, "ReportedInMs", &dsp_rep_in_str);
            let _ = write_reg_str(hive, &dsp_asio_key, "ReportedOutMs", &dsp_rep_out_str);
            let _ = write_reg_str(hive, &dsp_asio_key, "AsioCpuCores", &dsp_cores_str);
            let _ = write_reg_str(hive, &dsp_asio_key, "GpuStallMultPct", &dsp_mult_pct_str);
            let dsp_mode_str = (dsp_cfg.adaptive_mode as u8).to_string();
            let dsp_extra_str = format!("{:.1}", dsp_cfg.adaptive_extra_ms.clamp(0.0, 250.0));
            let dsp_guard_str = format!("{:.1}", dsp_cfg.stall_guard_ms.clamp(0.0, 150.0));
            let _ = write_reg_str(hive, &dsp_asio_key, "AdaptiveMode", &dsp_mode_str);
            let _ = write_reg_str(hive, &dsp_asio_key, "AdaptiveExtraMs", &dsp_extra_str);
            let _ = write_reg_str(hive, &dsp_asio_key, "StallGuardMs", &dsp_guard_str);

            // 3. Daftarkan Driver Built-in FlexASIO Mandiri: "RSB ASIO" (CLSID_RSB_ASIO_STR)
            //    Tidak pernah menimpa atau menyentuh driver FlexASIO asli milik user!
            let rsb_smp = rsb_flex_cfg.effective_buffer_size(smp);
            let (rsb_rep_in_smp, rsb_rep_out_smp) = compute_rsb_flexasio_reported_latency_samples(
                dsp_sr,
                rsb_smp,
                rsb_flex_cfg.set_in_latency_enabled,
                rsb_flex_cfg.in_latency_val,
                rsb_flex_cfg.set_out_latency_enabled,
                rsb_flex_cfg.out_latency_val,
            );

            let rsb_cls_key = format!("{}\\{}", cls_prefix, CLSID_RSB_ASIO_STR);
            let rsb_inproc_key = format!("{}\\InprocServer32", rsb_cls_key);
            let _ = write_reg_str(hive, &rsb_cls_key, "", RSB_ASIO_DRIVER_NAME);
            let _ = write_reg_str(hive, &rsb_inproc_key, "", &dll_str);
            let _ = write_reg_str(hive, &rsb_inproc_key, "ThreadingModel", "Both");

            let rsb_asio_key = format!("SOFTWARE\\ASIO\\{}", RSB_ASIO_DRIVER_NAME);
            let _ = write_reg_str(hive, &rsb_asio_key, "CLSID", CLSID_RSB_ASIO_STR);
            let _ = write_reg_str(hive, &rsb_asio_key, "Description", RSB_ASIO_DRIVER_NAME);
            let _ = write_reg_str(hive, &rsb_asio_key, "SampleRate", &dsp_sr_str);
            let _ = write_reg_str(hive, &rsb_asio_key, "PreferredBufferSize", &rsb_smp.to_string());
            let _ = write_reg_str(hive, &rsb_asio_key, "InLatencySamples", &rsb_rep_in_smp.to_string());
            let _ = write_reg_str(hive, &rsb_asio_key, "OutLatencySamples", &rsb_rep_out_smp.to_string());
            let _ = write_reg_str(hive, &rsb_asio_key, "InputDevice", &rsb_flex_cfg.input_device);
            let _ = write_reg_str(hive, &rsb_asio_key, "OutputDevice", &rsb_flex_cfg.output_device);
            let _ = write_reg_str(
                hive,
                &rsb_asio_key,
                "SetInEnabled",
                if rsb_flex_cfg.set_in_latency_enabled { "1" } else { "0" },
            );
            let _ = write_reg_str(
                hive,
                &rsb_asio_key,
                "SetInLatency",
                &format!("{:.2}", rsb_flex_cfg.in_latency_val),
            );
            let _ = write_reg_str(
                hive,
                &rsb_asio_key,
                "SetOutEnabled",
                if rsb_flex_cfg.set_out_latency_enabled { "1" } else { "0" },
            );
            let _ = write_reg_str(
                hive,
                &rsb_asio_key,
                "SetOutLatency",
                &format!("{:.2}", rsb_flex_cfg.out_latency_val),
            );
            let _ = write_reg_str(
                hive,
                &rsb_asio_key,
                "WasapiInSet",
                if rsb_flex_cfg.wasapi_in_set { "1" } else { "0" },
            );
            let _ = write_reg_str(
                hive,
                &rsb_asio_key,
                "WasapiInExclusive",
                if rsb_flex_cfg.wasapi_in_exclusive { "1" } else { "0" },
            );
            let _ = write_reg_str(
                hive,
                &rsb_asio_key,
                "WasapiInAutoConvert",
                if rsb_flex_cfg.wasapi_in_autoconvert { "1" } else { "0" },
            );
            let _ = write_reg_str(hive, &rsb_asio_key, "InChannels", &rsb_flex_cfg.in_channels.to_string());
            let _ = write_reg_str(
                hive,
                &rsb_asio_key,
                "WasapiOutSet",
                if rsb_flex_cfg.wasapi_out_set { "1" } else { "0" },
            );
            let _ = write_reg_str(
                hive,
                &rsb_asio_key,
                "WasapiOutExclusive",
                if rsb_flex_cfg.wasapi_out_exclusive { "1" } else { "0" },
            );
            let _ = write_reg_str(
                hive,
                &rsb_asio_key,
                "WasapiOutAutoConvert",
                if rsb_flex_cfg.wasapi_out_autoconvert { "1" } else { "0" },
            );
            let _ = write_reg_str(hive, &rsb_asio_key, "OutChannels", &rsb_flex_cfg.out_channels.to_string());
        }

        let hklm_ready = check_hklm_subkey_val(
            &format!("SOFTWARE\\ASIO\\{}", RSB_ASIO_DRIVER_NAME),
            "CLSID",
            CLSID_RSB_ASIO_STR,
        ) && check_hklm_subkey_val(
            &format!("SOFTWARE\\ASIO\\{}", DSP_RSB_ASIO_DRIVER_NAME),
            "CLSID",
            CLSID_DSP_RSB_ASIO_STR,
        ) && check_hklm_subkey_val(
            &format!("SOFTWARE\\Classes\\CLSID\\{}\\InprocServer32", CLSID_DSP_RSB_ASIO_STR),
            "",
            &dll_str,
        ) && !check_hklm_subkey_val(
            "SOFTWARE\\ASIO\\DSP RSB Exp ASIO",
            "CLSID",
            CLSID_DSP_RSB_ASIO_STR,
        );

        if !hklm_ready && request_admin_if_needed {
            let escaped_dll = dll_str.replace('\\', "\\\\");
            let reg_content = format!(
                "Windows Registry Editor Version 5.00\r\n\r\n\
[-HKEY_LOCAL_MACHINE\\SOFTWARE\\ASIO\\Rinka ASIO Line A Out]\r\n\
[-HKEY_LOCAL_MACHINE\\SOFTWARE\\ASIO\\Rinka ASIO Line B In]\r\n\
[-HKEY_LOCAL_MACHINE\\SOFTWARE\\ASIO\\Rinka Line A Out (ke input VC)]\r\n\
[-HKEY_LOCAL_MACHINE\\SOFTWARE\\ASIO\\Rinka Line B in (dari output VC)]\r\n\
[-HKEY_LOCAL_MACHINE\\SOFTWARE\\ASIO\\DSP RSB Exp ASIO]\r\n\
[-HKEY_CURRENT_USER\\SOFTWARE\\ASIO\\Rinka ASIO Line A Out]\r\n\
[-HKEY_CURRENT_USER\\SOFTWARE\\ASIO\\Rinka ASIO Line B In]\r\n\
[-HKEY_CURRENT_USER\\SOFTWARE\\ASIO\\Rinka Line A Out (ke input VC)]\r\n\
[-HKEY_CURRENT_USER\\SOFTWARE\\ASIO\\Rinka Line B in (dari output VC)]\r\n\
[-HKEY_CURRENT_USER\\SOFTWARE\\ASIO\\DSP RSB Exp ASIO]\r\n\
[-HKEY_LOCAL_MACHINE\\SOFTWARE\\Classes\\CLSID\\{clsid_old_b}]\r\n\
[-HKEY_CURRENT_USER\\Software\\Classes\\CLSID\\{clsid_old_b}]\r\n\r\n\
[HKEY_LOCAL_MACHINE\\SOFTWARE\\Classes\\CLSID\\{clsid}]\r\n\
@=\"{name}\"\r\n\r\n\
[HKEY_LOCAL_MACHINE\\SOFTWARE\\Classes\\CLSID\\{clsid}\\InprocServer32]\r\n\
@=\"{dll}\"\r\n\
\"ThreadingModel\"=\"Both\"\r\n\r\n\
[HKEY_LOCAL_MACHINE\\SOFTWARE\\ASIO\\{name}]\r\n\
\"CLSID\"=\"{clsid}\"\r\n\
\"Description\"=\"{name}\"\r\n\
\"SampleRate\"=\"{sr}\"\r\n\
\"PreferredBufferSize\"=\"{smp}\"\r\n\
\"InLatencySamples\"=\"{in_lat}\"\r\n\
\"OutLatencySamples\"=\"{out_lat}\"\r\n\
\"SetInEnabled\"=\"{set_in_en}\"\r\n\
\"SetInLatency\"=\"{set_in_val}\"\r\n\
\"SetOutEnabled\"=\"{set_out_en}\"\r\n\
\"SetOutLatency\"=\"{set_out_val}\"\r\n\r\n\
[HKEY_LOCAL_MACHINE\\SOFTWARE\\Classes\\CLSID\\{dsp_clsid}]\r\n\
@=\"{dsp_name}\"\r\n\r\n\
[HKEY_LOCAL_MACHINE\\SOFTWARE\\Classes\\CLSID\\{dsp_clsid}\\InprocServer32]\r\n\
@=\"{dll}\"\r\n\
\"ThreadingModel\"=\"Both\"\r\n\r\n\
[HKEY_LOCAL_MACHINE\\SOFTWARE\\ASIO\\{dsp_name}]\r\n\
\"CLSID\"=\"{dsp_clsid}\"\r\n\
\"Description\"=\"{dsp_name}\"\r\n\
\"SampleRate\"=\"{dsp_sr}\"\r\n\
\"PreferredBufferSize\"=\"{dsp_smp}\"\r\n\
\"InLatencySamples\"=\"{dsp_rep_in_smp}\"\r\n\
\"OutLatencySamples\"=\"{dsp_rep_out_smp}\"\r\n\
\"CushionSamples\"=\"{dsp_cush}\"\r\n\
\"MicroStretch\"=\"{dsp_stretch}\"\r\n\
\"AdaptiveCushion\"=\"{dsp_adap_cush}\"\r\n\
\"AdaptiveReported\"=\"{dsp_adap_rep}\"\r\n\r\n\
[HKEY_LOCAL_MACHINE\\SOFTWARE\\Classes\\CLSID\\{rsb_clsid}]\r\n\
@=\"{rsb_name}\"\r\n\r\n\
[HKEY_LOCAL_MACHINE\\SOFTWARE\\Classes\\CLSID\\{rsb_clsid}\\InprocServer32]\r\n\
@=\"{dll}\"\r\n\
\"ThreadingModel\"=\"Both\"\r\n\r\n\
[HKEY_LOCAL_MACHINE\\SOFTWARE\\ASIO\\{rsb_name}]\r\n\
\"CLSID\"=\"{rsb_clsid}\"\r\n\
\"Description\"=\"{rsb_name}\"\r\n\
\"SampleRate\"=\"{dsp_sr}\"\r\n\
\"PreferredBufferSize\"=\"{rsb_smp}\"\r\n",
                clsid = CLSID_RINKA_ASIO_STR,
                clsid_old_b = CLSID_RINKA_ASIO_LEGACY_B_STR,
                name = RINKA_ASIO_DRIVER_NAME,
                dsp_clsid = CLSID_DSP_RSB_ASIO_STR,
                dsp_name = DSP_RSB_ASIO_DRIVER_NAME,
                rsb_clsid = CLSID_RSB_ASIO_STR,
                rsb_name = RSB_ASIO_DRIVER_NAME,
                rsb_smp = rsb_flex_cfg.effective_buffer_size(smp),
                dll = escaped_dll,
                sr = sr_str,
                smp = smp_str,
                in_lat = in_lat_str,
                out_lat = out_lat_str,
                set_in_en = set_in_en_str,
                set_in_val = set_in_val_str,
                set_out_en = set_out_en_str,
                set_out_val = set_out_val_str,
                dsp_sr = dsp_sr,
                dsp_smp = dsp_smp,
                dsp_rep_in_smp = dsp_rep_in_smp,
                dsp_rep_out_smp = dsp_rep_out_smp,
                dsp_cush = dsp_cush_clamped,
                dsp_stretch = dsp_stretch_flag,
                dsp_adap_cush = if dsp_cfg.adaptive_cushion { "1" } else { "0" },
                dsp_adap_rep = if dsp_cfg.adaptive_reported_latency { "1" } else { "0" },
            );

            // Tulis ke file sementara unik di %TEMP% dengan `create_new(true)` (bukan file statis di `Modules\`)
            // dan wajibkan `C:\Windows\System32\reg.exe` absolut tanpa fallback PATH untuk mencegah TOCTOU/PATH hijack.
            let reg_exe = r"C:\Windows\System32\reg.exe";
            if std::path::Path::new(reg_exe).exists() {
                let reg_file_path = std::env::temp_dir().join(format!(
                    "rsb_asio_reg_{}_{}.reg",
                    std::process::id(),
                    now_epoch_ms()
                ));
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&reg_file_path)
                {
                    use std::io::Write;
                    if f.write_all(reg_content.as_bytes()).is_ok() && f.sync_all().is_ok() {
                        drop(f);
                        let reg_path_str = reg_file_path.to_string_lossy().to_string();
                        let w_op = to_wide("runas");
                        let w_file = to_wide(reg_exe);
                        let w_params = to_wide(&format!("import \"{}\"", reg_path_str));
                        let _ = ffi::ShellExecuteW(
                            0,
                            w_op.as_ptr(),
                            w_file.as_ptr(),
                            w_params.as_ptr(),
                            ptr::null(),
                            ffi::SW_HIDE,
                        );
                        std::thread::sleep(Duration::from_millis(550));
                    }
                }
                let _ = std::fs::remove_file(&reg_file_path);
            }
        }

        let flex_status = if find_flexasio_com_info().is_some() {
            "Active (Integrated 1:1 with FlexASIO Engine + FlexASIO.toml)"
        } else {
            "Active (Built-in Standalone WASAPI Flex-ASIO Engine)"
        };

        let fmt_prof = |p: &FlexAsioProfileConfig| -> String {
            let i = if p.input_device.is_empty() { "(None)" } else { &p.input_device };
            let o = if p.output_device.is_empty() { "(None)" } else { &p.output_device };
            let li = if p.set_in_latency_enabled { format!("{:.1}", p.in_latency_val) } else { "Auto".into() };
            let lo = if p.set_out_latency_enabled { format!("{:.1}", p.out_latency_val) } else { "Auto".into() };
            format!(
                "In: {}  ->  Out: {}\n   ({} smp | Latency In: {}, Out: {} | AutoConvert In:{}, Out:{})",
                i, o, p.buffer_size, li, lo, p.wasapi_in_autoconvert, p.wasapi_out_autoconvert
            )
        };

        if settings.is_some() {
            Ok(format!(
                "Konfigurasi Triple ASIO berhasil disimpan & diterapkan:\n\n\
                 1. ASIO Line A (Internal RSB) :\n   {la}\n\n\
                 2. ASIO Line B (Internal RSB) :\n   {lb}\n\n\
                 3. ASIO Khusus VC ('{name}') :\n   {vc}\n   Status: {flex_status}\n\n\
                 Di aplikasi AI Voice Changer, pilih Input & Output: 'ASIO: {name}'.",
                name = RINKA_ASIO_DRIVER_NAME,
                la = fmt_prof(&la_prof),
                lb = fmt_prof(&lb_prof),
                vc = fmt_prof(&vc_prof),
                flex_status = flex_status,
            ))
        } else {
            Ok(format!(
                "ASIO Driver ready for External AI Voice Changer:\n- Input & Output: ASIO: {} ({} Hz / {} smp)",
                RINKA_ASIO_DRIVER_NAME, sr, smp
            ))
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (
            request_admin_if_needed,
            sr,
            smp,
            in_lat_smp,
            out_lat_smp,
            set_in_en_str,
            set_in_val_str,
            set_out_en_str,
            set_out_val_str,
            wasapi_in_set_str,
            wasapi_in_excl_str,
            wasapi_in_auto_str,
            in_ch_str,
            wasapi_out_set_str,
            wasapi_out_excl_str,
            wasapi_out_auto_str,
            out_ch_str,
            la_prof,
            lb_prof,
            vc_prof,
            dsp_cfg,
            rsb_flex_cfg,
        );
        Err("Only available on Windows".into())
    }
}

// ==============================================================================
// IMPLEMENTASI COM IN-PROCESS SERVER (`IASIO` & `IClassFactory`) UNTUK EXTERNAL VC / DAW
// ==============================================================================

#[repr(C)]
struct Asio64Bit {
    hi: u32,
    lo: u32,
}

#[repr(C)]
struct IClassFactoryVtbl {
    query_interface:
        unsafe extern "system" fn(this: *mut c_void, riid: *const ffi::GUID, ppv: *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(this: *mut c_void) -> u32,
    release: unsafe extern "system" fn(this: *mut c_void) -> u32,
    create_instance: unsafe extern "system" fn(
        this: *mut c_void,
        outer: *mut c_void,
        riid: *const ffi::GUID,
        ppv: *mut *mut c_void,
    ) -> i32,
    lock_server: unsafe extern "system" fn(this: *mut c_void, lock: i32) -> i32,
}

#[repr(C)]
struct RinkaAsioClassFactory {
    vtbl: *const IClassFactoryVtbl,
}

#[repr(C)]
struct RinkaVirtualAsioComObject {
    vtbl: *const IASIOVtbl,
    ref_count: AtomicU32,
    inner_asio: Mutex<*mut c_void>,
    last_config_gen: AtomicU32,
    sample_rate: Mutex<f64>,
    buffer_size: AtomicI32,
    sample_pos: AtomicU64,
    running: Arc<AtomicBool>,
    stop_lock: Mutex<()>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
    standalone_pair: Mutex<Option<WasapiFlexAsioPair>>,
    buf_infos: Mutex<Vec<ASIOBufferInfo>>,
    channel_storage: Mutex<Vec<[Vec<f32>; 2]>>,
    callbacks: Mutex<Option<ASIOCallbacksCopy>>,
}

#[derive(Clone, Copy)]
struct ASIOCallbacksCopy {
    buffer_switch: unsafe extern "C" fn(double_buffer_index: i32, direct_process: i32),
}

unsafe impl Send for RinkaVirtualAsioComObject {}
unsafe impl Sync for RinkaVirtualAsioComObject {}

unsafe impl Send for ASIOCallbacksCopy {}
unsafe impl Sync for ASIOCallbacksCopy {}

unsafe extern "system" fn v_asio_qi(
    this: *mut c_void,
    _riid: *const ffi::GUID,
    ppv: *mut *mut c_void,
) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if this.is_null() || ppv.is_null() {
            return -2147467261; // E_POINTER
        }
        *ppv = this;
        v_asio_add_ref(this);
        0 // S_OK
    }))
    .unwrap_or(-2147467259)
}

unsafe extern "system" fn v_asio_add_ref(this: *mut c_void) -> u32 {
    if this.is_null() {
        return 0;
    }
    let obj = &*(this as *const RinkaVirtualAsioComObject);
    obj.ref_count.fetch_add(1, Ordering::Relaxed) + 1
}

unsafe extern "system" fn v_asio_release(this: *mut c_void) -> u32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if this.is_null() {
            return 0;
        }
        let obj = &*(this as *const RinkaVirtualAsioComObject);
        let prev = obj.ref_count.fetch_sub(1, Ordering::Release);
        if prev == 0 {
            obj.ref_count.store(0, Ordering::Relaxed);
            return 0;
        }
        if prev == 1 {
            std::sync::atomic::fence(Ordering::Acquire);
            let _ = v_asio_stop(this);
            let _ = v_asio_dispose_buffers(this);
            let inner = *obj.inner_asio.lock();
            if !inner.is_null() {
                let vtbl = &**(inner as *mut *const IASIOVtbl);
                let _ = (vtbl.release)(inner);
                *obj.inner_asio.lock() = ptr::null_mut();
            }
            let _ = Box::from_raw(this as *mut RinkaVirtualAsioComObject);
            return 0;
        }
        prev - 1
    }))
    .unwrap_or(0)
}

unsafe extern "system" fn v_asio_get_driver_version(_this: *mut c_void) -> i32 {
    2
}

unsafe extern "system" fn v_asio_get_error_message(_this: *mut c_void, string: *mut c_char) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if !string.is_null() {
            let s: &[u8] = &b"No Error\0"[..];
            ptr::copy_nonoverlapping(s.as_ptr() as *const c_char, string, s.len());
        }
    }));
}

static PY_PROBE_SESSION_GEN: AtomicU64 = AtomicU64::new(1);

unsafe extern "system" fn v_asio_stop(this: *mut c_void) -> ASIOError {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if this.is_null() {
            return -1000;
        }
        let obj = &*(this as *const RinkaVirtualAsioComObject);
        // Serialize concurrent `v_asio_stop` / `v_asio_dispose_buffers` calls so `channel_storage`
        // can NEVER be cleared while an earlier `v_asio_stop` is still joining the worker thread!
        let _stop_guard = obj.stop_lock.lock();
        PY_PROBE_SESSION_GEN.fetch_add(1, Ordering::SeqCst);
        *obj.standalone_pair.lock() = None;
        obj.running.store(false, Ordering::SeqCst);
        let handle = obj.worker.lock().take();
        if let Some(h) = handle {
            let _ = h.join();
        }
        ASE_OK
    }))
    .unwrap_or(-1000)
}

unsafe extern "system" fn v_asio_get_channels(
    _this: *mut c_void,
    num_in: *mut i32,
    num_out: *mut i32,
) -> ASIOError {
    if !num_in.is_null() {
        *num_in = 2;
    }
    if !num_out.is_null() {
        *num_out = 2;
    }
    ASE_OK
}

unsafe extern "system" fn v_asio_get_clock_sources(
    _this: *mut c_void,
    clocks: *mut c_void,
    num_sources: *mut i32,
) -> ASIOError {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if !num_sources.is_null() {
            if *num_sources > 0 && !clocks.is_null() {
                // sizeof(ASIOClockSource) == 48 bytes (index:4 + associatedChannel:4 + associatedGroup:4 + isCurrentSource:4 + name:32)
                ptr::write_bytes(clocks as *mut u8, 0, 48);
            }
            *num_sources = 1;
        }
        ASE_OK
    }))
    .unwrap_or(-1000)
}

unsafe extern "system" fn v_asio_set_clock_source(_this: *mut c_void, _reference: i32) -> ASIOError {
    ASE_OK
}

unsafe extern "system" fn v_asio_get_sample_position(
    this: *mut c_void,
    s_pos: *mut i64,
    t_stamp: *mut i64,
) -> ASIOError {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if this.is_null() {
            return -1000;
        }
        let obj = &*(this as *const RinkaVirtualAsioComObject);
        let sp = obj.sample_pos.load(Ordering::Relaxed);
        let ns = now_epoch_ns();
        if !s_pos.is_null() {
            let p = s_pos as *mut Asio64Bit;
            (*p).hi = (sp >> 32) as u32;
            (*p).lo = (sp & 0xFFFF_FFFF) as u32;
        }
        if !t_stamp.is_null() {
            let t = t_stamp as *mut Asio64Bit;
            (*t).hi = (ns >> 32) as u32;
            (*t).lo = (ns & 0xFFFF_FFFF) as u32;
        }
        ASE_OK
    }))
    .unwrap_or(-1000)
}

unsafe extern "system" fn v_asio_create_buffers(
    this: *mut c_void,
    buffer_infos: *mut ASIOBufferInfo,
    num_channels: i32,
    buffer_size: i32,
    callbacks: *const ASIOCallbacks,
) -> ASIOError {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if this.is_null() || buffer_infos.is_null() || callbacks.is_null() || num_channels <= 0 || num_channels > 8 {
            return -1000;
        }
        let obj = &*(this as *const RinkaVirtualAsioComObject);
        let _ = v_asio_stop(this);
        let n_ch = num_channels as usize;
        let blk = (buffer_size.clamp(32, 4096)) as usize;
        let alloc_cap = blk.max(4096);
        obj.buffer_size.store(blk as i32, Ordering::Relaxed);

        let mut storage = obj.channel_storage.lock();
        let mut infos = obj.buf_infos.lock();
        storage.clear();
        infos.clear();

        for i in 0..n_ch {
            let mut b0 = vec![0.0f32; alloc_cap];
            let mut b1 = vec![0.0f32; alloc_cap];
            let p0 = b0.as_mut_ptr() as *mut c_void;
            let p1 = b1.as_mut_ptr() as *mut c_void;
            storage.push([b0, b1]);

            let caller_info = &mut *buffer_infos.add(i);
            caller_info.buffers[0] = p0;
            caller_info.buffers[1] = p1;
            infos.push(*caller_info);
        }

        *obj.callbacks.lock() = Some(ASIOCallbacksCopy {
            buffer_switch: (*callbacks).buffer_switch,
        });
        ASE_OK
    }))
    .unwrap_or(-1000)
}

unsafe extern "system" fn v_asio_dispose_buffers(this: *mut c_void) -> ASIOError {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if this.is_null() {
            return -1000;
        }
        let _ = v_asio_stop(this);
        let obj = &*(this as *const RinkaVirtualAsioComObject);
        let _stop_guard = obj.stop_lock.lock();
        obj.buf_infos.lock().clear();
        obj.channel_storage.lock().clear();
        *obj.callbacks.lock() = None;
        ASE_OK
    }))
    .unwrap_or(-1000)
}

unsafe extern "system" fn v_asio_control_panel(_this: *mut c_void) -> ASIOError {
    ASE_OK
}

unsafe extern "system" fn v_asio_future(
    _this: *mut c_void,
    _selector: i32,
    _opt: *mut c_void,
) -> ASIOError {
    -1000
}

unsafe extern "system" fn v_asio_output_ready(_this: *mut c_void) -> ASIOError {
    ASE_OK
}

unsafe extern "system" fn cf_qi(
    this: *mut c_void,
    riid: *const ffi::GUID,
    ppv: *mut *mut c_void,
) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if this.is_null() || ppv.is_null() || riid.is_null() {
            return -2147467261;
        }
        let r = &*riid;
        if *r == IID_IUNKNOWN || *r == IID_ICLASSFACTORY {
            *ppv = this;
            return 0;
        }
        *ppv = ptr::null_mut();
        -2147467262 // E_NOINTERFACE
    }))
    .unwrap_or(-2147467259)
}

unsafe extern "system" fn cf_add_ref(_this: *mut c_void) -> u32 {
    2
}

unsafe extern "system" fn cf_release(_this: *mut c_void) -> u32 {
    1
}

unsafe extern "system" fn cf_lock_server(_this: *mut c_void, _lock: i32) -> i32 {
    0
}

unsafe impl Sync for RinkaAsioClassFactory {}

// ==============================================================================
// DRIVER ASIO MURNI INTERNAL DSP: "DSP RSB Exp ASIO" (DSP A Out -> VC -> DSP B In)
// ==============================================================================

unsafe extern "system" fn v_dsp_asio_init(this: *mut c_void, _sys_handle: *mut c_void) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if this.is_null() {
            return 0;
        }
        let obj = &*(this as *const RinkaVirtualAsioComObject);
        #[cfg(windows)]
        {
            let saved = read_saved_dsp_rsb_asio_reg_config();
            let eff_in_ms = if saved.adaptive_reported { 0.0 } else { saved.reported_in_ms };
            let eff_out_ms = if saved.adaptive_reported { 0.0 } else { saved.reported_out_ms };
            let (rep_in_smp, rep_out_smp) = compute_asio_reported_latency_samples(
                saved.sample_rate,
                saved.preferred_smp,
                eff_in_ms,
                eff_out_ms,
            );
            let bus_a = get_dsp_rsb_bus_a_out();
            if bus_a.config_generation() <= 1 {
                bus_a.sync_config(
                    saved.sample_rate,
                    saved.preferred_smp,
                    rep_in_smp,
                    rep_out_smp,
                );
            }
            let bus_b = get_dsp_rsb_bus_b_in();
            if bus_b.config_generation() <= 1 {
                bus_b.set_adaptive_flags_with_mult(
                    saved.adaptive_cushion,
                    saved.adaptive_reported,
                    (saved.gpu_stall_mult_pct as f32) / 100.0,
                );
                bus_b.set_adaptive_mode(saved.adaptive_mode);
                bus_b.set_user_extra_buffer_ms(saved.adaptive_extra_ms);
                bus_b.set_user_stall_guard_ms(saved.stall_guard_ms);
                bus_b.sync_config(
                    saved.sample_rate,
                    saved.preferred_smp,
                    1,
                    saved.cushion_samples,
                );
            }
        }
        let bus = get_dsp_rsb_bus_a_out();
        let (sr, smp, _, _) = bus.get_config();
        *obj.sample_rate.lock() = sr as f64;
        obj.buffer_size.store(smp.clamp(32, 2048) as i32, Ordering::Relaxed);
        obj.last_config_gen.store(bus.config_generation(), Ordering::Relaxed);
        obj.sample_pos.store(0, Ordering::Relaxed);
        1 // ASIOTrue
    }))
    .unwrap_or(0)
}

unsafe extern "system" fn v_dsp_asio_get_buffer_size(
    this: *mut c_void,
    min_size: *mut i32,
    max_size: *mut i32,
    preferred_size: *mut i32,
    granularity: *mut i32,
) -> ASIOError {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if this.is_null() {
            return -1000;
        }
        let obj = &*(this as *const RinkaVirtualAsioComObject);
        #[cfg(windows)]
        let (sr, pref_smp) = {
            let saved = read_saved_dsp_rsb_asio_reg_config();
            (saved.sample_rate, saved.preferred_smp)
        };
        #[cfg(not(windows))]
        let (sr, pref_smp) = {
            let bus = get_dsp_rsb_bus_a_out();
            let (s, p, _, _) = bus.get_config();
            (s, p)
        };
        *obj.sample_rate.lock() = sr as f64;
        let locked_smp = pref_smp.clamp(32, 2048) as i32;
        if !obj.running.load(Ordering::Relaxed) && obj.channel_storage.lock().is_empty() {
            obj.buffer_size.store(locked_smp, Ordering::Relaxed);
        }
        if !min_size.is_null() {
            *min_size = locked_smp;
        }
        if !max_size.is_null() {
            *max_size = locked_smp;
        }
        if !preferred_size.is_null() {
            *preferred_size = locked_smp;
        }
        if !granularity.is_null() {
            *granularity = 0;
        }
        ASE_OK
    }))
    .unwrap_or(-1000)
}

unsafe extern "system" fn v_dsp_asio_can_sample_rate(
    _this: *mut c_void,
    sample_rate: f64,
) -> ASIOError {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        #[cfg(windows)]
        let bus_sr = read_saved_dsp_rsb_asio_reg_config().sample_rate;
        #[cfg(not(windows))]
        let (bus_sr, _, _, _) = get_dsp_rsb_bus_a_out().get_config();
        if (sample_rate - (bus_sr as f64)).abs() < 1.0 {
            ASE_OK
        } else {
            ASE_NO_CLOCK
        }
    }))
    .unwrap_or(-1000)
}

unsafe extern "system" fn v_dsp_asio_get_sample_rate(
    this: *mut c_void,
    sample_rate: *mut f64,
) -> ASIOError {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if this.is_null() {
            return -1000;
        }
        let obj = &*(this as *const RinkaVirtualAsioComObject);
        #[cfg(windows)]
        let (bus_sr, pref_smp) = {
            let saved = read_saved_dsp_rsb_asio_reg_config();
            (saved.sample_rate, saved.preferred_smp)
        };
        #[cfg(not(windows))]
        let (bus_sr, pref_smp) = {
            let bus = get_dsp_rsb_bus_a_out();
            let (s, p, _, _) = bus.get_config();
            (s, p)
        };
        let mut guard = obj.sample_rate.lock();
        *guard = bus_sr as f64;
        if !obj.running.load(Ordering::Relaxed) && obj.channel_storage.lock().is_empty() {
            obj.buffer_size.store(pref_smp.clamp(32, 2048) as i32, Ordering::Relaxed);
        }
        if !sample_rate.is_null() {
            *sample_rate = bus_sr as f64;
        }
        ASE_OK
    }))
    .unwrap_or(-1000)
}

unsafe extern "system" fn v_dsp_asio_set_sample_rate(
    this: *mut c_void,
    sample_rate: f64,
) -> ASIOError {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if this.is_null() {
            return -1000;
        }
        let obj = &*(this as *const RinkaVirtualAsioComObject);
        #[cfg(windows)]
        let bus_sr = read_saved_dsp_rsb_asio_reg_config().sample_rate;
        #[cfg(not(windows))]
        let (bus_sr, _, _, _) = get_dsp_rsb_bus_a_out().get_config();
        if (sample_rate - (bus_sr as f64)).abs() < 1.0 {
            *obj.sample_rate.lock() = bus_sr as f64;
            ASE_OK
        } else {
            ASE_NO_CLOCK
        }
    }))
    .unwrap_or(-1000)
}

unsafe extern "system" fn v_dsp_asio_get_driver_name(_this: *mut c_void, name: *mut c_char) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if name.is_null() {
            return;
        }
        let s: &[u8] = &b"Rinka SoundBridge ASIO\0"[..];
        ptr::copy_nonoverlapping(s.as_ptr() as *const c_char, name, s.len());
    }));
}

unsafe extern "system" fn v_dsp_asio_get_channel_info(
    _this: *mut c_void,
    info: *mut ASIOChannelInfo,
) -> ASIOError {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if info.is_null() {
            return -1000;
        }
        let inf = &mut *info;
        inf.is_active = 1;
        inf.channel_group = 0;
        inf.sample_type = ASIOST_FLOAT32_LSB;
        inf.name = [0; 32];
        let label: &[u8] = if inf.is_input != 0 {
            if inf.channel == 0 {
                &b"DSP A Out L (In VC)\0"[..]
            } else {
                &b"DSP A Out R (In VC)\0"[..]
            }
        } else if inf.channel == 0 {
            &b"DSP B In L (Out VC)\0"[..]
        } else {
            &b"DSP B In R (Out VC)\0"[..]
        };
        ptr::copy_nonoverlapping(
            label.as_ptr() as *const c_char,
            inf.name.as_mut_ptr(),
            label.len().min(31),
        );
        ASE_OK
    }))
    .unwrap_or(-1000)
}

unsafe extern "system" fn v_dsp_asio_get_latencies(
    this: *mut c_void,
    input_lat: *mut i32,
    output_lat: *mut i32,
) -> ASIOError {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if this.is_null() {
            return -1000;
        }
        let obj = &*(this as *const RinkaVirtualAsioComObject);
        let blk = obj.buffer_size.load(Ordering::Relaxed).clamp(32, 4096) as u32;
        #[cfg(windows)]
        let (in_smp, out_smp) = {
            let saved = read_saved_dsp_rsb_asio_reg_config();
            let eff_in_ms = if saved.adaptive_reported { 0.0 } else { saved.reported_in_ms };
            let eff_out_ms = if saved.adaptive_reported { 0.0 } else { saved.reported_out_ms };
            compute_asio_reported_latency_samples(
                saved.sample_rate,
                blk,
                eff_in_ms,
                eff_out_ms,
            )
        };
        #[cfg(not(windows))]
        let (in_smp, out_smp) = {
            let (_, _, il, ol) = get_dsp_rsb_bus_a_out().get_config();
            (il.max(blk), ol.max(blk))
        };
        if !input_lat.is_null() {
            *input_lat = in_smp as i32;
        }
        if !output_lat.is_null() {
            *output_lat = out_smp as i32;
        }
        ASE_OK
    }))
    .unwrap_or(-1000)
}

static PY_PROBE_WAKE_NOTIFY: AtomicBool = AtomicBool::new(false);

#[cfg(windows)]
#[inline]
fn trigger_py_probe_wakeup() {
    PY_PROBE_WAKE_NOTIFY.store(true, Ordering::Release);
}

/// Membaca nilai `GPU Delay: XX ms` dari memori runtime Python External VC (`python312.dll`) secara aman:
/// - HANYA berjalan saat stream ASIO aktif (`run_flag == true` di dalam `v_dsp_asio_start`) dan langsung
///   berhenti begitu `v_asio_stop` dipanggil, sehingga TIDAK PERNAH menyentuh GIL atau CUDA context
///   ketika External VC menjalankan `prepare_conversion` (`CUDAGraph.capture_begin() .. capture_end()`)!
/// - Setelah `_rinka_gui` ditemukan 1 kali pada awal sesi, pembacaan berikutnya menggunakan C-API murni
///   (`PyObject_GetAttrString` -> `callback_processor` -> `current_inference_time` -> `PyFloat_AsDouble`)
///   tanpa kompilasi script Python berulang sehingga 100% bebas kontensi GIL!
#[cfg(windows)]
fn spawn_external_python_gpu_delay_probe(run_flag: Arc<AtomicBool>, session_gen: u64) {
    let _ = std::thread::Builder::new()
        .name("rinka-ext-py-probe".into())
        .spawn(move || unsafe {
            // Tunggu 650ms seusai `v_dsp_asio_start` agar seluruh inisialisasi stream & CUDA Graph selesai 100%
            for _ in 0..13 {
                if !run_flag.load(Ordering::Relaxed)
                    || PY_PROBE_SESSION_GEN.load(Ordering::Relaxed) != session_gen
                {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }

            let dll_names = [
                "python312.dll",
                "python311.dll",
                "python310.dll",
                "python313.dll",
            ];
            let mut h_py: ffi::HINSTANCE = 0;
            for name in &dll_names {
                let wname = to_wide(name);
                let h = ffi::GetModuleHandleW(wname.as_ptr());
                if h != 0 {
                    h_py = h;
                    break;
                }
            }
            if h_py == 0 {
                return;
            }

            type PyIsInitializedFn = unsafe extern "C" fn() -> i32;
            type PyGILStateEnsureFn = unsafe extern "C" fn() -> isize;
            type PyGILStateReleaseFn = unsafe extern "C" fn(isize);
            type PyImportAddModuleFn = unsafe extern "C" fn(*const u8) -> *mut c_void;
            type PyModuleGetDictFn = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
            type PyRunSimpleStringFn = unsafe extern "C" fn(*const u8) -> i32;
            type PyDictGetItemStringFn =
                unsafe extern "C" fn(*mut c_void, *const u8) -> *mut c_void;
            type PyObjectGetAttrStringFn =
                unsafe extern "C" fn(*mut c_void, *const u8) -> *mut c_void;
            type PyFloatAsDoubleFn = unsafe extern "C" fn(*mut c_void) -> f64;
            type PyErrClearFn = unsafe extern "C" fn();
            type PyDecRefFn = unsafe extern "C" fn(*mut c_void);

            let p_is_init = ffi::GetProcAddress(h_py, b"Py_IsInitialized\0".as_ptr());
            let p_gil_ensure = ffi::GetProcAddress(h_py, b"PyGILState_Ensure\0".as_ptr());
            let p_gil_release = ffi::GetProcAddress(h_py, b"PyGILState_Release\0".as_ptr());
            let p_add_mod = ffi::GetProcAddress(h_py, b"PyImport_AddModule\0".as_ptr());
            let p_get_dict = ffi::GetProcAddress(h_py, b"PyModule_GetDict\0".as_ptr());
            let p_run_simple = ffi::GetProcAddress(h_py, b"PyRun_SimpleString\0".as_ptr());
            let p_dict_get = ffi::GetProcAddress(h_py, b"PyDict_GetItemString\0".as_ptr());
            let p_get_attr = ffi::GetProcAddress(h_py, b"PyObject_GetAttrString\0".as_ptr());
            let p_float_as_dbl = ffi::GetProcAddress(h_py, b"PyFloat_AsDouble\0".as_ptr());
            let p_err_clear = ffi::GetProcAddress(h_py, b"PyErr_Clear\0".as_ptr());
            let p_decref = ffi::GetProcAddress(h_py, b"Py_DecRef\0".as_ptr());

            if p_is_init.is_null()
                || p_gil_ensure.is_null()
                || p_gil_release.is_null()
                || p_add_mod.is_null()
                || p_get_dict.is_null()
                || p_run_simple.is_null()
                || p_dict_get.is_null()
                || p_get_attr.is_null()
                || p_float_as_dbl.is_null()
                || p_err_clear.is_null()
                || p_decref.is_null()
            {
                return;
            }

            let py_is_init: PyIsInitializedFn = std::mem::transmute(p_is_init);
            let py_gil_ensure: PyGILStateEnsureFn = std::mem::transmute(p_gil_ensure);
            let py_gil_release: PyGILStateReleaseFn = std::mem::transmute(p_gil_release);
            let py_add_mod: PyImportAddModuleFn = std::mem::transmute(p_add_mod);
            let py_get_dict: PyModuleGetDictFn = std::mem::transmute(p_get_dict);
            let _py_run_simple: PyRunSimpleStringFn = std::mem::transmute(p_run_simple);
            let py_dict_get: PyDictGetItemStringFn = std::mem::transmute(p_dict_get);
            let py_get_attr: PyObjectGetAttrStringFn = std::mem::transmute(p_get_attr);
            let py_float_as_dbl: PyFloatAsDoubleFn = std::mem::transmute(p_float_as_dbl);
            let py_err_clear: PyErrClearFn = std::mem::transmute(p_err_clear);
            let py_decref: PyDecRefFn = std::mem::transmute(p_decref);

            // Thread-safe Python probe: NEVER call PySide6/Qt GUI APIs (`QApplication.topLevelWidgets()`)
            // from a non-GUI Rust background thread (violates Qt thread affinity). Only read an already-published
            // pure-Python `__main__._rinka_gui` attribute if present.
            let bus_b = get_dsp_rsb_bus_b_in();
            let mut gui_resolved = false;
            let mut gui_init_tries: u32 = 0;

            while run_flag.load(Ordering::Relaxed) && PY_PROBE_SESSION_GEN.load(Ordering::Relaxed) == session_gen {
                // Jika ORION aktif menyiarkan telemetri Shared Memory, JANGAN PERNAH menyentuh Python GIL
                // agar inferensi CUDA FP32 ORION berjalan 100% bebas gangguan!
                if read_orion_gpu_telemetry_shm_only().is_some() {
                    let sr = bus_b.layout().sample_rate.load(Ordering::Relaxed).max(44100);
                    bus_b.poll_orion_telemetry_realtime(sr);
                    std::thread::sleep(Duration::from_millis(80));
                    continue;
                }
                let cur_mode = bus_b.get_adaptive_mode();
                if cur_mode == 5 {
                    let sr = bus_b.layout().sample_rate.load(Ordering::Relaxed).max(44100);
                    bus_b.poll_orion_telemetry_realtime(sr);
                    let t = get_hybrid_telemetry();
                    let sw_med = t.sw_sensor_smp.load(Ordering::Acquire);
                    if sw_med > 0 {
                        bus_b.update_mode6_orion_sw_fusion(sw_med, sw_med, sw_med, 0, sr, 4);
                    }
                }
                let need_seed = bus_b.get_measured_gpu_stall_samples() == 0;
                if (cur_mode == 0 || cur_mode == 3 || cur_mode == 4 || need_seed)
                    && (gui_resolved || gui_init_tries < 2)
                    && py_is_init() != 0
                {
                    gui_init_tries += 1;
                    let gstate = py_gil_ensure();
                    if py_is_init() != 0 {
                        let main_mod = py_add_mod(b"__main__\0".as_ptr());
                        if !main_mod.is_null() {
                            let main_dict = py_get_dict(main_mod);
                            if !main_dict.is_null() {
                                let gui_obj = py_dict_get(main_dict, b"_rinka_gui\0".as_ptr());
                                if !gui_obj.is_null() {
                                    gui_resolved = true;
                                    let cp_obj =
                                        py_get_attr(gui_obj, b"callback_processor\0".as_ptr());
                                    if !cp_obj.is_null() {
                                        let t_obj = py_get_attr(
                                            cp_obj,
                                            b"current_inference_time\0".as_ptr(),
                                        );
                                        if !t_obj.is_null() {
                                            let ms_val = py_float_as_dbl(t_obj);
                                            py_decref(t_obj);
                                            if cur_mode == 0 || cur_mode == 3 || cur_mode == 4 {
                                                bus_b.set_direct_py_probe_active(true);
                                            } else {
                                                bus_b.set_direct_py_probe_active(false);
                                            }
                                            if ms_val > 0.0 && ms_val <= 2000.0 {
                                                let rounded_ms = (ms_val.round()) as f32;
                                                let sr = bus_b.layout().sample_rate.load(Ordering::Relaxed).max(44100) as f32;
                                                let py_smp = ((rounded_ms.clamp(0.5, 500.0) / 1000.0) * sr).round() as u32;
                                                get_hybrid_telemetry().py_sensor_smp.store(py_smp, Ordering::Release);

                                                if cur_mode == 0 || bus_b.get_measured_gpu_stall_samples() == 0 {
                                                    bus_b.set_direct_external_vc_gpu_delay_ms(rounded_ms);
                                                } else if cur_mode == 3 {
                                                    let t = get_hybrid_telemetry();
                                                    let sw_med = t.sw_sensor_smp.load(Ordering::Acquire);
                                                    if sw_med > 0 {
                                                        bus_b.update_mode4_hybrid_dual_fusion(
                                                            sw_med,
                                                            sw_med,
                                                            0,
                                                            sr as u32,
                                                            4,
                                                        );
                                                    } else {
                                                        bus_b.set_direct_external_vc_gpu_delay_ms(rounded_ms + 8.0);
                                                    }
                                                } else if cur_mode == 4 {
                                                    // Mode 5 (Predictive Preemptive PLC): dispatch to Mode 5 updater!
                                                    let t = get_hybrid_telemetry();
                                                    let sw_med = t.sw_sensor_smp.load(Ordering::Acquire);
                                                    if sw_med > 0 {
                                                        bus_b.update_mode5_predictive_preemptive_plc(
                                                            sw_med,
                                                            sw_med,
                                                            sw_med,
                                                            0,
                                                            sr as u32,
                                                            4,
                                                        );
                                                    } else {
                                                        bus_b.set_direct_external_vc_gpu_delay_ms(rounded_ms + 8.0);
                                                    }
                                                }
                                                if cur_mode == 0 {
                                                    let cur_pk = bus_b.get_stall_peak_samples();
                                                    if cur_pk > py_smp + 240 {
                                                        let next_pk = cur_pk.saturating_sub((cur_pk - py_smp) / 3).max(py_smp);
                                                        get_spike_shield_atomic().store(
                                                            ((next_pk as u64) << 32) | (now_epoch_ms() as u32 as u64),
                                                            Ordering::Release,
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                        py_decref(cp_obj);
                                    }
                                }
                                py_err_clear();
                            }
                        }
                    }
                    py_gil_release(gstate);
                }

                // Bangun cepat (<12ms) segera setelah callback chunk selesai (`PY_PROBE_WAKE_NOTIFY`),
                // dan pastikan tidak pernah mengambil GIL saat `buffer_switch` sedang berjalan (`in_flight_cb_start_ms != 0`)!
                for step in 0..16 {
                    if !run_flag.load(Ordering::Relaxed) || PY_PROBE_SESSION_GEN.load(Ordering::Relaxed) != session_gen {
                        return;
                    }
                    if step >= 1 && PY_PROBE_WAKE_NOTIFY.swap(false, Ordering::AcqRel) {
                        if get_hybrid_telemetry().in_flight_cb_start_ms.load(Ordering::Acquire) == 0 {
                            break;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(12));
                }
                while get_hybrid_telemetry().in_flight_cb_start_ms.load(Ordering::Acquire) != 0 {
                    if !run_flag.load(Ordering::Relaxed) || PY_PROBE_SESSION_GEN.load(Ordering::Relaxed) != session_gen {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(4));
                }
            }
        });
}

pub fn inject_probe_into_running_external_vc_if_needed() {
    // Dinonaktifkan agar tidak memicu konflik CUDA Graph capture (`CUDAGraphCaptureError`)
    // saat External VC melakukan Stop -> Start conversion.
}

unsafe extern "system" fn v_dsp_asio_start(this: *mut c_void) -> ASIOError {
    let obj = &*(this as *const RinkaVirtualAsioComObject);
    if obj.running.swap(true, Ordering::SeqCst) {
        return ASE_OK;
    }
    let cb_opt = *obj.callbacks.lock();
    let Some(cbs) = cb_opt else {
        obj.running.store(false, Ordering::SeqCst);
        return -1000;
    };

    obj.sample_pos.store(0, Ordering::Relaxed);
    #[cfg(windows)]
    let (init_cushion, saved_cpu_cores, saved_adap_cush, saved_adap_rep, saved_mult_pct, saved_mode, saved_extra_ms, saved_guard_ms) = {
        let s = read_saved_dsp_rsb_asio_reg_config();
        let eff_c = (s.cushion_samples as usize).min(24000);
        (eff_c, s.cpu_cores, s.adaptive_cushion, s.adaptive_reported, s.gpu_stall_mult_pct, s.adaptive_mode, s.adaptive_extra_ms, s.stall_guard_ms)
    };
    #[cfg(not(windows))]
    let (init_cushion, saved_cpu_cores, saved_adap_cush, saved_adap_rep, saved_mult_pct, saved_mode, saved_extra_ms, saved_guard_ms) =
        (2880usize, 2u16, true, true, 115u32, 3u8, 25.0f32, 15.0f32);

    let init_blk = obj.buffer_size.load(Ordering::Relaxed).clamp(32, 4096) as usize;
    // Pastikan BUS_DSP_A_OUT memiliki bantalan ~16..21ms (768..1024 sampel = 1 paket WASAPI 480 + 1 blok ASIO 256)
    // agar tidak pernah mengalami micro-underrun di sela-sela paket capture WASAPI 10ms!
    get_dsp_rsb_bus_a_out().reset_reader_with_cushion((init_blk * 3).clamp(768, 1200));
    let bus_b_init = get_dsp_rsb_bus_b_in();
    bus_b_init.reset_measured_gpu_stall();
    if bus_b_init.config_generation() <= 1 {
        bus_b_init.set_adaptive_flags_with_mult(
            saved_adap_cush,
            saved_adap_rep,
            (saved_mult_pct as f32) / 100.0,
        );
        bus_b_init.set_adaptive_mode(saved_mode);
        bus_b_init.set_user_extra_buffer_ms(saved_extra_ms);
        bus_b_init.set_user_stall_guard_ms(saved_guard_ms);
        bus_b_init.sync_config(
            (*obj.sample_rate.lock() as u32).clamp(44100, 192000),
            init_blk as u32,
            1,
            init_cushion as u32,
        );
    }
    let init_target = bus_b_init.get_dsp_b_params().0;
    if init_target > 0 {
        bus_b_init.reset_with_cushion(init_target);
    } else {
        bus_b_init.reset();
    }

    let session_gen = PY_PROBE_SESSION_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    #[cfg(windows)]
    spawn_external_python_gpu_delay_probe(Arc::clone(&obj.running), session_gen);

    let run_flag = Arc::clone(&obj.running);
    let this_addr = this as usize;

    let handle = std::thread::Builder::new()
        .name("rinka-dsp-rsb-asio-pump".into())
        .spawn(move || {
            boost_virtual_asio_pump_thread_with_cores(saved_cpu_cores);
            let obj_ref = &*(this_addr as *const RinkaVirtualAsioComObject);
            let infos_snapshot = obj_ref.buf_infos.lock().clone();
            let mut db_idx: i32 = 0;
            let mut clock_origin = Instant::now();
            let mut clock_frames: u64 = 0;
            let mut last_sr: u32 = (*obj_ref.sample_rate.lock() as u32).clamp(44100, 192000);
            let mut fade_in = true;
            let mut _was_catching_up = false;
            let mut window_voiced = false;
            let mut chunk_seq: u64 = 0;
            let mut last_chunk_instant = Instant::now();
            let mut win_frames: u64 = 0;
            let mut win_max_smp: u32 = 0;
            let mut cb_hist: [u32; 3] = [0; 3];
            let mut cb_hist_len: usize = 0;
            let mut in_l = vec![0.0f32; 4096];
            let mut in_r = vec![0.0f32; 4096];
            let mut out_l = vec![0.0f32; 4096];
            let mut out_r = vec![0.0f32; 4096];

            while run_flag.load(Ordering::Relaxed) {
                let vc_sr = (*obj_ref.sample_rate.lock() as u32).clamp(44100, 192000);
                if vc_sr != last_sr {
                    last_sr = vc_sr;
                    clock_origin = Instant::now();
                    clock_frames = 0;
                }
                let blk_len = obj_ref
                    .buffer_size
                    .load(Ordering::Relaxed)
                    .clamp(32, 4096) as usize;
                if out_l.len() < blk_len {
                    in_l.resize(blk_len, 0.0);
                    in_r.resize(blk_len, 0.0);
                    out_l.resize(blk_len, 0.0);
                    out_r.resize(blk_len, 0.0);
                }

                let bus_a = get_dsp_rsb_bus_a_out();
                let bus_b = get_dsp_rsb_bus_b_in();

                let target_time = clock_origin
                    + Duration::from_secs_f64((clock_frames as f64) / (vc_sr as f64));
                let now = Instant::now();
                let is_catching_up = if target_time > now {
                    sleep_high_res(target_time - now);
                    false
                } else {
                    let behind = now.duration_since(target_time);
                    let behind_us = behind.as_micros().min(u32::MAX as u128) as u32;
                    bus_b.note_pump_lag_us(behind_us);
                    if behind > Duration::from_millis(1200) {
                        bus_b.note_clock_reset();
                        bus_a.reset_reader_with_cushion((blk_len * 4).clamp(1200, 1920));
                        bus_b.layout().fade_in_pending.store(2, Ordering::Release);
                        clock_origin = now;
                        clock_frames = 0;
                        false
                    } else {
                        behind > Duration::from_millis(3)
                    }
                };

                // Strict SPSC v2.2: Never overwrite `bus_b.read_pos` from the writer pump thread!
                // Hardware-Crystal PLL Servo: gently nudge `clock_origin` by +/-20us when `bus_a` fill drifts
                // from `eff_in_cush` so CPU `Instant::now()` stays 100% phase-locked to the soundcard crystal
                // across multi-hour streaming sessions (>10 minutes to 24+ hours with zero drift)!
                _was_catching_up = is_catching_up;
                bus_b.set_pump_caught_up(!is_catching_up);

                let rsb_drv_mode = bus_a.get_rsb_driver_mode();
                let dsp_asio_allowed = rsb_drv_mode == 0 || rsb_drv_mode == 1;
                if dsp_asio_allowed && bus_a.is_writer_active() {
                    let eff_in_cush = (blk_len * 4).clamp(1024, 1536);
                    let mut avail_a = bus_a.available_to_read();
                    // Anti-Underrun Lock after 300ms GPU Callback Catch-Up Burst:
                    // If catch-up burst drained `bus_a` below `blk_len`, wait up to 5ms for Line A WASAPI
                    // to push the next capture block and re-anchor `clock_origin` so `bus_a` NEVER underruns!
                    if avail_a < blk_len {
                        bus_a.wait_for_write(5);
                        avail_a = bus_a.available_to_read();
                        if is_catching_up {
                            clock_origin = Instant::now();
                            clock_frames = 0;
                        }
                    }
                    if !is_catching_up {
                        if avail_a > eff_in_cush + blk_len * 2 {
                            clock_origin -= Duration::from_micros(22);
                        } else if avail_a + blk_len < eff_in_cush {
                            clock_origin += Duration::from_micros(22);
                        }
                    }
                    bus_a.read_stereo_asio_in_into_ex(
                        &mut in_l[..blk_len],
                        &mut in_r[..blk_len],
                        eff_in_cush,
                        is_catching_up,
                    );
                    for i in (0..blk_len).step_by(8) {
                        if in_l[i].abs() > 0.008 || in_r[i].abs() > 0.008 {
                            window_voiced = true;
                            break;
                        }
                    }
                } else {
                    in_l[..blk_len].fill(0.0);
                    in_r[..blk_len].fill(0.0);
                }

                for info in infos_snapshot.iter() {
                    if info.is_input != 0 {
                        let dst = info.buffers[db_idx as usize] as *mut f32;
                        if !dst.is_null() {
                            let src = if info.channel_num == 0 {
                                &in_l[..blk_len]
                            } else {
                                &in_r[..blk_len]
                            };
                            ptr::copy_nonoverlapping(src.as_ptr(), dst, blk_len);
                        }
                    }
                }

                get_hybrid_telemetry()
                    .in_flight_cb_start_ms
                    .store(now_epoch_ms(), Ordering::Release);
                let t_cb0 = Instant::now();
                (cbs.buffer_switch)(db_idx, 1);
                let last_cb_end = Instant::now();
                get_hybrid_telemetry()
                    .in_flight_cb_start_ms
                    .store(0, Ordering::Release);

                // Abaikan callback terakhir saat tombol Stop diklik (`!run_flag`) agar spike teardown
                // Python/PortAudio (~200ms) tidak pernah menimpa nilai GPU Delay External VC yang sebenarnya!
                if run_flag.load(Ordering::Relaxed) {
                    let cb_dur_secs = last_cb_end.duration_since(t_cb0).as_secs_f64();
                    let cb_us = (cb_dur_secs * 1_000_000.0).min(u32::MAX as f64) as u32;
                    let cb_smp = (cb_dur_secs * (vc_sr as f64)).round() as u32;

                    // Semua blok copy C++ PortAudio selesai dalam < 250 us (< 0.25 ms).
                    // Setiap panggilan >= 1500 us (1.5 ms) adalah callback inferensi chunk Python External VC
                    // (baik pada Buffer Size 64, 256, 512, maupun 1024 sampel!).
                    let min_chunk_smp = ((vc_sr * 8) / 10000).max(36);
                    let is_chunk_cb = cb_us >= 750 && cb_smp >= min_chunk_smp;
                    bus_b.note_pump_callback_us(cb_us, is_chunk_cb);

                    win_frames = win_frames.saturating_add(blk_len as u64);
                    if cb_smp > win_max_smp {
                        win_max_smp = cb_smp;
                    }

                    if is_chunk_cb {
                        trigger_py_probe_wakeup();
                        if last_chunk_instant.elapsed() > Duration::from_millis(1200) {
                            chunk_seq = 0;
                        }
                        last_chunk_instant = last_cb_end;
                        chunk_seq = chunk_seq.saturating_add(1);

                        // Jika cold-start warmup (`chunk_seq <= 2`) memakan waktu > 350ms, batasi lag maksimal
                        // pada consumer side (`bus_a` owned by this pump) saja — JANGAN sentuh `bus_b.read_pos`
                        // dari thread writer (Strict SPSC ownership: Line B render thread owns `bus_b.read_pos`)!
                        if chunk_seq <= 2 && cb_us >= 350_000 {
                            clock_origin = last_cb_end;
                            clock_frames = 0;
                            bus_a.reset_reader_with_cushion((blk_len * 3).clamp(768, 1200));
                        }

                        let active_mode = bus_b.get_adaptive_mode();
                        // Estimasi overhead non-GPU (Pedalboard/PortAudio wrapper ~5ms) untuk Mode 2 & Mode 3
                        let pa_overhead_smp = (vc_sr * 5) / 1000;
                        let net_cb_smp = if cb_smp > pa_overhead_smp + 96 {
                            cb_smp - pa_overhead_smp
                        } else {
                            cb_smp
                        };
                        match active_mode {
                            0 => {
                                // Mode 1 (Default): Hasil Coding & Perbaikan Claude (PySync + Flexible Spike Shield)
                                bus_b.pump_chunk_done(cb_smp, window_voiced, chunk_seq);
                            }
                            1 => {
                                // Mode 2: Pro ASIO Stopwatch + 3-Chunk Moving Median Outlier Filter
                                bus_b.set_direct_py_probe_active(false);
                                let s = if chunk_seq <= 2 {
                                    // Batasi spike cold-start chunk 1-2 maksimal 65ms
                                    net_cb_smp.min((vc_sr * 65) / 1000)
                                } else {
                                    net_cb_smp.min(24000)
                                };
                                if chunk_seq >= 2 {
                                    cb_hist[0] = cb_hist[1];
                                    cb_hist[1] = cb_hist[2];
                                    cb_hist[2] = s;
                                    cb_hist_len = (cb_hist_len + 1).min(3);
                                }
                                let med = if cb_hist_len < 3 {
                                    s
                                } else {
                                    let mut tmp = cb_hist;
                                    tmp.sort_unstable();
                                    tmp[1]
                                };
                                let instant_snap = chunk_seq <= 3 || bus_b.get_measured_gpu_stall_samples() == 0;
                                bus_b.update_measured_external_vc_gpu_delay(
                                    med,
                                    true,
                                    instant_snap,
                                );
                            }
                            2 => {
                                // Mode 3: Decoupled Pacing Ring Buffer (P95 Smooth Envelope + Breath-Pause Resync)
                                bus_b.set_direct_py_probe_active(false);
                                let s = if chunk_seq <= 2 {
                                    net_cb_smp.min((vc_sr * 65) / 1000)
                                } else {
                                    net_cb_smp.min(24000)
                                };
                                if chunk_seq >= 2 {
                                    cb_hist[0] = cb_hist[1];
                                    cb_hist[1] = cb_hist[2];
                                    cb_hist[2] = s;
                                    cb_hist_len = (cb_hist_len + 1).min(3);
                                }
                                let p95_est = if cb_hist_len < 3 {
                                    s
                                } else {
                                    let mut tmp = cb_hist;
                                    tmp.sort_unstable();
                                    (tmp[1] * 2 + tmp[2]) / 3
                                };
                                let instant_snap = chunk_seq <= 3 || bus_b.get_measured_gpu_stall_samples() == 0;
                                bus_b.update_measured_external_vc_gpu_delay(
                                    p95_est,
                                    true,
                                    instant_snap,
                                );
                            }
                            3 => {
                                // Mode 4: Hybrid Dual-Sensor Fusion (Gabungan Mode 1 [PySync] + Mode 2 [Stopwatch+Outlier])
                                let s = if chunk_seq <= 2 {
                                    net_cb_smp.min((vc_sr * 65) / 1000)
                                } else {
                                    net_cb_smp.min(24000)
                                };
                                if chunk_seq >= 2 {
                                    cb_hist[0] = cb_hist[1];
                                    cb_hist[1] = cb_hist[2];
                                    cb_hist[2] = s;
                                    cb_hist_len = (cb_hist_len + 1).min(3);
                                }
                                let (sw_med, sw_jitter) = if cb_hist_len < 3 {
                                    (s, 0)
                                } else {
                                    let mut tmp = cb_hist;
                                    tmp.sort_unstable();
                                    let d1 = cb_hist[2].abs_diff(cb_hist[1]);
                                    let d0 = cb_hist[1].abs_diff(cb_hist[0]);
                                    (tmp[1], (d1 + d0) / 2)
                                };
                                bus_b.update_mode4_hybrid_dual_fusion(
                                    s,
                                    sw_med,
                                    sw_jitter,
                                    vc_sr,
                                    chunk_seq,
                                );
                            }
                            4 => {
                                // Mode 5 (v2.0 Flagship): Predictive Pre-Emptive + Velocity Lookahead + Zero-Drop PLC
                                let s = if chunk_seq <= 2 {
                                    net_cb_smp.min((vc_sr * 65) / 1000)
                                } else {
                                    net_cb_smp.min(24000)
                                };
                                let prev_cb_smp = cb_hist[2];
                                if chunk_seq >= 2 {
                                    cb_hist[0] = cb_hist[1];
                                    cb_hist[1] = cb_hist[2];
                                    cb_hist[2] = s;
                                    cb_hist_len = (cb_hist_len + 1).min(3);
                                }
                                let (sw_med, sw_jitter) = if cb_hist_len < 3 {
                                    (s, 0)
                                } else {
                                    let mut tmp = cb_hist;
                                    tmp.sort_unstable();
                                    let d1 = cb_hist[2].abs_diff(cb_hist[1]);
                                    let d0 = cb_hist[1].abs_diff(cb_hist[0]);
                                    (tmp[1], (d1 + d0) / 2)
                                };
                                bus_b.update_mode5_predictive_preemptive_plc(
                                    s,
                                    prev_cb_smp,
                                    sw_med,
                                    sw_jitter,
                                    vc_sr,
                                    chunk_seq,
                                );
                            }
                            5 => {
                                // Mode 6 v2.1 (ORION Exclusive): Synchronized ORION IPC Telemetry + ASIO Stopwatch Fusion
                                let s = if chunk_seq <= 2 {
                                    net_cb_smp.min((vc_sr * 35) / 1000)
                                } else {
                                    net_cb_smp.min(24000)
                                };
                                let prev_cb_smp = cb_hist[2];
                                if chunk_seq >= 2 {
                                    cb_hist[0] = cb_hist[1];
                                    cb_hist[1] = cb_hist[2];
                                    cb_hist[2] = s;
                                    cb_hist_len = (cb_hist_len + 1).min(3);
                                }
                                let (sw_med, sw_jitter) = if cb_hist_len < 3 {
                                    (s, 0)
                                } else {
                                    let mut tmp = cb_hist;
                                    tmp.sort_unstable();
                                    let d1 = cb_hist[2].abs_diff(cb_hist[1]);
                                    let d0 = cb_hist[1].abs_diff(cb_hist[0]);
                                    (tmp[1], (d1 + d0) / 2)
                                };
                                bus_b.update_mode6_orion_sw_fusion(
                                    s,
                                    prev_cb_smp,
                                    sw_med,
                                    sw_jitter,
                                    vc_sr,
                                    chunk_seq,
                                );
                            }
                            _ => {
                                bus_b.pump_chunk_done(cb_smp, window_voiced, chunk_seq);
                            }
                        }
                        window_voiced = false;
                        win_frames = 0;
                        win_max_smp = 0;
                    } else if win_frames >= ((vc_sr as u64 * 320) / 1000) {
                        if win_max_smp >= ((vc_sr / 1000).max(48)) {
                            chunk_seq = chunk_seq.saturating_add(1);
                            match bus_b.get_adaptive_mode() {
                                0 => bus_b.pump_chunk_done(win_max_smp, window_voiced, chunk_seq),
                                3 => bus_b.update_mode4_hybrid_dual_fusion(
                                    win_max_smp.min(24000),
                                    win_max_smp.min(24000),
                                    0,
                                    vc_sr,
                                    chunk_seq,
                                ),
                                4 => bus_b.update_mode5_predictive_preemptive_plc(
                                    win_max_smp.min(24000),
                                    cb_hist[2],
                                    win_max_smp.min(24000),
                                    0,
                                    vc_sr,
                                    chunk_seq,
                                ),
                                5 => bus_b.update_mode6_orion_sw_fusion(
                                    win_max_smp.min(24000),
                                    cb_hist[2],
                                    win_max_smp.min(24000),
                                    0,
                                    vc_sr,
                                    chunk_seq,
                                ),
                                _ => {
                                    bus_b.set_direct_py_probe_active(false);
                                    let instant_snap = chunk_seq <= 3 || bus_b.get_measured_gpu_stall_samples() == 0;
                                    bus_b.update_measured_external_vc_gpu_delay(
                                        win_max_smp.min(24000),
                                        true,
                                        instant_snap,
                                    );
                                }
                            }
                        }
                        window_voiced = false;
                        win_frames = 0;
                        win_max_smp = 0;
                    }
                }

                out_l[..blk_len].fill(0.0);
                out_r[..blk_len].fill(0.0);
                let mut has_out_0 = false;
                let mut has_out_1 = false;
                for info in infos_snapshot.iter() {
                    if info.is_input == 0 {
                        let src = info.buffers[db_idx as usize] as *const f32;
                        if !src.is_null() {
                            if info.channel_num == 0 {
                                ptr::copy_nonoverlapping(src, out_l.as_mut_ptr(), blk_len);
                                has_out_0 = true;
                            } else if info.channel_num == 1 {
                                ptr::copy_nonoverlapping(src, out_r.as_mut_ptr(), blk_len);
                                has_out_1 = true;
                            }
                        }
                    }
                }
                if has_out_0
                    && (!has_out_1 || out_r[..blk_len].iter().all(|&x| x.abs() < 1e-7))
                {
                    out_r[..blk_len].copy_from_slice(&out_l[..blk_len]);
                }
                if fade_in {
                    fade_in = false;
                    let fade_n = blk_len.min(64);
                    for i in 0..fade_n {
                        let t = (i + 1) as f32 / (fade_n as f32);
                        let w = 0.5 * (1.0 - (std::f32::consts::PI * t).cos());
                        out_l[i] *= w;
                        out_r[i] *= w;
                    }
                }
                if dsp_asio_allowed {
                    bus_a.mark_vc_heartbeat(true);
                    bus_b.mark_vc_heartbeat(true);
                    if has_out_0 || has_out_1 {
                        let (cush_target, _) = bus_b.get_dsp_b_params();
                        bus_b.write_stereo_dsp_b(&out_l[..blk_len], &out_r[..blk_len], cush_target);
                    }
                }

                clock_frames = clock_frames.wrapping_add(blk_len as u64);
                obj_ref.sample_pos.fetch_add(blk_len as u64, Ordering::Relaxed);
                db_idx ^= 1;
            }
        });

    match handle {
        Ok(h) => {
            *obj.worker.lock() = Some(h);
            ASE_OK
        }
        Err(_) => {
            obj.running.store(false, Ordering::SeqCst);
            -1000
        }
    }
}

static DSP_RSB_ASIO_VTBL: IASIOVtbl = IASIOVtbl {
    query_interface: v_asio_qi,
    add_ref: v_asio_add_ref,
    release: v_asio_release,
    init: v_dsp_asio_init,
    get_driver_name: v_dsp_asio_get_driver_name,
    get_driver_version: v_asio_get_driver_version,
    get_error_message: v_asio_get_error_message,
    start: v_dsp_asio_start,
    stop: v_asio_stop,
    get_channels: v_asio_get_channels,
    get_latencies: v_dsp_asio_get_latencies,
    get_buffer_size: v_dsp_asio_get_buffer_size,
    can_sample_rate: v_dsp_asio_can_sample_rate,
    get_sample_rate: v_dsp_asio_get_sample_rate,
    set_sample_rate: v_dsp_asio_set_sample_rate,
    get_clock_sources: v_asio_get_clock_sources,
    set_clock_source: v_asio_set_clock_source,
    get_sample_position: v_asio_get_sample_position,
    get_channel_info: v_dsp_asio_get_channel_info,
    create_buffers: v_asio_create_buffers,
    dispose_buffers: v_asio_dispose_buffers,
    control_panel: v_asio_control_panel,
    future: v_asio_future,
    output_ready: v_asio_output_ready,
};

unsafe extern "system" fn cf_dsp_create_instance(
    _this: *mut c_void,
    outer: *mut c_void,
    _riid: *const ffi::GUID,
    ppv: *mut *mut c_void,
) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if ppv.is_null() {
            return -2147467261;
        }
        if !outer.is_null() {
            *ppv = ptr::null_mut();
            return -2147221232; // CLASS_E_NOAGGREGATION
        }
        #[cfg(windows)]
        let (sr, smp) = {
            let saved = read_saved_dsp_rsb_asio_reg_config();
            (saved.sample_rate, saved.preferred_smp)
        };
        #[cfg(not(windows))]
        let (sr, smp) = {
            let bus = get_dsp_rsb_bus_a_out();
            let (s, p, _, _) = bus.get_config();
            (s, p)
        };
        let bus = get_dsp_rsb_bus_a_out();
        let obj = Box::into_raw(Box::new(RinkaVirtualAsioComObject {
            vtbl: &DSP_RSB_ASIO_VTBL,
            ref_count: AtomicU32::new(1),
            inner_asio: Mutex::new(ptr::null_mut()),
            last_config_gen: AtomicU32::new(bus.config_generation()),
            sample_rate: Mutex::new(sr as f64),
            buffer_size: AtomicI32::new(smp.clamp(32, 2048) as i32),
            sample_pos: AtomicU64::new(0),
            running: Arc::new(AtomicBool::new(false)),
            stop_lock: Mutex::new(()),
            worker: Mutex::new(None),
            standalone_pair: Mutex::new(None),
            buf_infos: Mutex::new(Vec::new()),
            channel_storage: Mutex::new(Vec::new()),
            callbacks: Mutex::new(None),
        }));
        *ppv = obj as *mut c_void;
        0 // S_OK
    }))
    .unwrap_or(-2147467259)
}

static CF_DSP_VTBL: IClassFactoryVtbl = IClassFactoryVtbl {
    query_interface: cf_qi,
    add_ref: cf_add_ref,
    release: cf_release,
    create_instance: cf_dsp_create_instance,
    lock_server: cf_lock_server,
};

static FACTORY_DSP_RSB_ASIO: RinkaAsioClassFactory = RinkaAsioClassFactory {
    vtbl: &CF_DSP_VTBL,
};

// ==============================================================================
// DRIVER BUILT-IN FLEXASIO MANDIRI: "RSB ASIO" (CLSID_RSB_ASIO)
// ==============================================================================
// - Sama persis stabilitas, fitur, dan perilaku buffer-nya seperti FlexASIO standar,
//   tetapi terintegrasi penuh di dalam Rinka SoundBridge dengan nama `"RSB ASIO"`
//   sehingga tidak pernah bentrok/menimpa FlexASIO eksternal dan tidak butuh Virtual Cable!

unsafe extern "system" fn v_rsb_flex_asio_init(this: *mut c_void, _sys_handle: *mut c_void) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if this.is_null() {
            return 0;
        }
        let obj = &*(this as *const RinkaVirtualAsioComObject);
        #[cfg(windows)]
        {
            let saved = read_saved_rsb_flex_asio_reg_config();
            let (rep_in_smp, rep_out_smp) = compute_rsb_flexasio_reported_latency_samples(
                saved.sample_rate,
                saved.preferred_smp,
                saved.set_in_enabled,
                saved.in_latency_ms,
                saved.set_out_enabled,
                saved.out_latency_ms,
            );
            let bus_a = get_dsp_rsb_bus_a_out();
            bus_a.sync_config(
                saved.sample_rate,
                saved.preferred_smp,
                rep_in_smp,
                rep_out_smp,
            );
            let in_extra_smp = if saved.set_in_enabled && saved.in_latency_ms > 0.0 {
                (((saved.in_latency_ms.clamp(0.0, 1000.0) as f64) / 1000.0) * (saved.sample_rate as f64)).round() as u32
            } else {
                0
            };
            let out_extra_smp = if saved.set_out_enabled && saved.out_latency_ms > 0.0 {
                (((saved.out_latency_ms.clamp(0.0, 1000.0) as f64) / 1000.0) * (saved.sample_rate as f64)).round() as u32
            } else {
                0
            };
            let base_24ms_smp = (((0.028 * (saved.sample_rate as f64)).round() as u32) + out_extra_smp + (in_extra_smp / 2)).clamp(1440, 48000);
            let bus_b = get_dsp_rsb_bus_b_in();
            bus_b.set_adaptive_flags_with_mult(true, true, 1.15);
            bus_b.set_adaptive_mode(4);
            bus_b.set_user_extra_buffer_ms(0.0);
            bus_b.set_user_stall_guard_ms(15.0);
            bus_b.sync_config(
                saved.sample_rate,
                saved.preferred_smp,
                1,
                base_24ms_smp,
            );
        }
        let bus = get_dsp_rsb_bus_a_out();
        let (sr, smp, _, _) = bus.get_config();
        *obj.sample_rate.lock() = sr as f64;
        obj.buffer_size.store(smp.clamp(32, 2048) as i32, Ordering::Relaxed);
        obj.last_config_gen.store(bus.config_generation(), Ordering::Relaxed);
        obj.sample_pos.store(0, Ordering::Relaxed);
        1 // ASIOTrue
    }))
    .unwrap_or(0)
}

unsafe extern "system" fn v_rsb_flex_asio_get_driver_name(_this: *mut c_void, name: *mut c_char) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if name.is_null() {
            return;
        }
        let s: &[u8] = &b"RSB ASIO\0"[..];
        ptr::copy_nonoverlapping(s.as_ptr() as *const c_char, name, s.len());
    }));
}

unsafe extern "system" fn v_rsb_flex_asio_get_channel_info(
    _this: *mut c_void,
    info: *mut ASIOChannelInfo,
) -> ASIOError {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if info.is_null() {
            return -1000;
        }
        let inf = &mut *info;
        inf.is_active = 1;
        inf.channel_group = 0;
        inf.sample_type = ASIOST_FLOAT32_LSB;
        inf.name = [0; 32];
        let label: &[u8] = if inf.is_input != 0 {
            if inf.channel == 0 {
                &b"RSB Out A L (In VC)\0"[..]
            } else {
                &b"RSB Out A R (In VC)\0"[..]
            }
        } else if inf.channel == 0 {
            &b"RSB Loopback B L\0"[..]
        } else {
            &b"RSB Loopback B R\0"[..]
        };
        ptr::copy_nonoverlapping(
            label.as_ptr() as *const c_char,
            inf.name.as_mut_ptr(),
            label.len().min(31),
        );
        ASE_OK
    }))
    .unwrap_or(-1000)
}

unsafe extern "system" fn v_rsb_flex_asio_get_buffer_size(
    this: *mut c_void,
    min_size: *mut i32,
    max_size: *mut i32,
    preferred_size: *mut i32,
    granularity: *mut i32,
) -> ASIOError {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if this.is_null() {
            return -1000;
        }
        let obj = &*(this as *const RinkaVirtualAsioComObject);
        #[cfg(windows)]
        let (sr, pref_smp) = {
            let saved = read_saved_rsb_flex_asio_reg_config();
            (saved.sample_rate, saved.preferred_smp)
        };
        #[cfg(not(windows))]
        let (sr, pref_smp) = {
            let bus = get_dsp_rsb_bus_a_out();
            let (s, p, _, _) = bus.get_config();
            (s, p)
        };
        *obj.sample_rate.lock() = sr as f64;
        let locked_smp = pref_smp.clamp(32, 2048) as i32;
        if !obj.running.load(Ordering::Relaxed) && obj.channel_storage.lock().is_empty() {
            obj.buffer_size.store(locked_smp, Ordering::Relaxed);
        }
        if !min_size.is_null() {
            *min_size = locked_smp;
        }
        if !max_size.is_null() {
            *max_size = locked_smp;
        }
        if !preferred_size.is_null() {
            *preferred_size = locked_smp;
        }
        if !granularity.is_null() {
            *granularity = 0;
        }
        ASE_OK
    }))
    .unwrap_or(-1000)
}

unsafe extern "system" fn v_rsb_flex_asio_get_latencies(
    this: *mut c_void,
    input_lat: *mut i32,
    output_lat: *mut i32,
) -> ASIOError {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if this.is_null() {
            return -1000;
        }
        let obj = &*(this as *const RinkaVirtualAsioComObject);
        let blk = obj.buffer_size.load(Ordering::Relaxed).clamp(32, 4096) as u32;
        #[cfg(windows)]
        let (in_smp, out_smp) = {
            let saved = read_saved_rsb_flex_asio_reg_config();
            compute_rsb_flexasio_reported_latency_samples(
                saved.sample_rate,
                blk,
                saved.set_in_enabled,
                saved.in_latency_ms,
                saved.set_out_enabled,
                saved.out_latency_ms,
            )
        };
        #[cfg(not(windows))]
        let (in_smp, out_smp) = (blk + 9600, blk * 2 + 9600);
        if !input_lat.is_null() {
            *input_lat = in_smp as i32;
        }
        if !output_lat.is_null() {
            *output_lat = out_smp as i32;
        }
        ASE_OK
    }))
    .unwrap_or(-1000)
}

unsafe extern "system" fn v_rsb_flex_asio_start(this: *mut c_void) -> ASIOError {
    let obj = &*(this as *const RinkaVirtualAsioComObject);
    if obj.running.swap(true, Ordering::SeqCst) {
        return ASE_OK;
    }
    let cb_opt = *obj.callbacks.lock();
    let Some(cbs) = cb_opt else {
        obj.running.store(false, Ordering::SeqCst);
        return -1000;
    };

    obj.sample_pos.store(0, Ordering::Relaxed);

    #[cfg(windows)]
    let (in_cush_smp, out_cush_smp, saved_sr) = {
        let s = read_saved_rsb_flex_asio_reg_config();
        let sr_f = s.sample_rate.max(44100) as f64;
        let _base_24ms = (0.024 * sr_f).round() as usize;
        let in_extra = if s.set_in_enabled && s.in_latency_ms > 0.0 {
            (((s.in_latency_ms.clamp(0.0, 1000.0) as f64) / 1000.0) * sr_f).round() as usize
        } else {
            0
        };
        let out_extra = if s.set_out_enabled && s.out_latency_ms > 0.0 {
            (((s.out_latency_ms.clamp(0.0, 1000.0) as f64) / 1000.0) * sr_f).round() as usize
        } else {
            0
        };
        let base_50ms = (0.050 * sr_f).round() as usize;
        (
            1440usize,
            (base_50ms + out_extra + in_extra / 2).clamp(2400, 24000),
            s.sample_rate.clamp(44100, 192000),
        )
    };
    #[cfg(not(windows))]
    let (in_cush_smp, out_cush_smp, saved_sr) = (1440usize, 2400usize, 48000u32);

    let init_blk = obj.buffer_size.load(Ordering::Relaxed).clamp(32, 4096) as usize;
    let eff_in_cush = in_cush_smp.max(init_blk * 4).clamp(1200, 1920);

    // 1. Hanya gunakan standalone buses jika RinkaSoundBridge.exe sama sekali tidak aktif
    //    (`config_generation() <= 1` dan `!is_writer_active()`). Selama RinkaSoundBridge.exe berjalan,
    //    SELALU gunakan `BUS_DSP_A_OUT` & `BUS_DSP_B_IN` (identik dengan SoundBridge ASIO)!
    let mut use_standalone_buses = false;
    #[cfg(windows)]
    {
        let saved = read_saved_rsb_flex_asio_reg_config();
        let bus_a = get_dsp_rsb_bus_a_out();
        let bus_a_active = bus_a.is_writer_active() || bus_a.config_generation() > 1;
        if !bus_a_active && (!saved.input_device.is_empty() || !saved.output_device.is_empty()) {
            let vc_sr = saved_sr;
            *obj.sample_rate.lock() = vc_sr as f64;
            let in_tgt_opt = if saved.input_device.is_empty() {
                None
            } else {
                Some(WasapiEndpointTarget::ByName(saved.input_device.clone()))
            };
            let out_tgt_opt = if saved.output_device.is_empty() {
                None
            } else {
                Some(WasapiEndpointTarget::ByName(saved.output_device.clone()))
            };

            if in_tgt_opt.is_some() || out_tgt_opt.is_some() {
                let st_bus_a = get_rsb_standalone_bus_a_out();
                let st_bus_b = get_rsb_standalone_bus_b_in();
                st_bus_a.sync_config(vc_sr, init_blk as u32, eff_in_cush as u32, out_cush_smp as u32);
                st_bus_a.reset_reader_with_cushion(eff_in_cush);
                st_bus_b.reset_measured_gpu_stall();
                st_bus_b.set_adaptive_flags_with_mult(true, true, 1.05);
                st_bus_b.set_adaptive_mode(3);
                st_bus_b.set_user_extra_buffer_ms(0.0);
                st_bus_b.set_user_stall_guard_ms(11.0);
                st_bus_b.sync_config(vc_sr, init_blk as u32, 1, out_cush_smp as u32);
                st_bus_b.reset_with_cushion(out_cush_smp);

                let set_in_ms = if saved.set_in_enabled && saved.in_latency_ms > 0.0 {
                    Some(saved.in_latency_ms.clamp(0.0, 1000.0))
                } else {
                    Some(0.0)
                };
                let set_out_ms = if saved.set_out_enabled && saved.out_latency_ms > 0.0 {
                    Some(saved.out_latency_ms.clamp(0.0, 1000.0))
                } else {
                    Some(0.0)
                };

                let in_opts = FlexAsioEndpointOptions {
                    set_latency_ms: set_in_ms,
                    wasapi_set: saved.wasapi_in_set,
                    wasapi_exclusive: saved.wasapi_in_exclusive,
                    wasapi_autoconvert: saved.wasapi_in_autoconvert,
                    channels: saved.in_channels,
                };
                let out_opts = FlexAsioEndpointOptions {
                    set_latency_ms: set_out_ms,
                    wasapi_set: saved.wasapi_out_set,
                    wasapi_exclusive: saved.wasapi_out_exclusive,
                    wasapi_autoconvert: saved.wasapi_out_autoconvert,
                    channels: saved.out_channels,
                };

                let mut cap_l = vec![0.0f32; 4096];
                let mut cap_r = vec![0.0f32; 4096];
                let mut ren_scratch_l = vec![0.0f32; 4096];
                let mut ren_scratch_r = vec![0.0f32; 4096];

                if let Ok(pair) = WasapiFlexAsioPair::try_start_with_options(
                    in_tgt_opt,
                    out_tgt_opt,
                    vc_sr,
                    init_blk as u32,
                    in_opts,
                    out_opts,
                    move |data, ch, frames| {
                        if cap_l.len() < frames {
                            cap_l.resize(frames, 0.0);
                            cap_r.resize(frames, 0.0);
                        }
                        if ch == 1 {
                            for (i, &s) in data.iter().take(frames).enumerate() {
                                cap_l[i] = s;
                                cap_r[i] = s;
                            }
                        } else {
                            for (i, chunk) in data.chunks(ch.max(1)).take(frames).enumerate() {
                                let l = chunk[0];
                                let r = *chunk.get(1).unwrap_or(&l);
                                cap_l[i] = l;
                                cap_r[i] = r;
                            }
                        }
                        get_rsb_standalone_bus_a_out().write_stereo(&cap_l[..frames], &cap_r[..frames], 48000);
                    },
                    move |out_data, ch, frames| {
                        if ren_scratch_l.len() < frames {
                            ren_scratch_l.resize(frames, 0.0);
                            ren_scratch_r.resize(frames, 0.0);
                        }
                        get_rsb_standalone_bus_b_in().read_stereo_rsb_flex_b_into(
                            &mut ren_scratch_l[..frames],
                            &mut ren_scratch_r[..frames],
                            out_cush_smp,
                        );
                        if ch == 1 {
                            for i in 0..frames {
                                out_data[i] = 0.5 * (ren_scratch_l[i] + ren_scratch_r[i]);
                            }
                        } else {
                            for (i, chunk) in out_data.chunks_mut(ch.max(1)).take(frames).enumerate() {
                                chunk[0] = ren_scratch_l[i];
                                if chunk.len() > 1 {
                                    chunk[1] = ren_scratch_r[i];
                                }
                                for extra in &mut chunk[2..] {
                                    *extra = 0.0;
                                }
                            }
                        }
                    },
                ) {
                    *obj.standalone_pair.lock() = Some(pair);
                    use_standalone_buses = true;
                }
            }
        }
    }

    if !use_standalone_buses {
        let bus_a_init = get_dsp_rsb_bus_a_out();
        bus_a_init.reset_reader_with_cushion(eff_in_cush);
        let bus_b_init = get_dsp_rsb_bus_b_in();
        bus_b_init.reset_measured_gpu_stall();
        bus_b_init.set_adaptive_flags_with_mult(true, true, 1.05);
        bus_b_init.set_adaptive_mode(3);
        bus_b_init.set_user_extra_buffer_ms(0.0);
        bus_b_init.set_user_stall_guard_ms(11.0);
        bus_b_init.sync_config(saved_sr, init_blk as u32, 1, out_cush_smp as u32);
        bus_b_init.reset_with_cushion(out_cush_smp);
    }

    let session_gen = PY_PROBE_SESSION_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    #[cfg(windows)]
    spawn_external_python_gpu_delay_probe(Arc::clone(&obj.running), session_gen);

    let run_flag = Arc::clone(&obj.running);
    let this_addr = this as usize;

    let handle = std::thread::Builder::new()
        .name("rinka-rsb-flex-asio-pump".into())
        .spawn(move || {
            boost_virtual_asio_pump_thread();
            let obj_ref = &*(this_addr as *const RinkaVirtualAsioComObject);
            let infos_snapshot = obj_ref.buf_infos.lock().clone();
            let mut db_idx: i32 = 0;
            let mut clock_origin = Instant::now();
            let mut clock_frames: u64 = 0;
            let mut last_sr: u32 = (*obj_ref.sample_rate.lock() as u32).clamp(44100, 192000);
            let mut fade_in = true;
            let mut chunk_seq: u64 = 0;
            let mut last_chunk_instant = Instant::now();
            let mut cb_hist: [u32; 3] = [0; 3];
            let mut cb_hist_len: usize = 0;
            let mut in_l = vec![0.0f32; 4096];
            let mut in_r = vec![0.0f32; 4096];
            let mut out_l = vec![0.0f32; 4096];
            let mut out_r = vec![0.0f32; 4096];

            while run_flag.load(Ordering::Relaxed) {
                let vc_sr = (*obj_ref.sample_rate.lock() as u32).clamp(44100, 192000);
                if vc_sr != last_sr {
                    last_sr = vc_sr;
                    clock_origin = Instant::now();
                    clock_frames = 0;
                }
                let blk_len = obj_ref
                    .buffer_size
                    .load(Ordering::Relaxed)
                    .clamp(32, 4096) as usize;
                if out_l.len() < blk_len {
                    in_l.resize(blk_len, 0.0);
                    in_r.resize(blk_len, 0.0);
                    out_l.resize(blk_len, 0.0);
                    out_r.resize(blk_len, 0.0);
                }

                let (bus_a, bus_b) = if use_standalone_buses {
                    (get_rsb_standalone_bus_a_out(), get_rsb_standalone_bus_b_in())
                } else {
                    (get_dsp_rsb_bus_a_out(), get_dsp_rsb_bus_b_in())
                };

                // High-Resolution Sample-Clocked Pump dengan Cushion Lock (`eff_in_cush`):
                // Menjaga agar `bus_a` TIDAK PERNAH terkuras habis ke 0 sampel di awal stream,
                // sehingga ketika plugin Noise Suppression (Clear / Clarity Vx / RNNoise) di Line A atau External VC
                // membutuhkan 15..80ms untuk memproses frame neural, bantalan `eff_in_cush` (mis. 100ms)
                // selalu tersedia penuh tanpa pernah mengalami micro-underrun atau burst liar!
                let target_time = clock_origin
                    + Duration::from_secs_f64((clock_frames as f64) / (vc_sr as f64));
                let now = Instant::now();
                let is_catching_up = if target_time > now {
                    sleep_high_res(target_time - now);
                    false
                } else {
                    let behind = now.duration_since(target_time);
                    let behind_us = behind.as_micros().min(u32::MAX as u128) as u32;
                    bus_b.note_pump_lag_us(behind_us);
                    if behind > Duration::from_millis(1200) {
                        bus_b.note_clock_reset();
                        bus_a.reset_reader_with_cushion(eff_in_cush);
                        bus_b.layout().fade_in_pending.store(1, Ordering::Release);
                        clock_origin = now;
                        clock_frames = 0;
                        false
                    } else {
                        behind > Duration::from_millis(3)
                    }
                };
                bus_b.set_pump_caught_up(!is_catching_up);

                let rsb_drv_mode_flex = bus_a.get_rsb_driver_mode();
                let flex_asio_allowed = use_standalone_buses || rsb_drv_mode_flex == 0 || rsb_drv_mode_flex == 2;
                if flex_asio_allowed && bus_a.is_writer_active() {
                    let mut avail_a = bus_a.available_to_read();
                    if avail_a < blk_len {
                        bus_a.wait_for_write(5);
                        avail_a = bus_a.available_to_read();
                        if is_catching_up {
                            clock_origin = Instant::now();
                            clock_frames = 0;
                        }
                    }
                    if !is_catching_up {
                        if avail_a > eff_in_cush + blk_len * 2 {
                            clock_origin -= Duration::from_micros(22);
                        } else if avail_a + blk_len < eff_in_cush {
                            clock_origin += Duration::from_micros(22);
                        }
                    }
                    bus_a.read_stereo_asio_in_into_ex(
                        &mut in_l[..blk_len],
                        &mut in_r[..blk_len],
                        eff_in_cush,
                        is_catching_up,
                    );
                } else {
                    in_l[..blk_len].fill(0.0);
                    in_r[..blk_len].fill(0.0);
                }

                for info in infos_snapshot.iter() {
                    if info.is_input != 0 {
                        let dst = info.buffers[db_idx as usize] as *mut f32;
                        if !dst.is_null() {
                            let src = if info.channel_num == 0 {
                                &in_l[..blk_len]
                            } else {
                                &in_r[..blk_len]
                            };
                            ptr::copy_nonoverlapping(src.as_ptr(), dst, blk_len);
                        }
                    }
                }

                get_hybrid_telemetry()
                    .in_flight_cb_start_ms
                    .store(now_epoch_ms(), Ordering::Release);
                let t_cb0 = Instant::now();
                (cbs.buffer_switch)(db_idx, 1);
                let last_cb_end = Instant::now();
                get_hybrid_telemetry()
                    .in_flight_cb_start_ms
                    .store(0, Ordering::Release);

                if run_flag.load(Ordering::Relaxed) {
                    let cb_dur_secs = last_cb_end.duration_since(t_cb0).as_secs_f64();
                    let cb_us = (cb_dur_secs * 1_000_000.0).min(u32::MAX as f64) as u32;
                    let cb_smp = (cb_dur_secs * (vc_sr as f64)).round() as u32;
                    let min_chunk_smp = ((vc_sr * 8) / 10000).max(36);
                    let is_chunk_cb = cb_us >= 750 && cb_smp >= min_chunk_smp;
                    bus_b.note_pump_callback_us(cb_us, is_chunk_cb);

                    if is_chunk_cb {
                        trigger_py_probe_wakeup();
                        if last_chunk_instant.elapsed() > Duration::from_millis(1200) {
                            chunk_seq = 0;
                        }
                        last_chunk_instant = last_cb_end;
                        chunk_seq = chunk_seq.saturating_add(1);
                        let pa_overhead_smp = (vc_sr * 5) / 1000;
                        let net_cb_smp = if cb_smp > pa_overhead_smp + 96 {
                            cb_smp - pa_overhead_smp
                        } else {
                            cb_smp
                        };
                        let s_val = if chunk_seq <= 2 {
                            net_cb_smp.min((vc_sr * 85) / 1000)
                        } else {
                            net_cb_smp.min(36000)
                        };
                        let prev_cb_smp = cb_hist[2];
                        if chunk_seq >= 2 {
                            cb_hist[0] = cb_hist[1];
                            cb_hist[1] = cb_hist[2];
                            cb_hist[2] = s_val;
                            cb_hist_len = (cb_hist_len + 1).min(3);
                        }
                        let (sw_med, sw_jitter) = if cb_hist_len < 3 {
                            (s_val, 0)
                        } else {
                            let mut tmp = cb_hist;
                            tmp.sort_unstable();
                            let d1 = cb_hist[2].abs_diff(cb_hist[1]);
                            let d0 = cb_hist[1].abs_diff(cb_hist[0]);
                            (tmp[1], (d1 + d0) / 2)
                        };
                        if bus_b.get_adaptive_mode() == 5 || read_orion_gpu_telemetry_shm_only().is_some() {
                            bus_b.update_mode6_orion_sw_fusion(
                                s_val,
                                prev_cb_smp,
                                sw_med,
                                sw_jitter,
                                vc_sr,
                                chunk_seq,
                            );
                        } else {
                            bus_b.update_mode5_predictive_preemptive_plc(
                                s_val,
                                prev_cb_smp,
                                sw_med,
                                sw_jitter,
                                vc_sr,
                                chunk_seq,
                            );
                        }
                    }
                }

                out_l[..blk_len].fill(0.0);
                out_r[..blk_len].fill(0.0);
                let mut has_out_0 = false;
                let mut has_out_1 = false;
                for info in infos_snapshot.iter() {
                    if info.is_input == 0 {
                        let src = info.buffers[db_idx as usize] as *const f32;
                        if !src.is_null() {
                            if info.channel_num == 0 {
                                ptr::copy_nonoverlapping(src, out_l.as_mut_ptr(), blk_len);
                                has_out_0 = true;
                            } else if info.channel_num == 1 {
                                ptr::copy_nonoverlapping(src, out_r.as_mut_ptr(), blk_len);
                                has_out_1 = true;
                            }
                        }
                    }
                }
                if has_out_0
                    && (!has_out_1 || out_r[..blk_len].iter().all(|&x| x.abs() < 1e-7))
                {
                    out_r[..blk_len].copy_from_slice(&out_l[..blk_len]);
                }
                if fade_in {
                    fade_in = false;
                    let fade_n = blk_len.min(64);
                    for i in 0..fade_n {
                        let t = (i + 1) as f32 / (fade_n as f32);
                        let w = 0.5 * (1.0 - (std::f32::consts::PI * t).cos());
                        out_l[i] *= w;
                        out_r[i] *= w;
                    }
                }
                if flex_asio_allowed {
                    bus_a.mark_vc_heartbeat(true);
                    bus_b.mark_vc_heartbeat(true);
                    if has_out_0 || has_out_1 {
                        let (c_target, _) = bus_b.get_dsp_b_params();
                        bus_b.write_stereo_dsp_b(&out_l[..blk_len], &out_r[..blk_len], c_target.max(out_cush_smp));
                    }
                }

                clock_frames = clock_frames.wrapping_add(blk_len as u64);
                obj_ref.sample_pos.fetch_add(blk_len as u64, Ordering::Relaxed);
                db_idx ^= 1;
            }
        });

    match handle {
        Ok(h) => {
            *obj.worker.lock() = Some(h);
            ASE_OK
        }
        Err(_) => {
            obj.running.store(false, Ordering::SeqCst);
            -1000
        }
    }
}

unsafe extern "system" fn v_rsb_flex_asio_can_sample_rate(
    _this: *mut c_void,
    sample_rate: f64,
) -> ASIOError {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        #[cfg(windows)]
        let bus_sr = read_saved_rsb_flex_asio_reg_config().sample_rate;
        #[cfg(not(windows))]
        let (bus_sr, _, _, _) = get_dsp_rsb_bus_a_out().get_config();
        if (sample_rate - (bus_sr as f64)).abs() < 1.0 {
            ASE_OK
        } else {
            ASE_NO_CLOCK
        }
    }))
    .unwrap_or(-1000)
}

unsafe extern "system" fn v_rsb_flex_asio_get_sample_rate(
    this: *mut c_void,
    sample_rate: *mut f64,
) -> ASIOError {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if this.is_null() {
            return -1000;
        }
        let obj = &*(this as *const RinkaVirtualAsioComObject);
        #[cfg(windows)]
        let bus_sr = read_saved_rsb_flex_asio_reg_config().sample_rate;
        #[cfg(not(windows))]
        let (bus_sr, _, _, _) = get_dsp_rsb_bus_a_out().get_config();
        *obj.sample_rate.lock() = bus_sr as f64;
        if !sample_rate.is_null() {
            *sample_rate = bus_sr as f64;
        }
        ASE_OK
    }))
    .unwrap_or(-1000)
}

unsafe extern "system" fn v_rsb_flex_asio_set_sample_rate(
    this: *mut c_void,
    sample_rate: f64,
) -> ASIOError {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if this.is_null() {
            return -1000;
        }
        let obj = &*(this as *const RinkaVirtualAsioComObject);
        #[cfg(windows)]
        let bus_sr = read_saved_rsb_flex_asio_reg_config().sample_rate;
        #[cfg(not(windows))]
        let (bus_sr, _, _, _) = get_dsp_rsb_bus_a_out().get_config();
        if (sample_rate - (bus_sr as f64)).abs() < 1.0 {
            *obj.sample_rate.lock() = bus_sr as f64;
            ASE_OK
        } else {
            ASE_NO_CLOCK
        }
    }))
    .unwrap_or(-1000)
}

static RSB_FLEX_ASIO_VTBL: IASIOVtbl = IASIOVtbl {
    query_interface: v_asio_qi,
    add_ref: v_asio_add_ref,
    release: v_asio_release,
    init: v_rsb_flex_asio_init,
    get_driver_name: v_rsb_flex_asio_get_driver_name,
    get_driver_version: v_asio_get_driver_version,
    get_error_message: v_asio_get_error_message,
    start: v_rsb_flex_asio_start,
    stop: v_asio_stop,
    get_channels: v_asio_get_channels,
    get_latencies: v_rsb_flex_asio_get_latencies,
    get_buffer_size: v_rsb_flex_asio_get_buffer_size,
    can_sample_rate: v_rsb_flex_asio_can_sample_rate,
    get_sample_rate: v_rsb_flex_asio_get_sample_rate,
    set_sample_rate: v_rsb_flex_asio_set_sample_rate,
    get_clock_sources: v_asio_get_clock_sources,
    set_clock_source: v_asio_set_clock_source,
    get_sample_position: v_asio_get_sample_position,
    get_channel_info: v_rsb_flex_asio_get_channel_info,
    create_buffers: v_asio_create_buffers,
    dispose_buffers: v_asio_dispose_buffers,
    control_panel: v_asio_control_panel,
    future: v_asio_future,
    output_ready: v_asio_output_ready,
};

unsafe extern "system" fn cf_rsb_flex_create_instance(
    _this: *mut c_void,
    outer: *mut c_void,
    _riid: *const ffi::GUID,
    ppv: *mut *mut c_void,
) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if ppv.is_null() {
            return -2147467261;
        }
        if !outer.is_null() {
            *ppv = ptr::null_mut();
            return -2147221232; // CLASS_E_NOAGGREGATION
        }
        #[cfg(windows)]
        let (sr, smp) = {
            let saved = read_saved_rsb_flex_asio_reg_config();
            (saved.sample_rate, saved.preferred_smp)
        };
        #[cfg(not(windows))]
        let (sr, smp) = {
            let bus = get_dsp_rsb_bus_a_out();
            let (s, p, _, _) = bus.get_config();
            (s, p)
        };
        let bus = get_dsp_rsb_bus_a_out();
        let obj = Box::into_raw(Box::new(RinkaVirtualAsioComObject {
            vtbl: &RSB_FLEX_ASIO_VTBL,
            ref_count: AtomicU32::new(1),
            inner_asio: Mutex::new(ptr::null_mut()),
            last_config_gen: AtomicU32::new(bus.config_generation()),
            sample_rate: Mutex::new(sr as f64),
            buffer_size: AtomicI32::new(smp.clamp(32, 2048) as i32),
            sample_pos: AtomicU64::new(0),
            running: Arc::new(AtomicBool::new(false)),
            stop_lock: Mutex::new(()),
            worker: Mutex::new(None),
            standalone_pair: Mutex::new(None),
            buf_infos: Mutex::new(Vec::new()),
            channel_storage: Mutex::new(Vec::new()),
            callbacks: Mutex::new(None),
        }));
        *ppv = obj as *mut c_void;
        0 // S_OK
    }))
    .unwrap_or(-2147467259)
}

static CF_RSB_FLEX_VTBL: IClassFactoryVtbl = IClassFactoryVtbl {
    query_interface: cf_qi,
    add_ref: cf_add_ref,
    release: cf_release,
    create_instance: cf_rsb_flex_create_instance,
    lock_server: cf_lock_server,
};

static FACTORY_RSB_FLEX_ASIO: RinkaAsioClassFactory = RinkaAsioClassFactory {
    vtbl: &CF_RSB_FLEX_VTBL,
};

#[no_mangle]
pub unsafe extern "system" fn DllGetClassObject(
    rclsid: *const ffi::GUID,
    riid: *const ffi::GUID,
    ppv: *mut *mut c_void,
) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if ppv.is_null() || rclsid.is_null() || riid.is_null() {
            return -2147024809; // E_INVALIDARG
        }
        const CLSID_ALT: ffi::GUID = ffi::GUID {
            data1: 0x884A3B5E,
            data2: 0x9F2C,
            data3: 0x4E8A,
            data4: [0xB1, 0xD3, 0x7C, 0x6E, 0x5F, 0x4A, 0x3B, 0x2D],
        };
        let cid = &*rclsid;
        if *cid == CLSID_RSB_ASIO {
            return cf_qi(&FACTORY_RSB_FLEX_ASIO as *const RinkaAsioClassFactory as *mut c_void, riid, ppv);
        }
        if *cid == CLSID_RINKA_ASIO || *cid == CLSID_DSP_RSB_ASIO || *cid == CLSID_ALT {
            return cf_qi(&FACTORY_DSP_RSB_ASIO as *const RinkaAsioClassFactory as *mut c_void, riid, ppv);
        }
        *ppv = ptr::null_mut();
        -2147221231 // CLASS_E_CLASSNOTAVAILABLE
    }))
    .unwrap_or(-2147467259)
}

#[no_mangle]
pub unsafe extern "system" fn DllCanUnloadNow() -> i32 {
    1 // S_FALSE
}

/// Fusi presisi 55% Instant (ter-damped untuk spike toggle 1-blok) + 45% Fast-EMA dari CUDA Event ORION.
/// Mencegah 1-block stall sementara saat user mengubah setting ORION di tengah konversi menggembungkan `c_target`.
#[inline]
pub(crate) fn blend_orion_telemetry_ms(orion_inst_ms: f32, orion_ema_ms: f32) -> f32 {
    let eff_inst_ms = if orion_ema_ms > 1.0
        && orion_ema_ms < 60.0
        && orion_inst_ms > orion_ema_ms * 2.5
    {
        (orion_ema_ms * 2.5).min(orion_inst_ms)
    } else {
        orion_inst_ms
    };
    (0.55 * eff_inst_ms + 0.45 * orion_ema_ms).max(orion_ema_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes tests that mutate the process-global `HYBRID_TELEMETRY` singleton via
    /// `enforce_rsb_dsp_b_config(...)` so parallel Rust test threads do not race on `stall_guard_smp`.
    static HYBRID_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn test_shm_ipc_struct_sizes_exact_parity() {
        assert_eq!(std::mem::size_of::<SharedBusMemoryLayout>(), 524_400);
        assert_eq!(std::mem::size_of::<OrionGpuTelemetryPacket>(), 80);
        assert_eq!(std::mem::size_of::<OrionAutoResetSyncPacket>(), 64);
        assert_eq!(SHM_FLAG_COORD_RESET_FROM_ORION, 1u32 << 27);
        assert_eq!(SHM_FLAG_COORD_RESET_FROM_RSB, 1u32 << 31);
        assert_eq!(SHM_FLAG_COORD_RESET_FROM_ORION & 0x7000_0000, 0);
        assert_eq!(SHM_FLAG_COORD_RESET_FROM_RSB & 0x7000_0000, 0);
        assert_eq!(SHM_FLAG_COORD_RESET_FROM_ORION & SHM_FLAG_COORD_RESET_FROM_RSB, 0);
    }

    #[test]
    fn test_adaptive_flags_bit_independence_across_all_driver_modes() {
        let _lk = HYBRID_TEST_LOCK.lock();
        let bus = SharedAsioBus::new_in_memory();
        for mode in 0u8..=4u8 {
            bus.set_rsb_driver_mode(mode);
            assert_eq!(bus.get_rsb_driver_mode(), mode);

            // Setting driver_mode=4 (bit 30) must NEVER falsely trigger check_and_clear_coord_reset_flag!
            assert!(!bus.check_and_clear_coord_reset_flag());
            assert_eq!(bus.get_rsb_driver_mode(), mode);

            // Triggering RSB->ORION reset (bit 31) must NOT be self-consumed by RSB's check_and_clear (bit 27)
            bus.trigger_coord_reset_flag();
            assert_ne!(bus.raw_adaptive_flags() & SHM_FLAG_COORD_RESET_FROM_RSB, 0);
            assert!(!bus.check_and_clear_coord_reset_flag());
            assert_ne!(bus.raw_adaptive_flags() & SHM_FLAG_COORD_RESET_FROM_RSB, 0);
            assert_eq!(bus.get_rsb_driver_mode(), mode);

            // Simulate ORION setting bit 27 (ORION->RSB)
            bus.layout()
                .adaptive_flags
                .fetch_or(SHM_FLAG_COORD_RESET_FROM_ORION, Ordering::AcqRel);

            // Mutators must preserve bits 27..31 (both reset flags + driver_mode)
            bus.set_adaptive_flags_with_mult(true, false, 1.22);
            assert_eq!(bus.get_rsb_driver_mode(), mode);
            assert_ne!(bus.raw_adaptive_flags() & SHM_FLAG_COORD_RESET_FROM_ORION, 0);
            assert_ne!(bus.raw_adaptive_flags() & SHM_FLAG_COORD_RESET_FROM_RSB, 0);

            bus.set_adaptive_mode(4);
            assert_eq!(bus.get_adaptive_mode(), 4);
            assert_eq!(bus.get_rsb_driver_mode(), mode);
            assert_ne!(bus.raw_adaptive_flags() & SHM_FLAG_COORD_RESET_FROM_ORION, 0);
            assert_ne!(bus.raw_adaptive_flags() & SHM_FLAG_COORD_RESET_FROM_RSB, 0);

            bus.enforce_rsb_dsp_b_config(48000, 256, 60, true, true, 1.15, 5, 0.0, 12.0);
            assert_eq!(bus.get_adaptive_mode(), 5);
            assert_eq!(bus.get_rsb_driver_mode(), mode);
            assert_ne!(bus.raw_adaptive_flags() & SHM_FLAG_COORD_RESET_FROM_ORION, 0);
            assert_ne!(bus.raw_adaptive_flags() & SHM_FLAG_COORD_RESET_FROM_RSB, 0);

            // RSB consumes ONLY bit 27; bit 31 and driver_mode remain intact
            assert!(bus.check_and_clear_coord_reset_flag());
            assert!(!bus.check_and_clear_coord_reset_flag());
            assert_eq!(bus.get_rsb_driver_mode(), mode);
            assert_ne!(bus.raw_adaptive_flags() & SHM_FLAG_COORD_RESET_FROM_RSB, 0);

            // Simulate ORION consuming bit 31
            bus.layout()
                .adaptive_flags
                .fetch_and(!SHM_FLAG_COORD_RESET_FROM_RSB, Ordering::AcqRel);
            assert_eq!(bus.raw_adaptive_flags() & SHM_FLAG_COORD_RESET_FROM_RSB, 0);
            assert_eq!(bus.get_rsb_driver_mode(), mode);
        }
    }

    #[test]
    fn test_adaptive_flags_concurrent_cas_no_lost_updates() {
        let _lk = HYBRID_TEST_LOCK.lock();
        let bus = Arc::new(SharedAsioBus::new_in_memory());
        bus.set_rsb_driver_mode(4);
        bus.set_pump_caught_up(true);
        bus.trigger_coord_reset_flag();
        bus.layout()
            .adaptive_flags
            .fetch_or(SHM_FLAG_COORD_RESET_FROM_ORION, Ordering::AcqRel);

        let mut handles = Vec::new();

        // Thread 1: mutates adaptive_cush/rep/mult bits (0..1, 8..15)
        {
            let b = Arc::clone(&bus);
            handles.push(std::thread::spawn(move || {
                for i in 0..4000u32 {
                    b.set_adaptive_flags_with_mult(i % 2 == 0, i % 3 == 0, 1.05 + ((i % 20) as f32) * 0.01);
                }
            }));
        }
        // Thread 2: mutates adaptive_mode bits (24..26)
        {
            let b = Arc::clone(&bus);
            handles.push(std::thread::spawn(move || {
                for i in 0..4000u32 {
                    b.set_adaptive_mode((i % 6) as u8);
                }
            }));
        }
        // Thread 3: mutates config & adaptive flags via enforce_rsb_dsp_b_config
        {
            let b = Arc::clone(&bus);
            handles.push(std::thread::spawn(move || {
                for i in 0..4000u32 {
                    b.enforce_rsb_dsp_b_config(48000, 256, 60, true, i % 2 == 0, 1.15, (i % 6) as u8, 0.0, 12.0);
                }
            }));
        }
        // Thread 4: asserts preserved bits (16, 27, 28..30==4, 31) are NEVER lost under contention
        {
            let b = Arc::clone(&bus);
            handles.push(std::thread::spawn(move || {
                for _ in 0..4000u32 {
                    let raw = b.raw_adaptive_flags();
                    assert_eq!((raw >> 28) & 0x07, 4, "driver_mode corrupted: {raw:#010x}");
                    assert_ne!(raw & (1 << 16), 0, "pump_caught_up lost: {raw:#010x}");
                    assert_ne!(raw & SHM_FLAG_COORD_RESET_FROM_ORION, 0, "bit 27 lost: {raw:#010x}");
                    assert_ne!(raw & SHM_FLAG_COORD_RESET_FROM_RSB, 0, "bit 31 lost: {raw:#010x}");
                }
            }));
        }

        for h in handles {
            h.join().expect("CAS stress thread panicked");
        }
        assert_eq!(bus.get_rsb_driver_mode(), 4);
        assert_ne!(bus.raw_adaptive_flags() & SHM_FLAG_COORD_RESET_FROM_ORION, 0);
        assert_ne!(bus.raw_adaptive_flags() & SHM_FLAG_COORD_RESET_FROM_RSB, 0);
    }

    #[test]
    fn test_bug2_bus_b_excess_trim_at_rvc_noise_floor_and_tight_max_lag() {
        let _lk = HYBRID_TEST_LOCK.lock();
        let bus = SharedAsioBus::new_in_memory();
        bus.set_rsb_driver_mode(3); // OrionDirect
        bus.set_pump_caught_up(true);
        bus.enforce_rsb_dsp_b_config(48_000, 256, 2400, true, true, 1.15, 5, 25.0, 15.0);

        // Case 1: True-Silence Excess Trim recovers stuck backlog even with RVC idle noise floor (0.0012 = -58.4 dBFS)
        let rvc_idle = vec![0.0012f32; 9600]; // 200 ms of RVC noise floor (> safe_target ~72ms)
        bus.write_stereo_dsp_b(&rvc_idle, &rvc_idle, 2400);
        assert_eq!(bus.available_to_read(), 9600);

        let mut out_l = vec![0.0f32; 480];
        let mut out_r = vec![0.0f32; 480];
        bus.read_stereo_dsp_b_into(&mut out_l, &mut out_r, 2400, true);
        let avail_after_silence_trim = bus.available_to_read();
        assert!(
            avail_after_silence_trim <= 2880,
            "True-Silence Excess Trim must trim RVC idle backlog down near c_target (2400), got {avail_after_silence_trim}"
        );

        // Case 2: Active-speech hard max_lag (+140ms ceiling, <= 16800) recovers a +285ms stuck toggle backlog
        let loud_speech = vec![0.25f32; 19_200]; // 400 ms active speech backlog
        bus.write_stereo_dsp_b(&loud_speech, &loud_speech, 2400);
        assert!(bus.available_to_read() > 16_800);
        bus.read_stereo_dsp_b_into(&mut out_l, &mut out_r, 2400, true);
        let avail_after_jump = bus.available_to_read();
        assert!(
            avail_after_jump <= 3360,
            "Tightened hard max_lag must jump excess active-speech backlog back to c_target (~2400), got {avail_after_jump}"
        );
    }

    #[test]
    fn test_reconciled_effective_gpu_stall_matches_active_cushion_formula() {
        let _lk = HYBRID_TEST_LOCK.lock();
        let bus = SharedAsioBus::new_in_memory();
        // Set sensor stall = 20 ms (960 smp @ 48 kHz), mult = 1.15x, stall_guard = 15 ms (720 smp), extra = 50 ms (unused in cushion)
        bus.enforce_rsb_dsp_b_config(48_000, 256, 960, true, true, 1.15, 5, 50.0, 15.0);
        bus.layout()
            .measured_gpu_stall_samples
            .store(960, Ordering::Release);
        let eff = bus.get_effective_gpu_stall_samples();
        let (active_cush, _) = bus.get_dsp_b_params();
        // Expected: (960 * 115) / 100 + 720 = 1104 + 720 = 1824 smp (NOT 960 + 2400 = 3360!)
        assert_eq!(eff, 1824);
        assert_eq!(active_cush, 1824);
    }

    #[test]
    fn test_bug2_mode6_orion_toggle_spike_damping() {
        // Steady-state conversion (ema=22ms, inst=25ms): normal 55/45 blend
        let steady = blend_orion_telemetry_ms(25.0, 22.0);
        assert!((steady - 23.65).abs() < 1e-4, "steady={steady}");

        // 1-block mid-conversion toggle stall spike (ema=22ms, inst=280ms):
        // clamped at 2.5x ema (55ms) -> 0.55*55 + 0.45*22 = 40.15ms instead of 163.9ms!
        let damped = blend_orion_telemetry_ms(280.0, 22.0);
        assert!((damped - 40.15).abs() < 1e-4, "damped={damped}");

        // Sustained high GPU load (ema=70ms >= 60ms, inst=200ms): NOT damped, respects real load
        let heavy = blend_orion_telemetry_ms(200.0, 70.0);
        assert!((heavy - (0.55 * 200.0 + 0.45 * 70.0)).abs() < 1e-4, "heavy={heavy}");

        // Instant drop below EMA (ema=24ms, inst=15ms): clamped at EMA floor (24ms)
        let floor = blend_orion_telemetry_ms(15.0, 24.0);
        assert!((floor - 24.0).abs() < 1e-4, "floor={floor}");
    }
}
