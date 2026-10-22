#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! Rinka SoundBridge v2.0 2026.10 (Rust Native Edition)
//! Desain UI identik 1:1 dengan Rinka SoundBridge v1.22 (Windows 11 Dark):
//! - Font native Windows Segoe UI (tanpa kotak-kotak/tofu) + Ikon Vektor Presisi
//! - GroupBox bergaya PyQt6 dengan judul di atas garis border
//! - Kotak wadah VST Chain besar (#232323) yang selalu tampil utuh + tombol penuh "+ Add VST3"
//! - Pilihan Driver (WASAPI Shared / WASAPI Exclusive / ASIO Native) berbentuk Dropdown rapi
//! - Slider Master Volume berlapis Cyan (#60CDFF) dengan kenop bulat putih bertepi biru
//! - Baris Overlay Audio (Loop File) rata penuh dengan tombol stepper [ < ] 50% [ > ] di sisi kanan

use eframe::egui::{
    self, Align, Color32, FontData, FontDefinitions, FontFamily, FontId, Layout, Margin, Pos2,
    Rect, Response, RichText, Rounding, Sense, Shape, Stroke, Vec2,
};
use rinka_soundbridge_core::asio_host::{
    asio_diag_poll, ensure_asio_requirements_installed, get_dsp_rsb_bus_b_in, open_asio_control_panel,
    read_user_flexasio_toml_devices,
    register_virtual_asio_drivers, register_virtual_asio_drivers_with_settings, AsioDriverInfo,
    AsioRegistrationSettings, DSP_RSB_ASIO_DRIVER_NAME, RINKA_ASIO_DRIVER_NAME, RSB_ASIO_DRIVER_NAME,
};
use rinka_soundbridge_core::config::{
    get_asio_notes_txt_path, load_or_create_asio_notes_txt, save_asio_notes_txt, AdaptiveCushionMode,
    AppConfig, DspRsbAsioConfig, FlexAsioProfileConfig, LineConfig, OverlaySlotConfig, RsbAsioConfig,
    VstSlotConfig, DEFAULT_ASIO_NOTES_TEXT,
};
use rinka_soundbridge_core::dsp::estimate_latency_ms;
use rinka_soundbridge_core::logger::{
    install_crash_handlers, log_error, log_info, shutdown_logger,
};
use rinka_soundbridge_core::pipeline::{
    get_asio_devices, get_wasapi_devices, normalize_virtual_device_name, AudioPipeline, DriverBackend, WasapiDeviceInfo,
};
use rinka_soundbridge_core::vst3_host::VSTScanner;
use rinka_soundbridge_core::win32::{
    acquire_single_instance, boost_process_priority, capture_host_gl_context, enable_cpu_denormal_flush, get_audio_priority_boost, get_logical_cpu_count, restore_host_gl_context,
    get_main_window_rect, is_tray_quit_requested, open_file_in_notepad, poll_gpu_clock_telemetry_with_failsafe, release_single_instance, reset_gpu_boost_clock_lock_on_exit, restore_main_window_pos, set_asio_cpu_cores, set_audio_priority_boost, set_gpu_boost_clock_lock_with_failsafe, show_info_message_box, take_gpu_failsafe_alert, trim_process_working_set, GpuTelemetrySnapshot, ProcessPerfMonitor, SystemTrayManager,
};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const APP_WINDOW_TITLE: &str = "Rinka SoundBridge v2.0 Beta2";

const APP_ICON_RGBA: [u8; 4096] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 127, 127, 255, 2, 0, 0, 0, 5, 86, 100, 106, 192, 63, 72, 78, 88, 0, 0, 0, 0, 85, 85, 85, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 255, 255, 255, 1, 0, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 127, 127, 127, 4, 0, 0, 0, 0, 60, 68, 72, 130, 166, 215, 240, 255, 108, 134, 145, 210, 0, 0, 0, 5, 255, 255, 255, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 85, 85, 85, 3, 2, 2, 2, 0, 28, 28, 24, 53, 115, 146, 162, 254, 116, 185, 228, 250, 125, 165, 185, 255, 24, 19, 14, 53, 0, 0, 1, 0, 0, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3, 2, 3, 3, 0, 0, 0, 0, 11, 88, 103, 110, 184, 62, 72, 79, 74, 0, 0, 0, 0, 85, 85, 85, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 170, 255, 255, 3, 0, 0, 0, 5, 96, 113, 123, 207, 122, 178, 212, 255, 104, 174, 216, 248, 137, 190, 218, 255, 79, 83, 81, 119, 0, 0, 0, 0, 127, 127, 127, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 63, 63, 63, 4, 3, 3, 3, 0, 0, 0, 0, 14, 87, 104, 114, 192, 154, 204, 229, 255, 67, 84, 88, 109, 0, 0, 0, 0, 63, 127, 127, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 127, 127, 127, 4, 0, 0, 0, 0, 72, 80, 82, 133, 136, 175, 199, 255, 107, 173, 214, 251, 111, 177, 217, 252, 134, 195, 230, 255, 110, 125, 128, 187, 0, 0, 0, 0, 255, 255, 255, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 127, 127, 127, 4, 0, 0, 0, 0, 0, 0, 0, 34, 115, 131, 136, 203, 139, 186, 214, 255, 117, 177, 211, 252, 65, 79, 81, 109, 0, 0, 0, 0, 63, 127, 127, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 85, 85, 3, 2, 3, 3, 0, 23, 18, 18, 55, 147, 164, 171, 253, 116, 176, 213, 252, 101, 168, 208, 254, 103, 159, 196, 254, 124, 191, 229, 255, 134, 156, 165, 237, 0, 0, 0, 18, 1, 2, 3, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 3, 255, 255, 255, 1, 0, 0, 0, 0, 27, 30, 30, 83, 164, 172, 172, 234, 194, 225, 241, 255, 100, 165, 205, 247, 130, 191, 224, 255, 76, 93, 97, 123, 0, 0, 0, 0, 127, 127, 127, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 255, 255, 255, 2, 0, 0, 0, 7, 136, 137, 136, 210, 174, 197, 217, 255, 88, 132, 168, 252, 161, 189, 209, 255, 156, 180, 199, 255, 97, 167, 211, 255, 145, 174, 189, 255, 36, 18, 6, 42, 0, 0, 0, 2, 0, 0, 0, 4, 0, 0, 0, 3, 1, 1, 1, 0, 0, 0, 0, 0, 75, 76, 78, 136, 203, 205, 206, 255, 235, 244, 251, 255, 103, 153, 187, 252, 107, 171, 211, 251, 134, 192, 224, 255, 89, 99, 103, 126, 0, 0, 0, 0, 127, 127, 127, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 127, 127, 127, 4, 1, 1, 1, 0, 70, 70, 68, 115, 216, 222, 229, 255, 157, 176, 198, 251, 218, 224, 229, 255, 252, 253, 255, 255, 211, 219, 227, 255, 116, 156, 188, 252, 145, 179, 198, 255, 47, 38, 34, 119, 1, 1, 1, 0, 3, 3, 3, 0, 1, 1, 1, 0, 3, 6, 3, 82, 89, 91, 92, 196, 230, 232, 235, 255, 255, 255, 255, 253, 148, 168, 187, 253, 166, 187, 204, 255, 115, 165, 201, 251, 139, 193, 225, 255, 92, 92, 92, 121, 0, 0, 0, 0, 127, 127, 127, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 255, 255, 255, 2, 0, 0, 0, 17, 165, 166, 167, 234, 204, 217, 232, 254, 183, 198, 216, 253, 255, 255, 255, 255, 242, 246, 252, 255, 255, 255, 255, 255, 185, 202, 217, 253, 152, 188, 211, 255, 116, 116, 114, 222, 0, 0, 0, 50, 0, 0, 0, 36, 0, 0, 0, 57, 161, 163, 164, 247, 255, 255, 255, 255, 255, 255, 255, 251, 207, 216, 225, 255, 193, 206, 220, 255, 245, 246, 247, 255, 110, 153, 186, 251, 140, 185, 211, 255, 88, 80, 75, 98, 0, 0, 0, 0, 63, 63, 63, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 63, 63, 63, 4, 1, 1, 1, 0, 54, 56, 56, 103, 240, 241, 244, 255, 193, 205, 217, 251, 175, 191, 211, 255, 222, 232, 244, 255, 227, 235, 243, 255, 214, 222, 230, 255, 181, 194, 209, 255, 184, 200, 215, 255, 196, 205, 211, 255, 157, 168, 180, 255, 157, 169, 179, 251, 156, 165, 172, 254, 225, 232, 238, 255, 214, 224, 232, 252, 235, 241, 244, 255, 164, 179, 199, 255, 206, 218, 232, 255, 249, 251, 253, 255, 223, 232, 239, 254, 137, 154, 167, 255, 33, 29, 20, 61, 0, 0, 0, 0, 0, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 255, 255, 255, 2, 0, 0, 0, 26, 114, 117, 119, 200, 255, 255, 255, 254, 211, 220, 229, 252, 184, 200, 224, 255, 180, 197, 221, 255, 175, 190, 208, 255, 209, 218, 227, 255, 248, 250, 252, 255, 219, 227, 235, 255, 179, 197, 214, 254, 232, 241, 248, 255, 249, 255, 255, 255, 217, 228, 239, 255, 191, 205, 217, 254, 229, 235, 239, 255, 194, 204, 215, 255, 159, 177, 203, 255, 200, 216, 235, 255, 240, 243, 247, 254, 209, 225, 239, 255, 126, 144, 155, 233, 0, 0, 0, 16, 1, 2, 2, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 63, 63, 63, 4, 1, 1, 1, 0, 46, 46, 46, 160, 178, 179, 182, 255, 255, 255, 255, 251, 165, 180, 198, 255, 158, 178, 203, 255, 192, 204, 217, 255, 244, 247, 250, 255, 255, 255, 255, 255, 188, 205, 218, 255, 149, 174, 197, 255, 200, 213, 225, 255, 236, 241, 245, 255, 248, 249, 252, 254, 215, 224, 233, 254, 168, 189, 206, 255, 168, 188, 206, 255, 250, 252, 254, 255, 210, 219, 230, 255, 168, 186, 210, 255, 239, 243, 250, 253, 195, 213, 227, 255, 80, 89, 95, 216, 0, 0, 0, 12, 255, 255, 255, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 255, 255, 3, 0, 0, 0, 0, 122, 124, 125, 166, 255, 255, 255, 254, 255, 255, 255, 251, 173, 188, 203, 255, 187, 199, 212, 255, 255, 255, 255, 255, 251, 253, 255, 255, 201, 213, 224, 255, 176, 191, 208, 255, 247, 250, 252, 255, 251, 252, 255, 255, 247, 249, 255, 255, 244, 247, 253, 255, 250, 252, 255, 255, 255, 255, 255, 255, 200, 212, 224, 255, 189, 201, 214, 255, 255, 255, 255, 255, 209, 218, 227, 255, 170, 187, 202, 253, 151, 187, 210, 255, 112, 116, 116, 219, 0, 0, 0, 19, 255, 255, 255, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 255, 255, 255, 1, 23, 23, 23, 11, 97, 98, 98, 202, 255, 254, 254, 255, 229, 235, 241, 253, 198, 208, 218, 255, 255, 255, 255, 255, 246, 249, 253, 255, 240, 245, 250, 255, 212, 219, 228, 255, 253, 255, 255, 255, 227, 233, 238, 255, 232, 237, 243, 255, 247, 250, 255, 255, 245, 247, 252, 255, 245, 248, 253, 255, 224, 230, 237, 255, 250, 253, 255, 255, 223, 230, 237, 255, 232, 238, 245, 255, 255, 255, 255, 255, 172, 193, 210, 251, 169, 199, 218, 255, 90, 85, 80, 146, 0, 0, 0, 0, 127, 127, 127, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 3, 3, 3, 0, 0, 0, 0, 14, 128, 133, 141, 238, 254, 255, 255, 255, 194, 205, 215, 254, 240, 244, 248, 255, 252, 252, 254, 255, 248, 250, 253, 255, 235, 242, 248, 255, 249, 253, 255, 255, 246, 250, 254, 255, 211, 220, 229, 255, 245, 248, 252, 255, 246, 249, 254, 255, 244, 247, 252, 255, 250, 252, 255, 255, 227, 234, 239, 255, 225, 231, 238, 255, 253, 255, 255, 255, 237, 243, 248, 255, 249, 250, 251, 254, 255, 255, 255, 254, 146, 152, 158, 244, 55, 51, 51, 64, 170, 255, 255, 3, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 63, 63, 63, 4, 0, 0, 0, 0, 38, 41, 44, 86, 177, 191, 205, 254, 207, 218, 228, 252, 227, 233, 238, 255, 249, 251, 255, 255, 246, 249, 254, 255, 238, 242, 248, 255, 232, 238, 244, 255, 252, 254, 254, 255, 236, 241, 248, 255, 200, 210, 220, 255, 254, 255, 255, 255, 247, 249, 252, 255, 244, 247, 253, 255, 253, 254, 255, 255, 242, 245, 247, 255, 218, 227, 233, 255, 254, 255, 255, 255, 233, 239, 246, 255, 236, 240, 245, 255, 255, 255, 255, 253, 200, 202, 205, 249, 7, 7, 7, 66, 1, 1, 1, 0, 85, 85, 85, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 255, 255, 2, 18, 18, 18, 14, 80, 87, 100, 186, 199, 212, 228, 255, 231, 236, 241, 252, 247, 249, 253, 255, 243, 247, 252, 255, 249, 252, 255, 255, 211, 219, 230, 255, 232, 236, 244, 255, 255, 255, 255, 255, 194, 196, 204, 255, 175, 176, 184, 255, 255, 255, 255, 255, 244, 246, 251, 255, 244, 248, 253, 255, 248, 250, 253, 255, 247, 249, 252, 255, 171, 179, 187, 255, 246, 249, 253, 255, 228, 234, 242, 255, 203, 215, 228, 255, 245, 248, 253, 253, 255, 255, 255, 255, 164, 167, 170, 236, 0, 0, 0, 47, 0, 0, 0, 0, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3, 255, 255, 255, 1, 18, 20, 20, 112, 145, 155, 168, 255, 208, 221, 234, 252, 200, 212, 226, 255, 240, 244, 248, 255, 247, 250, 255, 255, 241, 245, 250, 255, 195, 206, 218, 255, 247, 253, 255, 255, 215, 224, 230, 255, 180, 148, 147, 255, 189, 181, 187, 255, 247, 254, 255, 255, 244, 246, 252, 255, 239, 244, 250, 255, 243, 247, 253, 255, 240, 249, 255, 255, 160, 143, 147, 255, 206, 203, 207, 255, 243, 253, 255, 255, 177, 193, 210, 255, 238, 242, 248, 255, 240, 243, 248, 254, 218, 228, 238, 255, 163, 168, 172, 255, 64, 64, 64, 122, 23, 23, 23, 11, 255, 255, 255, 1, 0, 0, 0, 0, 0, 0, 0, 0, 102, 102, 102, 5, 2, 2, 2, 0, 59, 61, 61, 108, 216, 220, 227, 255, 204, 215, 228, 252, 183, 196, 210, 255, 249, 251, 255, 255, 247, 250, 255, 255, 232, 236, 243, 255, 195, 212, 230, 255, 198, 209, 219, 255, 174, 153, 155, 255, 247, 212, 202, 255, 183, 174, 177, 255, 206, 219, 229, 255, 244, 246, 251, 255, 233, 240, 247, 255, 243, 247, 252, 255, 202, 215, 226, 255, 203, 178, 178, 255, 196, 164, 160, 255, 209, 217, 223, 255, 197, 211, 228, 255, 214, 222, 231, 254, 255, 255, 255, 255, 147, 150, 155, 244, 93, 98, 102, 156, 67, 67, 69, 132, 0, 0, 0, 13, 255, 255, 255, 1, 0, 0, 0, 0, 51, 51, 51, 5, 5, 5, 5, 0, 0, 0, 0, 27, 164, 166, 170, 237, 255, 255, 255, 254, 236, 240, 246, 254, 212, 220, 229, 255, 249, 251, 255, 255, 249, 251, 255, 255, 224, 232, 240, 255, 155, 163, 182, 255, 197, 174, 175, 255, 255, 241, 233, 255, 228, 218, 210, 255, 151, 132, 132, 255, 144, 155, 170, 255, 230, 241, 248, 255, 238, 243, 248, 255, 218, 230, 241, 255, 129, 134, 148, 255, 164, 144, 142, 255, 247, 223, 214, 255, 200, 182, 182, 255, 159, 176, 196, 255, 213, 222, 233, 255, 251, 255, 255, 254, 188, 192, 195, 252, 0, 0, 0, 26, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 8, 125, 125, 124, 195, 252, 255, 255, 255, 235, 241, 246, 253, 218, 226, 237, 255, 206, 216, 228, 255, 247, 250, 254, 255, 250, 254, 255, 255, 210, 212, 218, 255, 201, 175, 170, 255, 169, 139, 132, 255, 157, 133, 129, 255, 223, 208, 201, 255, 252, 232, 223, 255, 159, 160, 167, 255, 145, 158, 172, 255, 230, 242, 255, 255, 153, 163, 180, 255, 150, 143, 149, 255, 247, 233, 225, 255, 199, 181, 176, 255, 145, 111, 106, 255, 153, 135, 138, 255, 215, 220, 227, 255, 235, 244, 253, 251, 221, 230, 236, 255, 45, 51, 62, 123, 255, 255, 255, 1, 51, 102, 102, 5, 0, 0, 0, 0, 85, 85, 85, 6, 38, 38, 38, 79, 132, 136, 141, 218, 235, 244, 252, 255, 200, 213, 227, 252, 239, 245, 251, 255, 209, 220, 231, 255, 216, 226, 236, 255, 242, 246, 251, 255, 242, 249, 254, 255, 215, 216, 221, 255, 222, 206, 199, 255, 182, 161, 156, 255, 116, 84, 82, 255, 100, 69, 66, 255, 143, 112, 108, 255, 202, 182, 175, 255, 187, 174, 173, 255, 169, 174, 185, 255, 165, 163, 167, 255, 206, 179, 171, 255, 129, 101, 98, 255, 100, 68, 65, 255, 160, 137, 132, 255, 220, 201, 196, 255, 211, 214, 220, 255, 212, 225, 237, 253, 195, 209, 222, 254, 131, 137, 144, 231, 0, 0, 0, 16, 1, 2, 2, 0, 0, 0, 0, 2, 0, 0, 0, 9, 43, 43, 47, 135, 124, 131, 138, 188, 74, 83, 93, 208, 186, 196, 208, 255, 241, 248, 251, 254, 195, 208, 220, 255, 237, 241, 247, 255, 218, 226, 234, 255, 233, 242, 249, 255, 224, 228, 235, 255, 206, 194, 192, 255, 255, 252, 244, 255, 219, 202, 195, 255, 133, 105, 102, 255, 40, 2, 2, 255, 129, 100, 96, 255, 255, 253, 244, 255, 237, 216, 209, 255, 234, 224, 218, 255, 132, 102, 99, 255, 44, 6, 6, 255, 151, 126, 123, 255, 234, 219, 212, 255, 198, 189, 185, 255, 180, 192, 204, 255, 191, 206, 225, 255, 142, 158, 181, 252, 245, 249, 253, 255, 129, 130, 132, 189, 15, 15, 15, 16, 255, 255, 255, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 95, 102, 113, 184, 225, 239, 253, 255, 217, 228, 239, 252, 182, 196, 211, 255, 214, 225, 234, 255, 221, 229, 237, 255, 193, 206, 221, 255, 212, 225, 235, 255, 202, 195, 195, 255, 123, 87, 85, 255, 80, 39, 37, 255, 140, 105, 100, 255, 206, 187, 180, 255, 245, 227, 220, 255, 250, 233, 226, 255, 255, 241, 234, 255, 255, 242, 235, 255, 245, 227, 220, 255, 206, 186, 180, 255, 131, 91, 88, 255, 95, 52, 49, 255, 140, 122, 125, 255, 150, 167, 186, 255, 154, 173, 197, 254, 179, 194, 211, 255, 133, 134, 135, 228, 142, 144, 146, 227, 40, 40, 40, 139, 1, 1, 1, 0, 0, 0, 0, 2, 170, 255, 255, 3, 28, 28, 28, 27, 131, 154, 170, 246, 193, 219, 238, 255, 200, 215, 228, 254, 119, 158, 189, 255, 178, 204, 221, 255, 253, 255, 255, 255, 182, 198, 216, 255, 160, 180, 201, 255, 141, 144, 153, 253, 199, 155, 150, 255, 252, 203, 195, 255, 255, 215, 208, 255, 255, 232, 223, 255, 255, 242, 234, 255, 229, 207, 200, 255, 204, 178, 173, 255, 226, 203, 196, 255, 255, 239, 232, 255, 255, 231, 223, 255, 255, 213, 205, 255, 252, 201, 193, 255, 211, 173, 170, 255, 164, 140, 142, 255, 189, 199, 210, 252, 214, 230, 246, 255, 133, 138, 139, 197, 0, 0, 0, 2, 145, 145, 145, 7, 0, 0, 0, 0, 0, 0, 0, 2, 1, 2, 2, 0, 0, 0, 0, 29, 126, 164, 185, 248, 97, 123, 139, 254, 158, 188, 206, 254, 83, 153, 200, 255, 157, 191, 214, 254, 242, 254, 255, 255, 232, 247, 255, 255, 160, 169, 182, 255, 213, 173, 169, 255, 255, 213, 203, 254, 255, 198, 193, 251, 248, 193, 188, 252, 247, 219, 211, 253, 252, 236, 229, 254, 237, 215, 208, 254, 232, 210, 202, 255, 241, 221, 214, 254, 249, 232, 225, 254, 247, 220, 212, 254, 248, 197, 187, 252, 252, 197, 189, 252, 255, 218, 208, 255, 197, 164, 160, 255, 201, 209, 218, 254, 201, 221, 241, 255, 158, 172, 177, 238, 51, 51, 51, 15, 3, 3, 3, 0, 0, 0, 0, 1, 0, 0, 0, 0, 255, 255, 255, 2, 0, 0, 0, 2, 86, 117, 132, 213, 51, 67, 76, 204, 128, 160, 177, 238, 89, 158, 203, 253, 113, 166, 201, 255, 210, 242, 253, 255, 209, 240, 253, 255, 190, 207, 216, 252, 169, 128, 124, 217, 230, 187, 179, 255, 255, 229, 220, 255, 255, 244, 235, 255, 255, 248, 240, 255, 255, 240, 232, 255, 255, 239, 232, 255, 255, 242, 235, 255, 255, 243, 236, 255, 255, 242, 234, 255, 255, 247, 239, 255, 255, 247, 238, 255, 255, 236, 227, 255, 199, 172, 165, 255, 167, 166, 177, 255, 221, 237, 246, 254, 156, 180, 201, 255, 140, 164, 172, 239, 0, 0, 0, 19, 2, 3, 3, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 31, 39, 43, 65, 15, 20, 20, 101, 81, 102, 113, 137, 80, 111, 133, 255, 64, 101, 126, 247, 163, 223, 251, 254, 170, 222, 242, 251, 191, 233, 250, 254, 80, 104, 115, 95, 93, 35, 31, 57, 161, 122, 119, 139, 187, 157, 151, 194, 207, 185, 178, 227, 233, 210, 200, 244, 240, 217, 210, 255, 233, 211, 205, 255, 225, 204, 197, 252, 220, 199, 193, 247, 210, 188, 182, 230, 195, 173, 167, 193, 141, 125, 127, 166, 151, 160, 177, 251, 197, 228, 248, 253, 171, 211, 229, 251, 77, 92, 103, 255, 91, 111, 120, 203, 0, 0, 0, 1, 255, 255, 255, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 6, 23, 25, 28, 109, 29, 35, 36, 131, 122, 183, 213, 255, 145, 220, 251, 251, 149, 216, 246, 253, 112, 153, 170, 215, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 9, 117, 68, 58, 26, 176, 127, 121, 42, 115, 81, 75, 44, 75, 34, 34, 37, 51, 0, 0, 30, 0, 0, 0, 14, 0, 0, 0, 0, 0, 6, 39, 39, 146, 179, 201, 233, 168, 224, 250, 255, 82, 114, 130, 234, 20, 20, 22, 162, 33, 40, 46, 76, 0, 0, 0, 0, 0, 0, 85, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 127, 127, 2, 0, 0, 0, 2, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 9, 78, 113, 129, 227, 115, 185, 213, 255, 141, 226, 255, 255, 70, 109, 125, 212, 28, 36, 42, 90, 170, 170, 170, 6, 255, 255, 255, 4, 2, 2, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 3, 3, 0, 255, 255, 255, 2, 46, 53, 58, 104, 126, 173, 196, 255, 62, 88, 102, 227, 10, 8, 8, 123, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 42, 85, 85, 6, 0, 1, 1, 0, 37, 50, 55, 96, 45, 66, 75, 173, 116, 190, 215, 221, 107, 174, 201, 228, 21, 36, 43, 35, 127, 255, 255, 2, 0, 0, 0, 2, 0, 0, 0, 1, 255, 255, 255, 1, 255, 127, 127, 2, 127, 127, 127, 2, 127, 127, 127, 2, 127, 0, 0, 2, 127, 127, 127, 2, 0, 0, 0, 0, 37, 44, 48, 115, 41, 54, 63, 93, 0, 0, 0, 12, 0, 0, 0, 0, 1, 1, 1, 0, 0, 0, 127, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 37, 15, 15, 23, 32, 95, 148, 170, 163, 52, 82, 98, 111, 0, 0, 0, 0, 127, 127, 127, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 2, 3, 0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

// ==============================================================================
// 1. PALET WARNA WINDOWS 11 DARK (IDENTIK 1:1 DENGAN v1.22)
// ==============================================================================

pub struct Palette;

#[allow(dead_code)]
impl Palette {
    pub const BG_APP: Color32 = Color32::from_rgb(0x20, 0x20, 0x20);
    pub const BG_PANEL: Color32 = Color32::from_rgb(0x2C, 0x2C, 0x2C);
    pub const BG_PANEL_ALT: Color32 = Color32::from_rgb(0x27, 0x27, 0x27);
    pub const BG_INPUT: Color32 = Color32::from_rgb(0x1E, 0x1E, 0x1E);
    pub const BG_SCROLL: Color32 = Color32::from_rgb(0x23, 0x23, 0x23);
    pub const BORDER: Color32 = Color32::from_rgb(0x38, 0x38, 0x38);
    pub const BORDER_STRONG: Color32 = Color32::from_rgb(0x4A, 0x4A, 0x4A);
    pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);
    pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(0xCC, 0xCC, 0xCC);
    pub const TEXT_MUTED: Color32 = Color32::from_rgb(0x8A, 0x8A, 0x8A);
    pub const ACCENT_PINK: Color32 = Color32::from_rgb(0x60, 0xCD, 0xFF);
    pub const ACCENT_PINK_SOFT: Color32 = Color32::from_rgb(0x24, 0x3E, 0x4F);
    pub const ACCENT_PINK_HOVER: Color32 = Color32::from_rgb(0x78, 0xD5, 0xFF);
    pub const ACCENT_GREEN: Color32 = Color32::from_rgb(0x6C, 0xCB, 0x5F);
    pub const ACCENT_GREEN_HOVER: Color32 = Color32::from_rgb(0x84, 0xD7, 0x79);
    pub const ACCENT_GREEN_DEEP: Color32 = Color32::from_rgb(0x44, 0x9E, 0x37);
    pub const ACCENT_BLUE: Color32 = Color32::from_rgb(0x60, 0xCD, 0xFF);
    pub const ACCENT_BLUE_HOVER: Color32 = Color32::from_rgb(0x78, 0xD5, 0xFF);
    pub const ACCENT_BLUE_DEEP: Color32 = Color32::from_rgb(0x00, 0x78, 0xD4);
    pub const ACCENT_CORAL: Color32 = Color32::from_rgb(0xFF, 0x6B, 0x6B);
    pub const ACCENT_CORAL_HOVER: Color32 = Color32::from_rgb(0xFF, 0x85, 0x85);
    pub const ACCENT_CORAL_DEEP: Color32 = Color32::from_rgb(0xC4, 0x2B, 0x1C);
    pub const ACCENT_YELLOW: Color32 = Color32::from_rgb(0xFC, 0xE1, 0x00);
    pub const ACCENT_YELLOW_HOVER: Color32 = Color32::from_rgb(0xFF, 0xEA, 0x4D);
    pub const ACCENT_LAVENDER: Color32 = Color32::from_rgb(0x7E, 0x8C, 0xE0);
    pub const ACCENT_LAVENDER_HOVER: Color32 = Color32::from_rgb(0x93, 0xA0, 0xEC);
}

fn parse_hex_color(hex: &str) -> Color32 {
    let s = hex.trim_start_matches('#');
    if s.len() == 6 {
        if let (Ok(r), Ok(g), Ok(b)) = (
            u8::from_str_radix(&s[0..2], 16),
            u8::from_str_radix(&s[2..4], 16),
            u8::from_str_radix(&s[4..6], 16),
        ) {
            return Color32::from_rgb(r, g, b);
        }
    }
    Palette::BG_INPUT
}

// ==============================================================================
// 2. IKON VEKTOR PRESISI (ANTI-TOFU / ANTI KOTAK-KOTAK DI SEMUA WINDOWS)
// ==============================================================================

#[derive(Clone, Copy)]
enum GroupIcon {
    Mic,
    Plug,
    Folder,
    Speaker,
}

fn paint_group_icon(painter: &egui::Painter, center: Pos2, icon: GroupIcon) {
    match icon {
        GroupIcon::Mic => {
            // Kapsul mikrofon perak/biru muda seperti di v1.22
            let cap = Rect::from_center_size(Pos2::new(center.x, center.y - 2.5), Vec2::new(5.6, 8.5));
            painter.rect_filled(cap, Rounding::same(2.8), Color32::from_rgb(0xB8, 0xC4, 0xD0));
            painter.line_segment(
                [Pos2::new(center.x - 4.2, center.y - 1.0), Pos2::new(center.x - 4.2, center.y + 2.2)],
                Stroke::new(1.3_f32, Color32::from_rgb(0x90, 0xA0, 0xB0)),
            );
            painter.line_segment(
                [Pos2::new(center.x + 4.2, center.y - 1.0), Pos2::new(center.x + 4.2, center.y + 2.2)],
                Stroke::new(1.3_f32, Color32::from_rgb(0x90, 0xA0, 0xB0)),
            );
            painter.line_segment(
                [Pos2::new(center.x - 4.2, center.y + 2.2), Pos2::new(center.x + 4.2, center.y + 2.2)],
                Stroke::new(1.3_f32, Color32::from_rgb(0x90, 0xA0, 0xB0)),
            );
            painter.line_segment(
                [Pos2::new(center.x, center.y + 2.2), Pos2::new(center.x, center.y + 5.5)],
                Stroke::new(1.4_f32, Color32::from_rgb(0x90, 0xA0, 0xB0)),
            );
            painter.line_segment(
                [Pos2::new(center.x - 3.2, center.y + 5.5), Pos2::new(center.x + 3.2, center.y + 5.5)],
                Stroke::new(1.4_f32, Color32::from_rgb(0x90, 0xA0, 0xB0)),
            );
        }
        GroupIcon::Plug => {
            // Ikon steker VST (ungu/biru dengan 2 pin emas)
            painter.line_segment(
                [Pos2::new(center.x - 2.2, center.y - 6.0), Pos2::new(center.x - 2.2, center.y - 2.0)],
                Stroke::new(1.5_f32, Color32::from_rgb(0xE0, 0xC0, 0x60)),
            );
            painter.line_segment(
                [Pos2::new(center.x + 2.2, center.y - 6.0), Pos2::new(center.x + 2.2, center.y - 2.0)],
                Stroke::new(1.5_f32, Color32::from_rgb(0xE0, 0xC0, 0x60)),
            );
            let body = Rect::from_center_size(Pos2::new(center.x, center.y + 0.8), Vec2::new(8.0, 6.2));
            painter.rect_filled(body, Rounding::same(2.0), Color32::from_rgb(0x88, 0x96, 0xE8));
            painter.line_segment(
                [Pos2::new(center.x, center.y + 3.8), Pos2::new(center.x, center.y + 6.5)],
                Stroke::new(1.8_f32, Color32::from_rgb(0x70, 0x7E, 0xD0)),
            );
        }
        GroupIcon::Folder => {
            // Ikon map/folder kuning keemasan seperti di v1.22
            let tab = Rect::from_min_size(Pos2::new(center.x - 6.5, center.y - 5.2), Vec2::new(5.5, 2.8));
            painter.rect_filled(tab, Rounding::same(1.2), Color32::from_rgb(0xE0, 0x9F, 0x24));
            let body = Rect::from_min_size(Pos2::new(center.x - 6.5, center.y - 3.2), Vec2::new(13.0, 8.8));
            painter.rect_filled(body, Rounding::same(1.8), Color32::from_rgb(0xFC, 0xC6, 0x2E));
        }
        GroupIcon::Speaker => {
            // Ikon speaker + gelombang suara biru/putih
            let cone = vec![
                Pos2::new(center.x - 5.5, center.y - 2.2),
                Pos2::new(center.x - 2.5, center.y - 2.2),
                Pos2::new(center.x + 1.0, center.y - 5.2),
                Pos2::new(center.x + 1.0, center.y + 5.2),
                Pos2::new(center.x - 2.5, center.y + 2.2),
                Pos2::new(center.x - 5.5, center.y + 2.2),
            ];
            painter.add(Shape::convex_polygon(
                cone,
                Color32::from_rgb(0xD8, 0xE2, 0xEC),
                Stroke::NONE,
            ));
            painter.line_segment(
                [Pos2::new(center.x + 3.2, center.y - 2.8), Pos2::new(center.x + 4.4, center.y)],
                Stroke::new(1.3_f32, Palette::ACCENT_BLUE),
            );
            painter.line_segment(
                [Pos2::new(center.x + 4.4, center.y), Pos2::new(center.x + 3.2, center.y + 2.8)],
                Stroke::new(1.3_f32, Palette::ACCENT_BLUE),
            );
        }
    }
}

#[allow(dead_code)]
fn paint_no_entry_icon(painter: &egui::Painter, center: Pos2, radius: f32) {
    painter.circle_filled(center, radius, Color32::from_rgb(0xE0, 0x43, 0x43));
    painter.circle_stroke(center, radius, Stroke::new(1.0_f32, Color32::from_rgb(0xFF, 0x80, 0x80)));
    painter.line_segment(
        [
            Pos2::new(center.x - radius * 0.55, center.y - radius * 0.55),
            Pos2::new(center.x + radius * 0.55, center.y + radius * 0.55),
        ],
        Stroke::new(1.8_f32, Color32::WHITE),
    );
}

fn paint_monitor_icon(painter: &egui::Painter, center: Pos2) {
    let scr = Rect::from_center_size(Pos2::new(center.x, center.y - 1.5), Vec2::new(12.5, 8.5));
    painter.rect_filled(scr, Rounding::same(1.5), Color32::from_rgb(0x3A, 0x96, 0xDD));
    painter.rect_stroke(scr, Rounding::same(1.5), Stroke::new(1.2_f32, Color32::from_rgb(0x90, 0xD5, 0xFF)));
    painter.line_segment(
        [Pos2::new(center.x, center.y + 2.8), Pos2::new(center.x, center.y + 5.2)],
        Stroke::new(1.6_f32, Color32::from_rgb(0x9A, 0xA6, 0xB2)),
    );
    painter.line_segment(
        [Pos2::new(center.x - 3.5, center.y + 5.2), Pos2::new(center.x + 3.5, center.y + 5.2)],
        Stroke::new(1.5_f32, Color32::from_rgb(0x9A, 0xA6, 0xB2)),
    );
}


fn paint_shield_icon(painter: &egui::Painter, center: Pos2, active: bool) {
    let col = if active { Palette::ACCENT_GREEN } else { Palette::TEXT_MUTED };
    let pts = vec![
        Pos2::new(center.x, center.y - 5.5),
        Pos2::new(center.x + 4.8, center.y - 3.5),
        Pos2::new(center.x + 4.0, center.y + 1.8),
        Pos2::new(center.x, center.y + 5.5),
        Pos2::new(center.x - 4.0, center.y + 1.8),
        Pos2::new(center.x - 4.8, center.y - 3.5),
    ];
    painter.add(Shape::convex_polygon(pts, col, Stroke::NONE));
}

fn paint_bolt_icon(painter: &egui::Painter, center: Pos2, col: Color32) {
    let pts = vec![
        Pos2::new(center.x + 1.0, center.y - 5.5),
        Pos2::new(center.x - 3.5, center.y + 0.5),
        Pos2::new(center.x - 0.3, center.y + 0.5),
        Pos2::new(center.x - 1.2, center.y + 5.5),
        Pos2::new(center.x + 3.5, center.y - 0.5),
        Pos2::new(center.x + 0.3, center.y - 0.5),
    ];
    painter.add(Shape::convex_polygon(pts, col, Stroke::NONE));
}

/// Menggambar kontainer `QGroupBox` persis seperti PyQt6 di v1.22 (judul memotong garis border atas).
fn draw_groupbox<R>(
    ui: &mut egui::Ui,
    icon: GroupIcon,
    title: &str,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    ui.add_space(7.0);
    let avail_w = ui.available_width();
    let frame = egui::Frame::none()
        .fill(Palette::BG_PANEL_ALT)
        .stroke(Stroke::new(1.0_f32, Palette::BORDER))
        .rounding(Rounding::same(10.0))
        .inner_margin(Margin {
            left: 10.0,
            right: 10.0,
            top: 13.0,
            bottom: 9.0,
        });

    let prepared = frame.show(ui, |ui| {
        ui.set_min_width((avail_w - 22.0).max(80.0));
        ui.set_max_width((avail_w - 22.0).max(80.0));
        add_contents(ui)
    });

    let rect = prepared.response.rect;
    let painter = ui.painter();
    let font_id = FontId::new(12.5, FontFamily::Proportional);
    let text_w = (title.chars().count() as f32) * 7.2;
    let mask_w = 24.0 + text_w + 10.0;

    // Mask garis border atas di belakang judul GroupBox
    let mask_top = Rect::from_min_size(Pos2::new(rect.min.x + 10.0, rect.min.y - 4.0), Vec2::new(mask_w, 4.5));
    let mask_bot = Rect::from_min_size(Pos2::new(rect.min.x + 10.0, rect.min.y), Vec2::new(mask_w, 4.5));
    painter.rect_filled(mask_top, Rounding::ZERO, Palette::BG_PANEL);
    painter.rect_filled(mask_bot, Rounding::ZERO, Palette::BG_PANEL_ALT);

    paint_group_icon(&painter, Pos2::new(rect.min.x + 20.0, rect.min.y - 0.5), icon);
    painter.text(
        Pos2::new(rect.min.x + 31.0, rect.min.y - 0.5),
        egui::Align2::LEFT_CENTER,
        title,
        font_id,
        Palette::ACCENT_PINK,
    );

    prepared.inner
}

/// Menggambar garis putus-putus (dashed border) untuk kotak tombol System Tray di Header seperti v1.22.
fn paint_dashed_rect(painter: &egui::Painter, rect: Rect, color: Color32, dash_len: f32, gap_len: f32) {
    let stroke = Stroke::new(1.0_f32, color);
    let step = (dash_len + gap_len).max(2.0);

    // Garis atas & bawah
    let mut x = rect.min.x + 6.0;
    while x < rect.max.x - 6.0 {
        let x2 = (x + dash_len).min(rect.max.x - 6.0);
        painter.line_segment([Pos2::new(x, rect.min.y), Pos2::new(x2, rect.min.y)], stroke);
        painter.line_segment([Pos2::new(x, rect.max.y), Pos2::new(x2, rect.max.y)], stroke);
        x += step;
    }
    // Garis kiri & kanan
    let mut y = rect.min.y + 6.0;
    while y < rect.max.y - 6.0 {
        let y2 = (y + dash_len).min(rect.max.y - 6.0);
        painter.line_segment([Pos2::new(rect.min.x, y), Pos2::new(rect.min.x, y2)], stroke);
        painter.line_segment([Pos2::new(rect.max.x, y), Pos2::new(rect.max.x, y2)], stroke);
        y += step;
    }
}

/// Menggambar Slider Master Volume berlapis Cyan (#60CDFF) dengan kenop putih bertepi biru (persis v1.22).
fn draw_v122_volume_slider(ui: &mut egui::Ui, vol: &mut i32, width: f32) -> bool {
    let desired = Vec2::new(width.max(60.0), 26.0);
    let (rect, response) = ui.allocate_exact_size(desired, Sense::click_and_drag());
    let mut changed = false;

    if response.dragged() || response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            let pad = 8.0;
            let usable = (rect.width() - 2.0 * pad).max(1.0);
            let t = ((pos.x - (rect.min.x + pad)) / usable).clamp(0.0, 1.0);
            let new_val = (t * 150.0).round() as i32;
            if new_val != *vol {
                *vol = new_val;
                changed = true;
            }
        }
    }

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let pad = 8.0;
        let track_y = rect.center().y;
        let track_rect = Rect::from_min_max(
            Pos2::new(rect.min.x + pad, track_y - 3.0),
            Pos2::new(rect.max.x - pad, track_y + 3.0),
        );
        // Track dasar (#383838)
        painter.rect_filled(track_rect, Rounding::same(3.0), Palette::BORDER);

        // Sub-page aktif (#60CDFF)
        let t = (*vol as f32 / 150.0).clamp(0.0, 1.0);
        let handle_x = track_rect.min.x + t * track_rect.width();
        if t > 0.0 {
            let sub_rect = Rect::from_min_max(
                track_rect.min,
                Pos2::new(handle_x, track_rect.max.y),
            );
            painter.rect_filled(sub_rect, Rounding::same(3.0), Palette::ACCENT_PINK);
        }

        // Kenop bulat putih dengan border cyan 2px
        let center = Pos2::new(handle_x, track_y);
        painter.circle_filled(center, 7.0, Color32::WHITE);
        painter.circle_stroke(center, 7.0, Stroke::new(2.0_f32, Palette::ACCENT_PINK));
    }

    changed
}

/// Menggambar Slider Float presisi berlapis Cyan (#60CDFF) dengan kenop bulat putih bertepi biru
/// (digunakan untuk slider Peak GPU Stall Multiplier `1.01x – 1.30x`).
fn draw_v122_f32_slider(
    ui: &mut egui::Ui,
    val: &mut f32,
    min_val: f32,
    max_val: f32,
    width: f32,
) -> bool {
    let desired = Vec2::new(width.max(60.0), 22.0);
    let (rect, response) = ui.allocate_exact_size(desired, Sense::click_and_drag());
    let mut changed = false;
    let span = (max_val - min_val).max(0.001);

    if response.dragged() || response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            let pad = 7.0;
            let usable = (rect.width() - 2.0 * pad).max(1.0);
            let t = ((pos.x - (rect.min.x + pad)) / usable).clamp(0.0, 1.0);
            let raw = min_val + t * span;
            let stepped = ((raw * 100.0).round() / 100.0).clamp(min_val, max_val);
            if (stepped - *val).abs() > 1e-4 {
                *val = stepped;
                changed = true;
            }
        }
    }

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let pad = 7.0;
        let track_y = rect.center().y;
        let track_rect = Rect::from_min_max(
            Pos2::new(rect.min.x + pad, track_y - 2.5),
            Pos2::new(rect.max.x - pad, track_y + 2.5),
        );
        painter.rect_filled(track_rect, Rounding::same(2.5), Palette::BORDER);

        let t = ((*val - min_val) / span).clamp(0.0, 1.0);
        let handle_x = track_rect.min.x + t * track_rect.width();
        if t > 0.0 {
            let sub_rect = Rect::from_min_max(
                track_rect.min,
                Pos2::new(handle_x, track_rect.max.y),
            );
            painter.rect_filled(sub_rect, Rounding::same(2.5), Palette::ACCENT_PINK);
        }

        let center = Pos2::new(handle_x, track_y);
        painter.circle_filled(center, 6.2, Color32::WHITE);
        painter.circle_stroke(center, 6.2, Stroke::new(1.8_f32, Palette::ACCENT_PINK));
    }

    changed
}

/// Gradasi warna VU & DSP Meter: Hijau (0-60%) -> Kuning Amber (60-82%) -> Merah Peak (>82%)
fn meter_gradient_color(t: f32) -> (u8, u8, u8) {
    if t < 0.60 {
        let rel = t / 0.60;
        let r = (35.0 + (175.0 - 35.0) * rel) as i32;
        let g = (210.0 + (225.0 - 210.0) * rel) as i32;
        let b = (90.0 - 50.0 * rel) as i32;
        (r.clamp(0, 255) as u8, g.clamp(0, 255) as u8, b.clamp(0, 255) as u8)
    } else if t < 0.82 {
        let rel = (t - 0.60) / 0.22;
        let r = (175.0 + (255.0 - 175.0) * rel) as i32;
        let g = (225.0 - 95.0 * rel) as i32;
        let b = (40.0 - 20.0 * rel) as i32;
        (r.clamp(0, 255) as u8, g.clamp(0, 255) as u8, b.clamp(0, 255) as u8)
    } else {
        let rel = ((t - 0.82) / 0.18).min(1.0);
        let r = 255;
        let g = (130.0 - 105.0 * rel) as i32;
        let b = (20.0 + 25.0 * rel) as i32;
        (r, g.clamp(0, 255) as u8, b.clamp(0, 255) as u8)
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_diagonal_meter(
    ui: &mut egui::Ui,
    size: Vec2,
    level: f32,
    seg_w_target: f32,
    gap: f32,
    slant: f32,
    margin_x: f32,
    margin_y: f32,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    if !ui.is_rect_visible(rect) {
        return response;
    }

    let painter = ui.painter();
    painter.rect(
        rect,
        Rounding::same(4.0),
        Palette::BG_INPUT,
        Stroke::new(1.0_f32, Palette::BORDER),
    );

    let usable_w = rect.width() - (2.0 * margin_x) - slant;
    if usable_w <= 4.0 {
        return response;
    }

    let num_bars = ((usable_w / (seg_w_target + gap)) as usize).max(8);
    let actual_step = usable_w / (num_bars as f32);
    let actual_seg_w = (actual_step - gap).max(2.5);
    let y_top = rect.min.y + margin_y;
    let y_bot = rect.max.y - margin_y;
    let clamped_level = level.clamp(0.0, 1.0);

    for i in 0..num_bars {
        let t = (i + 1) as f32 / (num_bars as f32);
        let x_bot = rect.min.x + margin_x + (i as f32) * actual_step;

        let pts = vec![
            Pos2::new(x_bot + slant, y_top),
            Pos2::new(x_bot + slant + actual_seg_w, y_top),
            Pos2::new(x_bot + actual_seg_w, y_bot),
            Pos2::new(x_bot, y_bot),
        ];

        let (r, g, b) = meter_gradient_color(t);
        let fill = if t <= clamped_level {
            Color32::from_rgb(r, g, b)
        } else {
            Color32::from_rgba_unmultiplied(r, g, b, 32)
        };

        painter.add(Shape::convex_polygon(pts, fill, Stroke::NONE));
    }

    response
}

/// Dropdown perangkat bergaya PyQt6 (`#1E1E1E` dengan ikon bulat merah `🚫` saat kosong).
fn draw_v122_device_dropdown(
    ui: &mut egui::Ui,
    id_str: &str,
    selected: &mut String,
    none_label: &str,
    items: &[(String, String)],
    width: f32,
) -> bool {
    let prev = selected.clone();
    let display = if selected.is_empty() {
        format!("🚫 {}", none_label)
    } else {
        items
            .iter()
            .find(|(val, _)| val == selected)
            .map(|(_, disp)| disp.clone())
            .unwrap_or_else(|| selected.clone())
    };

    egui::ComboBox::from_id_source(id_str)
        .selected_text(display)
        .width(width.max(80.0))
        .show_ui(ui, |ui| {
            ui.selectable_value(selected, String::new(), format!("🚫 {}", none_label));
            for (val, disp) in items {
                ui.selectable_value(selected, val.clone(), disp.clone());
            }
        });

    *selected != prev
}

// ==============================================================================
// 3. STATE APLIKASI UTAMA (`RinkaSoundBridgeApp`)
// ==============================================================================

#[derive(Clone, Copy, PartialEq, Eq)]
enum ActiveLineTarget {
    LineA,
    LineB,
}

struct VstPickerModal {
    open: bool,
    target_line: ActiveLineTarget,
    search_query: String,
    selected_path: Option<String>,
}

pub struct RinkaSoundBridgeApp {
    wasapi_inputs: Vec<WasapiDeviceInfo>,
    wasapi_outputs: Vec<WasapiDeviceInfo>,
    asio_drivers: Vec<AsioDriverInfo>,

    pipeline_a: AudioPipeline,
    pipeline_b: AudioPipeline,

    vol_a: i32,
    vol_b: i32,
    ov_in_labels_a: Vec<String>,
    ov_out_labels_a: Vec<String>,
    ov_in_labels_b: Vec<String>,
    ov_out_labels_b: Vec<String>,

    scanned_vst_cache: BTreeMap<String, String>,
    vst_picker: VstPickerModal,
    asio_setting_open: bool,
    dsp_rsb_setting_open: bool,
    rsb_asio_setting_open: bool,
    sync_triple_asio: bool,
    asio_vc_profile: FlexAsioProfileConfig,
    dsp_rsb_config: DspRsbAsioConfig,
    rsb_asio_config: RsbAsioConfig,

    last_watchdog_tick: Instant,
    last_perf_tick: Instant,
    last_latency_tick: Instant,
    save_pending_since: Option<Instant>,

    perf_monitor: ProcessPerfMonitor,
    tray_manager: SystemTrayManager,
    cpu_pct: f32,
    ram_mb: u64,

    lat_current_ms: Option<f64>,
    lat_peak_ms: Option<f64>,
    lat_last_updated: Instant,
    lat_last_auto_reset: Instant,
    show_delay_bar: bool,

    win_x: Option<i32>,
    win_y: Option<i32>,
    win_w: Option<u32>,
    win_h: Option<u32>,
    win_pos_apply_frames: u8,
    is_shut_down: bool,
    gpu_boost_lock_enabled: bool,
    gpu_boost_lock_ui_open: bool,
    gpu_boost_lock_mhz: u32,
    gpu_boost_lock_failsafe: bool,
    gpu_telemetry: GpuTelemetrySnapshot,
    asio_notes_text: String,
}

impl RinkaSoundBridgeApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        capture_host_gl_context();
        Self::configure_fonts(&cc.egui_ctx);
        Self::configure_theme(&cc.egui_ctx);

        // Cek requirement FlexASIO & ASIO saat .exe dijalankan:
        // - Jika sudah terinstal di PC: langsung lanjut dalam <0.1 ms!
        // - Jika belum ada: otomatis unduh & instal lalu daftarkan driver ASIO sebelum memindai perangkat.
        ensure_asio_requirements_installed(false);

        let (wasapi_inputs, wasapi_outputs) = get_wasapi_devices();
        let asio_drivers = get_asio_devices();
        let scanned_vst_cache = VSTScanner::scan_plugins();
        let mut perf_monitor = ProcessPerfMonitor::new();
        let (cpu_pct, ram_mb) = perf_monitor.sample();

        let mut app = Self {
            wasapi_inputs,
            wasapi_outputs,
            asio_drivers,
            pipeline_a: AudioPipeline::new("Line A"),
            pipeline_b: AudioPipeline::new("Line B"),
            vol_a: 100,
            vol_b: 100,
            ov_in_labels_a: vec!["(no file loaded)".to_string()],
            ov_out_labels_a: vec!["(no file loaded)".to_string()],
            ov_in_labels_b: vec!["(no file loaded)".to_string()],
            ov_out_labels_b: vec!["(no file loaded)".to_string()],
            scanned_vst_cache,
            vst_picker: VstPickerModal {
                open: false,
                target_line: ActiveLineTarget::LineA,
                search_query: String::new(),
                selected_path: None,
            },
            asio_setting_open: false,
            dsp_rsb_setting_open: false,
            rsb_asio_setting_open: false,
            sync_triple_asio: true,
            asio_vc_profile: FlexAsioProfileConfig::default(),
            dsp_rsb_config: DspRsbAsioConfig::default(),
            rsb_asio_config: RsbAsioConfig::default(),
            gpu_boost_lock_enabled: false,
            gpu_boost_lock_ui_open: false,
            gpu_boost_lock_mhz: 1980,
            gpu_boost_lock_failsafe: true,
            gpu_telemetry: poll_gpu_clock_telemetry_with_failsafe(true),
            asio_notes_text: load_or_create_asio_notes_txt(),
            last_watchdog_tick: Instant::now(),
            last_perf_tick: Instant::now(),
            last_latency_tick: Instant::now(),
            save_pending_since: None,
            perf_monitor,
            tray_manager: SystemTrayManager::new_with_icon(&APP_ICON_RGBA),
            cpu_pct,
            ram_mb,
            lat_current_ms: None,
            lat_peak_ms: None,
            lat_last_updated: Instant::now(),
            lat_last_auto_reset: Instant::now(),
            show_delay_bar: true,
            win_x: None,
            win_y: None,
            win_w: Some(1020),
            win_h: Some(940),
            win_pos_apply_frames: 0,
            is_shut_down: false,
        };

        app.load_config_on_startup();
        app.asio_drivers = get_asio_devices();
        if boost_process_priority() {
            log_info("Process priority raised to Above Normal & EcoQoS Throttling disabled");
        }
        trim_process_working_set();
        app
    }

    fn configure_fonts(ctx: &egui::Context) {
        let mut fonts = FontDefinitions::default();
        if let Ok(segoe) = std::fs::read("C:\\Windows\\Fonts\\segoeui.ttf") {
            fonts
                .font_data
                .insert("segoeui".to_owned(), FontData::from_owned(segoe));
            fonts
                .families
                .entry(FontFamily::Proportional)
                .or_default()
                .insert(0, "segoeui".to_owned());
        }
        if let Ok(sym) = std::fs::read("C:\\Windows\\Fonts\\seguisym.ttf") {
            fonts
                .font_data
                .insert("seguisym".to_owned(), FontData::from_owned(sym));
            fonts
                .families
                .entry(FontFamily::Proportional)
                .or_default()
                .push("seguisym".to_owned());
        }
        if let Ok(emj) = std::fs::read("C:\\Windows\\Fonts\\seguiemj.ttf") {
            fonts
                .font_data
                .insert("seguiemj".to_owned(), FontData::from_owned(emj));
            fonts
                .families
                .entry(FontFamily::Proportional)
                .or_default()
                .push("seguiemj".to_owned());
        }

        ctx.set_fonts(fonts);
    }

    fn configure_theme(ctx: &egui::Context) {
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = Palette::BG_APP;
        visuals.window_fill = Palette::BG_PANEL;
        visuals.faint_bg_color = Palette::BG_PANEL_ALT;
        visuals.extreme_bg_color = Palette::BG_INPUT;
        visuals.widgets.noninteractive.bg_fill = Palette::BG_PANEL;
        visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, Palette::BORDER);
        visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, Palette::TEXT_PRIMARY);
        visuals.widgets.inactive.bg_fill = Palette::BG_INPUT;
        visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, Palette::BORDER);
        visuals.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, Palette::TEXT_PRIMARY);
        visuals.widgets.hovered.bg_fill = Palette::ACCENT_PINK_SOFT;
        visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, Palette::ACCENT_PINK);
        visuals.widgets.active.bg_fill = Palette::ACCENT_BLUE_DEEP;
        ctx.set_visuals(visuals);
    }

    fn trigger_auto_save(&mut self) {
        self.save_pending_since = Some(Instant::now());
    }

    /// Sinkronkan ke-3 ASIO (Line A, Line B, dan "Rinka SoundBridge ASIO" khusus Output Line A & Input Line B)
    /// agar selalu mengikuti 1 setting terpadu (smp, Sample Rate, Set In, Set Out, dan perangkat yang dipilih).
    /// Menjaga `BUS_DSP_B_IN` di Shared Memory selalu terkunci 100% pada `self.dsp_rsb_config` milik user
    /// di setiap frame sehingga tidak pernah bisa kembali ke 90ms/100ms saat Vonovox di-stop/start!
    fn enforce_live_dsp_b_cushion(&self) {
        if self.pipeline_a.driver_backend == DriverBackend::RsbAsio {
            return;
        }
        let dsp_sr = self.pipeline_a.sample_rate.max(44100);
        let dsp_smp = self
            .dsp_rsb_config
            .effective_buffer_size(self.pipeline_a.buffer_size);
        let eff_cush_ms = self.dsp_rsb_config.dsp_b_cushion_ms.clamp(20.0, 450.0);
        let dsp_cush_smp = (((eff_cush_ms as f64) / 1000.0) * (dsp_sr as f64)).round() as u32;
        get_dsp_rsb_bus_b_in().enforce_rsb_dsp_b_config(
            dsp_sr,
            dsp_smp,
            dsp_cush_smp.min(24000),
            self.dsp_rsb_config.adaptive_cushion,
            self.dsp_rsb_config.adaptive_reported_latency,
            self.dsp_rsb_config.gpu_stall_multiplier,
            self.dsp_rsb_config.adaptive_mode as u8,
            self.dsp_rsb_config.adaptive_extra_ms,
        );
    }

    fn sync_unified_asio_config(&self, request_admin: bool) -> Result<String, String> {
        let reg_settings = AsioRegistrationSettings {
            sample_rate: self.pipeline_a.sample_rate,
            line_a: self.pipeline_a.asio_profile.clone(),
            line_b: self.pipeline_b.asio_profile.clone(),
            vc: self.asio_vc_profile.clone(),
            dsp_rsb: self.dsp_rsb_config.clone(),
            rsb_asio: self.rsb_asio_config.clone(),
        };
        register_virtual_asio_drivers_with_settings(request_admin, Some(&reg_settings))
    }

    fn serialize_line(pipeline: &AudioPipeline, vol: i32) -> LineConfig {
        let serialize_bank =
            |bank: &rinka_soundbridge_core::dsp::OverlayBank| -> Vec<OverlaySlotConfig> {
                bank.players
                    .iter()
                    .map(|p| OverlaySlotConfig {
                        enabled: p.is_enabled(),
                        file_path: p.file_path(),
                        gain: p.gain(),
                    })
                    .collect()
            };

        let vst_slots = pipeline
            .vst_slots
            .iter()
            .map(|s| VstSlotConfig {
                path: s.file_path.clone(),
                bypassed: s.is_bypassed(),
                params: s.cached_params.lock().clone(),
                component_state_hex: s.cached_comp_state.lock().clone(),
                controller_state_hex: s.cached_ctrl_state.lock().clone(),
            })
            .collect();

        LineConfig {
            is_enabled: pipeline.is_enabled(),
            driver_mode: pipeline.driver_backend.as_str().to_string(),
            input_device_name: pipeline.input_device_name.clone(),
            output_device_name: pipeline.output_device_name.clone(),
            asio_driver_name: pipeline.asio_driver_name.clone(),
            asio_exp_in_samples: pipeline.asio_exp_in_samples,
            asio_exp_out_samples: pipeline.asio_exp_out_samples,
            master_volume: vol,
            is_muted: pipeline.is_muted(),
            fail_closed: pipeline.fail_closed(),
            dc_blocker: pipeline.dc_blocker_enabled(),
            overlay_input: serialize_bank(&pipeline.shared.overlay_input_bank),
            overlay_output: serialize_bank(&pipeline.shared.overlay_output_bank),
            vst_slots,
        }
    }

    fn save_config_now(&mut self) {
        self.save_pending_since = None;
        if let Some((x, y, w, h)) = get_main_window_rect(APP_WINDOW_TITLE) {
            self.win_x = Some(x);
            self.win_y = Some(y);
            self.win_w = Some(w);
            self.win_h = Some(h);
        }
        let mut cfg = AppConfig::default();
        cfg.global.buffer_size = self.pipeline_a.buffer_size;
        cfg.global.sample_rate = self.pipeline_a.sample_rate;
        cfg.global.fifo_mode = self.pipeline_a.fifo_mode.clone();
        cfg.global.audio_priority = get_audio_priority_boost();
        cfg.global.show_delay_bar = self.show_delay_bar;
        cfg.global.sync_triple_asio = self.sync_triple_asio;
        cfg.global.asio_set_in_enabled = self.pipeline_a.asio_profile.set_in_latency_enabled;
        cfg.global.asio_set_in_latency = self.pipeline_a.asio_profile.in_latency_val;
        cfg.global.asio_set_out_enabled = self.pipeline_a.asio_profile.set_out_latency_enabled;
        cfg.global.asio_set_out_latency = self.pipeline_a.asio_profile.out_latency_val;
        cfg.global.window_x = self.win_x;
        cfg.global.window_y = self.win_y;
        cfg.global.window_w = self.win_w;
        cfg.global.window_h = self.win_h;
        cfg.global.gpu_boost_lock_enabled = self.gpu_boost_lock_enabled;
        cfg.global.gpu_boost_lock_mhz = self.gpu_telemetry.max_graphics_mhz.clamp(900, 3300);
        cfg.global.gpu_boost_lock_failsafe = self.gpu_boost_lock_failsafe;

        cfg.line_a = Self::serialize_line(&self.pipeline_a, self.vol_a);
        cfg.line_b = Self::serialize_line(&self.pipeline_b, self.vol_b);
        cfg.asio_line_a = self.pipeline_a.asio_profile.clone();
        cfg.asio_line_b = self.pipeline_b.asio_profile.clone();
        cfg.asio_vc = self.asio_vc_profile.clone();
        cfg.asio_dsp_rsb = self.dsp_rsb_config.clone();
        cfg.rsb_asio = self.rsb_asio_config.clone();
        cfg.save();
    }

    fn load_config_on_startup(&mut self) {
        let Some(cfg) = AppConfig::load() else {
            return;
        };

        let buf = cfg.global.buffer_size;
        let sr = cfg.global.sample_rate;
        set_audio_priority_boost(cfg.global.audio_priority);
        self.show_delay_bar = cfg.global.show_delay_bar;
        self.win_x = cfg.global.window_x;
        self.win_y = cfg.global.window_y;
        self.win_w = cfg.global.window_w.or(Some(1020));
        self.win_h = cfg.global.window_h.or(Some(940));
        if self.win_x.is_some() && self.win_y.is_some() {
            self.win_pos_apply_frames = 3;
        }

        for p in [&mut self.pipeline_a, &mut self.pipeline_b] {
            p.buffer_size = buf;
            p.asio_exp_in_samples = buf;
            p.asio_exp_out_samples = buf;
            p.sample_rate = sr;
            p.asio_set_in_enabled = cfg.global.asio_set_in_enabled;
            p.asio_set_in_latency = cfg.global.asio_set_in_latency;
            p.asio_set_out_enabled = cfg.global.asio_set_out_enabled;
            p.asio_set_out_latency = cfg.global.asio_set_out_latency;
            p.set_fifo_mode(&cfg.global.fifo_mode);
        }

        self.sync_triple_asio = cfg.global.sync_triple_asio;
        self.gpu_boost_lock_enabled = cfg.global.gpu_boost_lock_enabled;
        self.gpu_boost_lock_mhz = cfg.global.gpu_boost_lock_mhz.clamp(900, 3300);
        self.gpu_boost_lock_failsafe = cfg.global.gpu_boost_lock_failsafe;
        if self.gpu_boost_lock_enabled {
            if set_gpu_boost_clock_lock_with_failsafe(true, self.gpu_boost_lock_failsafe).is_err() {
                self.gpu_boost_lock_enabled = false;
            }
            self.gpu_telemetry = poll_gpu_clock_telemetry_with_failsafe(self.gpu_boost_lock_failsafe);
        }

        Self::restore_line_state(
            &mut self.pipeline_a,
            &mut self.vol_a,
            &mut self.ov_in_labels_a,
            &mut self.ov_out_labels_a,
            &cfg.line_a,
        );
        Self::restore_line_state(
            &mut self.pipeline_b,
            &mut self.vol_b,
            &mut self.ov_in_labels_b,
            &mut self.ov_out_labels_b,
            &cfg.line_b,
        );

        self.pipeline_a.asio_profile = cfg.asio_line_a.clone();
        if self.pipeline_a.asio_profile.input_device.is_empty() {
            if let Some(ref d) = self.pipeline_a.input_device_name {
                self.pipeline_a.asio_profile.input_device = d.clone();
            }
        }
        if self.pipeline_a.asio_profile.output_device.is_empty() {
            if let Some(ref d) = self.pipeline_a.output_device_name {
                self.pipeline_a.asio_profile.output_device = d.clone();
            }
        }

        self.pipeline_b.asio_profile = cfg.asio_line_b.clone();
        if self.pipeline_b.asio_profile.input_device.is_empty() {
            if let Some(ref d) = self.pipeline_b.input_device_name {
                self.pipeline_b.asio_profile.input_device = d.clone();
            }
        }
        if self.pipeline_b.asio_profile.output_device.is_empty() {
            if let Some(ref d) = self.pipeline_b.output_device_name {
                self.pipeline_b.asio_profile.output_device = d.clone();
            }
        }

        self.asio_vc_profile = cfg.asio_vc.clone();
        let (toml_in, toml_out) = read_user_flexasio_toml_devices();
        let is_swapped_legacy = (!self.asio_vc_profile.input_device.is_empty()
            && self.asio_vc_profile.input_device == self.pipeline_b.asio_profile.input_device
            && self.asio_vc_profile.output_device == self.pipeline_a.asio_profile.output_device)
            || (self.asio_vc_profile.input_device.to_lowercase().contains("loopback 2")
                && self.asio_vc_profile.output_device.to_lowercase().contains("output 1/2"));

        if self.asio_vc_profile.input_device.is_empty() || is_swapped_legacy {
            if let Some(found_lb1) = self
                .wasapi_inputs
                .iter()
                .find(|d| d.name.to_lowercase().contains("loopback 1"))
                .map(|d| d.name.clone())
            {
                self.asio_vc_profile.input_device = found_lb1;
            } else if let Some(ti) = toml_in {
                self.asio_vc_profile.input_device = ti;
            }
        }
        if self.asio_vc_profile.output_device.is_empty() || is_swapped_legacy {
            if let Some(found_out34) = self
                .wasapi_outputs
                .iter()
                .find(|d| d.name.to_lowercase().contains("output 3/4"))
                .map(|d| d.name.clone())
            {
                self.asio_vc_profile.output_device = found_out34;
            } else if let Some(to) = toml_out {
                self.asio_vc_profile.output_device = to;
            }
        }

        if self.sync_triple_asio {
            let unified_smp = self.pipeline_a.asio_profile.buffer_size.clamp(32, 2048);
            self.pipeline_b.asio_profile.buffer_size = unified_smp;
            self.asio_vc_profile.buffer_size = unified_smp;
            self.pipeline_a.buffer_size = unified_smp;
            self.pipeline_b.buffer_size = unified_smp;
        }

        self.dsp_rsb_config = cfg.asio_dsp_rsb.clone();
        self.dsp_rsb_config.dsp_b_cushion_ms =
            self.dsp_rsb_config.dsp_b_cushion_ms.clamp(20.0, 450.0);
        self.dsp_rsb_config.adaptive_extra_ms =
            self.dsp_rsb_config.adaptive_extra_ms.clamp(0.0, 250.0);
        self.dsp_rsb_config.gpu_stall_multiplier =
            self.dsp_rsb_config.gpu_stall_multiplier.clamp(1.00, 1.30);
        self.dsp_rsb_config.sync_sample_rate_with_rsb = true;
        self.dsp_rsb_config.buffer_size = self
            .dsp_rsb_config
            .effective_buffer_size(self.pipeline_a.buffer_size);
        set_asio_cpu_cores(self.dsp_rsb_config.asio_cpu_cores);
        self.pipeline_a.dsp_rsb_config = self.dsp_rsb_config.clone();
        self.pipeline_b.dsp_rsb_config = self.dsp_rsb_config.clone();

        self.rsb_asio_config = cfg.rsb_asio.clone();
        if self.rsb_asio_config.input_device.is_empty() {
            if let Some(ref d) = self.pipeline_a.input_device_name {
                self.rsb_asio_config.input_device = d.clone();
            }
        }
        if self.rsb_asio_config.output_device.is_empty() {
            if let Some(ref d) = self.pipeline_b.output_device_name {
                self.rsb_asio_config.output_device = d.clone();
            }
        }
        self.rsb_asio_config.buffer_size = self
            .rsb_asio_config
            .effective_buffer_size(self.pipeline_a.buffer_size);
        self.pipeline_a.rsb_asio_config = self.rsb_asio_config.clone();
        self.pipeline_b.rsb_asio_config = self.rsb_asio_config.clone();

        let _ = self.sync_unified_asio_config(true);
        get_dsp_rsb_bus_b_in().reset_spike_and_resync_cushion();
        rinka_soundbridge_core::asio_host::inject_probe_into_running_vonovox_if_needed();
        self.pipeline_a.restart_stream();
        self.pipeline_b.restart_stream();
    }

    fn restore_line_state(
        pipeline: &mut AudioPipeline,
        vol_out: &mut i32,
        ov_in_labels: &mut Vec<String>,
        ov_out_labels: &mut Vec<String>,
        data: &LineConfig,
    ) {
        *vol_out = data.master_volume.clamp(0, 150);
        pipeline.set_master_gain(*vol_out as f32 / 100.0);
        pipeline.set_muted(data.is_muted);
        pipeline.set_fail_closed(data.fail_closed);
        pipeline.set_dc_blocker_enabled(data.dc_blocker);
        pipeline.driver_backend = DriverBackend::from_str(&data.driver_mode);
        pipeline.input_device_name = data
            .input_device_name
            .as_deref()
            .map(normalize_virtual_device_name);
        pipeline.output_device_name = data
            .output_device_name
            .as_deref()
            .map(normalize_virtual_device_name);
        pipeline.asio_driver_name = data.asio_driver_name.clone();
        pipeline.asio_exp_in_samples = data.asio_exp_in_samples.clamp(32, 1024);
        pipeline.asio_exp_out_samples = data.asio_exp_out_samples.clamp(32, 1024);
        pipeline
            .shared
            .is_enabled
            .store(data.is_enabled, std::sync::atomic::Ordering::Relaxed);

        let sr = pipeline.effective_sample_rate();
        for (bank, cfg_list, labels) in [
            (&pipeline.shared.overlay_input_bank, &data.overlay_input, ov_in_labels),
            (&pipeline.shared.overlay_output_bank, &data.overlay_output, ov_out_labels),
        ] {
            for (idx, (player, item)) in bank.players.iter().zip(cfg_list.iter()).enumerate() {
                player.set_gain(item.gain);
                if let Some(ref p) = item.file_path {
                    if Path::new(p).exists() {
                        match player.load_file(p, sr) {
                            Ok(dur) => {
                                let fname = Path::new(p)
                                    .file_name()
                                    .and_then(|s| s.to_str())
                                    .unwrap_or(p);
                                if idx < labels.len() {
                                    labels[idx] = format!("{} ({:.1}s)", fname, dur);
                                }
                            }
                            Err(_) => {
                                if idx < labels.len() {
                                    labels[idx] = "Failed to load file!".to_string();
                                }
                            }
                        }
                    }
                }
                player.set_enabled(item.enabled);
            }
        }

        for item in &data.vst_slots {
            if Path::new(&item.path).exists() {
                match pipeline.add_vst(&item.path) {
                    Ok(slot) => {
                        slot.set_bypassed(item.bypassed);
                        if !item.params.is_empty()
                            || item.component_state_hex.is_some()
                            || item.controller_state_hex.is_some()
                        {
                            slot.restore_full_state(
                                &item.params,
                                item.component_state_hex.as_deref(),
                                item.controller_state_hex.as_deref(),
                            );
                        }
                    }
                    Err(e) => {
                        log_error(format!("Failed to restore VST {}: {}", item.path, e));
                    }
                }
            }
        }
    }

    fn refresh_audio_devices(&mut self) {
        self.pipeline_a.stop_stream();
        self.pipeline_b.stop_stream();
        std::thread::sleep(Duration::from_millis(120));

        let (ins, outs) = get_wasapi_devices();
        self.wasapi_inputs = ins;
        self.wasapi_outputs = outs;
        self.asio_drivers = get_asio_devices();

        get_dsp_rsb_bus_b_in().reset_spike_and_resync_cushion();
        rinka_soundbridge_core::asio_host::inject_probe_into_running_vonovox_if_needed();
        self.pipeline_a.restart_stream();
        self.pipeline_b.restart_stream();
        self.lat_peak_ms = None;
        self.lat_current_ms = None;

        log_info(format!(
            "Audio devices refreshed: {} WASAPI In, {} WASAPI Out, {} ASIO Drivers.",
            self.wasapi_inputs.len(),
            self.wasapi_outputs.len(),
            self.asio_drivers.len()
        ));
    }

    fn clean_shutdown(&mut self) {
        if self.is_shut_down {
            return;
        }
        self.is_shut_down = true;
        // Auto-Reset & Auto-Off GPU Boost Lock saat Rinka SoundBridge ditutup / dimatikan
        if self.gpu_boost_lock_enabled || self.gpu_telemetry.is_boost_locked {
            self.gpu_boost_lock_enabled = false;
            self.gpu_telemetry.is_boost_locked = false;
            reset_gpu_boost_clock_lock_on_exit();
            log_info("[GPU Boost Lock] Auto-reset & disabled on application exit.");
        } else {
            reset_gpu_boost_clock_lock_on_exit();
        }
        self.tray_manager.remove_tray_icon();
        self.pipeline_a.stop_stream();
        self.pipeline_b.stop_stream();
        for p in [&self.pipeline_a, &self.pipeline_b] {
            for slot in &p.vst_slots {
                slot.sync_cached_params();
            }
        }
        self.save_config_now();
        for p in [&self.pipeline_a, &self.pipeline_b] {
            for slot in &p.vst_slots {
                slot.shutdown_and_release();
            }
        }
        release_single_instance();
        shutdown_logger();
    }

    fn reload_application(&mut self, ctx: &egui::Context) {
        get_dsp_rsb_bus_b_in().reset_spike_and_resync_cushion();
        self.clean_shutdown();
        if let Ok(exe) = std::env::current_exe() {
            let _ = Command::new(exe).arg("--restarted").spawn();
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    fn periodic_background_tasks(&mut self, _ctx: &egui::Context) {
        self.enforce_live_dsp_b_cushion();
        rinka_soundbridge_core::asio_host::inject_probe_into_running_vonovox_if_needed();
        let (restored_from_tray, quit_from_tray) = self.tray_manager.poll_events();
        if quit_from_tray || is_tray_quit_requested() {
            self.clean_shutdown();
            std::process::exit(0);
        }
        if restored_from_tray {
            self.win_pos_apply_frames = 2;
        }

        let mut any_editor_closed = false;
        for p in [&self.pipeline_a, &self.pipeline_b] {
            for slot in &p.vst_slots {
                if slot.poll_editor_closed() {
                    any_editor_closed = true;
                }
            }
        }
        if any_editor_closed {
            self.trigger_auto_save();
        }

        let now = Instant::now();

        if let Some(t_save) = self.save_pending_since {
            if now.saturating_duration_since(t_save) >= Duration::from_millis(400) {
                self.save_config_now();
            }
        }

        if self.win_pos_apply_frames > 0 {
            self.win_pos_apply_frames -= 1;
            if let (Some(x), Some(y)) = (self.win_x, self.win_y) {
                let _ = restore_main_window_pos(APP_WINDOW_TITLE, x, y, self.win_w, self.win_h);
            }
        }

        if now.saturating_duration_since(self.last_perf_tick) >= Duration::from_secs(1) {
            self.last_perf_tick = now;
            let (cpu, ram) = self.perf_monitor.sample();
            self.cpu_pct = cpu;
            self.ram_mb = ram;
            self.gpu_telemetry = poll_gpu_clock_telemetry_with_failsafe(self.gpu_boost_lock_failsafe);
            if self.gpu_boost_lock_enabled && !self.gpu_telemetry.is_boost_locked {
                self.gpu_boost_lock_enabled = false;
                self.trigger_auto_save();
                if let Some(alert) = take_gpu_failsafe_alert() {
                    show_info_message_box("GPU Boost Lock — Failsafe Auto-Canceled", &alert);
                }
            }

            // Pantau jika user menggeser atau mengubah ukuran jendela (mis. taruh di separuh kiri layar)
            if let Some((x, y, w, h)) = get_main_window_rect(APP_WINDOW_TITLE) {
                if self.win_x != Some(x)
                    || self.win_y != Some(y)
                    || self.win_w != Some(w)
                    || self.win_h != Some(h)
                {
                    self.win_x = Some(x);
                    self.win_y = Some(y);
                    self.win_w = Some(w);
                    self.win_h = Some(h);
                    self.trigger_auto_save();
                }
            }
        }

        if now.saturating_duration_since(self.last_watchdog_tick) >= Duration::from_secs(2) {
            self.last_watchdog_tick = now;
            let _ = self.pipeline_a.check_stream_health();
            let _ = self.pipeline_b.check_stream_health();
            asio_diag_poll();
        }

        if now.saturating_duration_since(self.last_latency_tick) >= Duration::from_millis(1000) {
            self.last_latency_tick = now;
            // Auto-reset angka ms delay In A -> Out B setiap 10 detik agar selalu menampilkan pembacaan segar!
            if now.saturating_duration_since(self.lat_last_auto_reset) >= Duration::from_secs(10) {
                self.lat_last_auto_reset = now;
                self.lat_peak_ms = None;
                self.lat_current_ms = None;
            }
            if self.pipeline_a.is_enabled()
                && self.pipeline_a.streams_running()
                && self.pipeline_b.is_enabled()
                && self.pipeline_b.streams_running()
            {
                if let Some(delay) = estimate_latency_ms(
                    &self.pipeline_a.shared.probe_in,
                    &self.pipeline_b.shared.probe_out,
                    1500.0,
                ) {
                    self.lat_current_ms = Some(delay);
                    let new_peak = match self.lat_peak_ms {
                        Some(p) if (delay - p).abs() > 180.0 => delay,
                        Some(p) => p * 0.65 + delay * 0.35,
                        None => delay,
                    };
                    self.lat_peak_ms = Some(new_peak);
                    self.lat_last_updated = now;
                } else if now.saturating_duration_since(self.lat_last_updated) >= Duration::from_secs(8) {
                    self.lat_current_ms = None;
                }
            } else {
                self.lat_current_ms = None;
                self.lat_peak_ms = None;
            }
        }
    }

    // ==========================================================================
    // RENDERING UI KOMPONEN (REPLIKA PRESISI v1.22)
    // ==========================================================================

    fn render_header(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        egui::Frame::none()
            .fill(Palette::BG_PANEL_ALT)
            .stroke(Stroke::new(1.0_f32, Palette::BORDER))
            .rounding(Rounding::same(12.0))
            .inner_margin(Margin::symmetric(12.0, 10.0))
            .show(ui, |ui| {
                // BARIS 1: Pill CPU/RAM di Kiri | Buffer Size & Sample Rate di Kanan
                ui.horizontal(|ui| {
                    egui::Frame::none()
                        .fill(Palette::BG_PANEL)
                        .stroke(Stroke::new(1.0_f32, Palette::BORDER))
                        .rounding(Rounding::same(8.0))
                        .inner_margin(Margin::symmetric(10.0, 5.0))
                        .show(ui, |ui| {
                            let (icon_rect, _) =
                                ui.allocate_exact_size(Vec2::new(16.0, 16.0), Sense::hover());
                            paint_monitor_icon(&ui.painter(), icon_rect.center());
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new(format!(
                                    "CPU: {:.1}%  |  RAM: {} MB  |",
                                    self.cpu_pct, self.ram_mb
                                ))
                                .font(FontId::new(12.5, FontFamily::Proportional))
                                .strong()
                                .color(Palette::ACCENT_GREEN_DEEP),
                            );
                            let (b_rect, b_resp) =
                                ui.allocate_exact_size(Vec2::new(12.0, 15.0), Sense::hover());
                            paint_bolt_icon(&ui.painter(), b_rect.center(), Palette::ACCENT_GREEN);
                            b_resp.on_hover_text(
                                "FTZ/DAZ Hardware Denormal Flush + Windows 11 EcoQoS Power Throttling Guard Active",
                            );
                            ui.label(
                                RichText::new("FTZ/DAZ + EcoQoS Guard Active")
                                    .font(FontId::new(12.0, FontFamily::Proportional))
                                    .strong()
                                    .color(Palette::ACCENT_GREEN_DEEP),
                            )
                            .on_hover_text(
                                "FTZ/DAZ prevents CPU spikes during silence in VST3 plugins, and EcoQoS Guard prevents Windows 11 from throttling performance when minimized.",
                            );
                        });

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let mut sr = self.pipeline_a.sample_rate;
                        egui::ComboBox::from_id_source("cb_sr")
                            .selected_text(format!("{} Hz", sr))
                            .width(105.0)
                            .show_ui(ui, |ui| {
                                for cand in [44100u32, 48000u32, 96000u32] {
                                    ui.selectable_value(&mut sr, cand, format!("{} Hz", cand));
                                }
                            });

                        egui::Frame::none()
                            .fill(Palette::BG_PANEL)
                            .stroke(Stroke::new(1.0_f32, Palette::BORDER))
                            .rounding(Rounding::same(6.0))
                            .inner_margin(Margin::symmetric(8.0, 4.0))
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new("Sample Rate:")
                                        .size(12.0)
                                        .color(Palette::TEXT_SECONDARY),
                                );
                            });

                        ui.add_space(8.0);

                        let mut buf = self.pipeline_a.buffer_size;
                        let cur_ms = (buf as f64 / (sr.max(44100) as f64)) * 1000.0;
                        egui::ComboBox::from_id_source("cb_buf")
                            .selected_text(format!("{} Samples ({:.1} ms)", buf, cur_ms))
                            .width(175.0)
                            .show_ui(ui, |ui| {
                                for cand in [32u32, 64, 128, 256, 512, 1024, 2048] {
                                    let ms = (cand as f64 / (sr.max(44100) as f64)) * 1000.0;
                                    ui.selectable_value(
                                        &mut buf,
                                        cand,
                                        format!("{} Samples ({:.1} ms)", cand, ms),
                                    );
                                }
                            });

                        egui::Frame::none()
                            .fill(Palette::BG_PANEL)
                            .stroke(Stroke::new(1.0_f32, Palette::BORDER))
                            .rounding(Rounding::same(6.0))
                            .inner_margin(Margin::symmetric(8.0, 4.0))
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new("Buffer Size:")
                                        .size(12.0)
                                        .color(Palette::TEXT_SECONDARY),
                                );
                            });

                        if buf != self.pipeline_a.buffer_size || sr != self.pipeline_a.sample_rate {
                            self.pipeline_a.buffer_size = buf;
                            self.pipeline_a.sample_rate = sr;
                            self.pipeline_b.buffer_size = buf;
                            self.pipeline_b.sample_rate = sr;
                            let exp_b = buf.clamp(32, 4096);
                            self.pipeline_a.asio_exp_in_samples = exp_b;
                            self.pipeline_a.asio_exp_out_samples = exp_b;
                            self.pipeline_b.asio_exp_in_samples = exp_b;
                            self.pipeline_b.asio_exp_out_samples = exp_b;
                            self.pipeline_a.asio_profile.buffer_size = exp_b;
                            self.pipeline_b.asio_profile.buffer_size = exp_b;
                            self.asio_vc_profile.buffer_size = exp_b;
                            self.dsp_rsb_config.sync_sample_rate_with_rsb = true;
                            self.dsp_rsb_config.buffer_size =
                                self.dsp_rsb_config.effective_buffer_size(exp_b);
                            self.pipeline_a.dsp_rsb_config = self.dsp_rsb_config.clone();
                            self.pipeline_b.dsp_rsb_config = self.dsp_rsb_config.clone();
                            self.rsb_asio_config.buffer_size =
                                self.rsb_asio_config.effective_buffer_size(exp_b);
                            self.pipeline_a.rsb_asio_config = self.rsb_asio_config.clone();
                            self.pipeline_b.rsb_asio_config = self.rsb_asio_config.clone();
                            self.pipeline_a.restart_stream();
                            self.pipeline_b.restart_stream();
                            let _ = self.sync_unified_asio_config(false);
                            self.trigger_auto_save();
                        }
                    });
                });

                ui.add_space(8.0);

                // BARIS 2: Dropdown Mode Driver (di posisi kotak kiri v1.22) + Kotak Putus-Putus 3 Tombol Utama
                ui.horizontal(|ui| {
                    // Kotak Dropdown Driver di sebelah kiri (menggantikan tempat dropdown tema di v1.22 agar rapi)
                    egui::Frame::none()
                        .fill(Palette::BG_INPUT)
                        .stroke(Stroke::new(1.0_f32, Palette::BORDER))
                        .rounding(Rounding::same(8.0))
                        .inner_margin(Margin::symmetric(8.0, 5.0))
                        .show(ui, |ui| {
                            let mut cur_drv = match self.pipeline_a.driver_backend {
                                DriverBackend::WasapiExclusive => DriverBackend::WasapiShared,
                                DriverBackend::AsioExperimental => DriverBackend::AsioDspRsb,
                                other => other,
                            };
                            let drv_text = match cur_drv {
                                DriverBackend::WasapiShared | DriverBackend::WasapiExclusive => {
                                    "Driver: 🔊 WASAPI Shared"
                                }
                                DriverBackend::AsioNative => "Driver: 🎹 ASIO Native",
                                DriverBackend::RsbAsio => "Driver: 🎛️ Rsb ASIO (Experimental)",
                                DriverBackend::AsioDspRsb | DriverBackend::AsioExperimental => {
                                    "Driver: ⚡ SoundBridge ASIO (Experimental)"
                                }
                            };
                            egui::ComboBox::from_id_source("cb_global_driver")
                                .selected_text(
                                    RichText::new(drv_text)
                                        .size(12.5)
                                        .strong()
                                        .color(match cur_drv {
                                            DriverBackend::RsbAsio => Palette::ACCENT_PINK,
                                            DriverBackend::AsioDspRsb
                                            | DriverBackend::AsioExperimental => Palette::ACCENT_GREEN,
                                            _ => Palette::TEXT_PRIMARY,
                                        }),
                                )
                                .width(250.0)
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(
                                        &mut cur_drv,
                                        DriverBackend::WasapiShared,
                                        "🔊 WASAPI Shared",
                                    )
                                    .on_hover_text(
                                        "WASAPI Shared (Default):
                                         Uses 10ms COM Event-Driven Bit-Exact Direct Audio + Tight Lock-Free FIFO.",
                                    );
                                    ui.selectable_value(
                                        &mut cur_drv,
                                        DriverBackend::AsioNative,
                                        "🎹 ASIO Native",
                                    );
                                    ui.selectable_value(
                                        &mut cur_drv,
                                        DriverBackend::RsbAsio,
                                        "🎛️ Rsb ASIO (Experimental)",
                                    )
                                    .on_hover_text(
                                        "Rsb ASIO (Experimental — Built-in FlexASIO without Virtual Cable):
                                         • Flow: WASAPI In A (Mic A) -> VST A -> ASIO Out A -> Vonovox ('ASIO: RSB ASIO') -> Loopback In B -> VST B -> WASAPI Out B (Speaker B).
                                         • 100% Bit-Exact 1:1 audio quality with Hardware-Clock-Slaved SPSC & Mode 4 Adaptive Cushion.",
                                    );
                                    ui.selectable_value(
                                        &mut cur_drv,
                                        DriverBackend::AsioDspRsb,
                                        "⚡ SoundBridge ASIO (Experimental)",
                                    )
                                    .on_hover_text(
                                        "SoundBridge ASIO (Experimental — Adaptive GPU Cushion Modes 1-5):
                                         • Uses 'ASIO: Rinka SoundBridge ASIO' in Vonovox with Dynamic GPU Delay Sensors (Modes 1-5, Default Mode 4) + Extra Buffer.",
                                    );
                                });

                            if cur_drv != self.pipeline_a.driver_backend {
                                self.pipeline_a.driver_backend = cur_drv;
                                self.pipeline_b.driver_backend = cur_drv;
                                let _ = self.sync_unified_asio_config(false);
                                self.pipeline_a.restart_stream();
                                self.pipeline_b.restart_stream();
                                self.trigger_auto_save();
                            }
                        });

                    ui.add_space(6.0);

                    // Kotak bertepi putus-putus (dashed box) persis seperti di v1.22
                    let tray_box = egui::Frame::none()
                        .fill(Palette::BG_PANEL)
                        .rounding(Rounding::same(8.0))
                        .inner_margin(Margin::symmetric(6.0, 5.0))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let avail_for_tray = (ui.available_width() - 326.0).max(160.0);

                                // 1. Tombol SYSTEM TRAY (#60CDFF biru cerah seperti v1.22)
                                let btn_tray = egui::Button::new(
                                    RichText::new("📥 SYSTEM TRAY (RUN IN BACKGROUND)")
                                        .font(FontId::new(12.0, FontFamily::Proportional))
                                        .strong()
                                        .color(Color32::WHITE),
                                )
                                .fill(Palette::ACCENT_BLUE)
                                .rounding(Rounding::same(8.0));
                                if ui.add_sized([avail_for_tray, 28.0], btn_tray).clicked() {
                                    for p in [&self.pipeline_a, &self.pipeline_b] {
                                        for slot in &p.vst_slots {
                                            slot.hide_editor_window();
                                        }
                                    }
                                    self.save_config_now();
                                    self.tray_manager.minimize_to_tray(APP_WINDOW_TITLE);
                                }

                                ui.add_space(4.0);

                                // 2. Tombol REFRESH AUDIO DEVICE (#6CCB5F hijau cerah seperti v1.22)
                                let btn_ref = egui::Button::new(
                                    RichText::new("🎧 REFRESH AUDIO DEVICE")
                                        .font(FontId::new(12.0, FontFamily::Proportional))
                                        .strong()
                                        .color(Color32::WHITE),
                                )
                                .fill(Palette::ACCENT_GREEN)
                                .rounding(Rounding::same(8.0));
                                if ui.add_sized([192.0, 28.0], btn_ref).clicked() {
                                    self.refresh_audio_devices();
                                }

                                ui.add_space(4.0);

                                // 3. Tombol RESTART APP (#FCE100 kuning cerah seperti v1.22)
                                let btn_rel = egui::Button::new(
                                    RichText::new("⚡ RESTART APP")
                                        .font(FontId::new(12.0, FontFamily::Proportional))
                                        .strong()
                                        .color(Color32::WHITE),
                                )
                                .fill(Palette::ACCENT_YELLOW)
                                .rounding(Rounding::same(8.0));
                                if ui.add_sized([118.0, 28.0], btn_rel).clicked() {
                                    self.reload_application(ctx);
                                }
                            });
                        });

                    paint_dashed_rect(
                        &ui.painter(),
                        tray_box.response.rect,
                        Palette::BORDER_STRONG,
                        5.0,
                        4.0,
                    );
                });
            });
    }

    fn render_line_panel(&mut self, ui: &mut egui::Ui, target: ActiveLineTarget) {
        let is_line_a = target == ActiveLineTarget::LineA;
        let title = if is_line_a { "Line A" } else { "Line B" };

        let mut open_vst_picker = false;
        let mut open_asio_setting_modal = false;
        let mut open_dsp_rsb_setting_modal = false;
        let mut open_rsb_asio_setting_modal = false;
        let mut do_refresh_devices = false;
        let mut auto_save = false;

        let wasapi_inputs = self.wasapi_inputs.clone();
        let wasapi_outputs = self.wasapi_outputs.clone();
        let asio_drivers = self.asio_drivers.clone();
        let vc_prof_snap = self.asio_vc_profile.clone();

        let (pipeline, vol, ov_in_labels, ov_out_labels) = if is_line_a {
            (
                &mut self.pipeline_a,
                &mut self.vol_a,
                &mut self.ov_in_labels_a,
                &mut self.ov_out_labels_a,
            )
        } else {
            (
                &mut self.pipeline_b,
                &mut self.vol_b,
                &mut self.ov_in_labels_b,
                &mut self.ov_out_labels_b,
            )
        };

        egui::Frame::none()
            .fill(Palette::BG_PANEL)
            .stroke(Stroke::new(1.0_f32, Palette::BORDER))
            .rounding(Rounding::same(12.0))
            .inner_margin(Margin::same(12.0))
            .show(ui, |ui| {
                // 1. Header Bar Line (`─── Line A ───` digambar vektor bebas kotak-kotak + Tombol LINE ON)
                ui.horizontal(|ui| {
                    let (line_l, _) = ui.allocate_exact_size(Vec2::new(24.0, 18.0), Sense::hover());
                    ui.painter().hline(
                        line_l.x_range(),
                        line_l.center().y,
                        Stroke::new(1.8_f32, Palette::ACCENT_PINK),
                    );
                    ui.label(
                        RichText::new(title)
                            .font(FontId::new(16.0, FontFamily::Proportional))
                            .strong()
                            .color(Palette::ACCENT_PINK),
                    );
                    let (line_r, _) = ui.allocate_exact_size(Vec2::new(24.0, 18.0), Sense::hover());
                    ui.painter().hline(
                        line_r.x_range(),
                        line_r.center().y,
                        Stroke::new(1.8_f32, Palette::ACCENT_PINK),
                    );

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let enabled = pipeline.is_enabled();
                        let (fill, border_col, txt) = if enabled {
                            (Palette::ACCENT_GREEN, Palette::ACCENT_GREEN_DEEP, "LINE ON")
                        } else {
                            (Palette::ACCENT_CORAL, Palette::ACCENT_CORAL_DEEP, "LINE OFF")
                        };

                        let (btn_rect, btn_resp) =
                            ui.allocate_exact_size(Vec2::new(96.0, 26.0), Sense::click());
                        if ui.is_rect_visible(btn_rect) {
                            let painter = ui.painter();
                            let bg = if btn_resp.hovered() {
                                if enabled {
                                    Palette::ACCENT_GREEN_HOVER
                                } else {
                                    Palette::ACCENT_CORAL_HOVER
                                }
                            } else {
                                fill
                            };
                            painter.rect(
                                btn_rect,
                                Rounding::same(6.0),
                                bg,
                                Stroke::new(1.0_f32, border_col),
                            );
                            let dot_c = Pos2::new(btn_rect.min.x + 17.0, btn_rect.center().y);
                            if enabled {
                                painter.circle_filled(
                                    dot_c,
                                    5.5,
                                    Color32::from_rgb(0x48, 0xB8, 0x88),
                                );
                                painter.circle_filled(
                                    Pos2::new(dot_c.x - 1.2, dot_c.y - 1.2),
                                    2.2,
                                    Color32::from_rgb(0xB8, 0xFF, 0xD8),
                                );
                            } else {
                                painter.circle_filled(dot_c, 5.5, Color32::WHITE);
                            }
                            painter.text(
                                Pos2::new(btn_rect.min.x + 28.0, btn_rect.center().y),
                                egui::Align2::LEFT_CENTER,
                                txt,
                                FontId::new(12.0, FontFamily::Proportional),
                                Color32::WHITE,
                            );
                        }
                        if btn_resp.clicked() {
                            pipeline.set_enabled(!enabled);
                            auto_save = true;
                        }
                    });
                });

                ui.add_space(2.0);

                // 2. Baris Status (Titik bulat putih/hijau + teks status bersih seperti v1.22)
                ui.horizontal(|ui| {
                    let (dot_rect, _) =
                        ui.allocate_exact_size(Vec2::new(14.0, 16.0), Sense::hover());
                    let is_running = pipeline.is_enabled() && pipeline.streams_running();
                    let dot_color = if !pipeline.is_enabled() {
                        Color32::from_rgb(0x90, 0x90, 0x90)
                    } else if is_running {
                        Palette::ACCENT_GREEN
                    } else {
                        Color32::from_rgb(0xE0, 0xE0, 0xE8)
                    };
                    ui.painter()
                        .circle_filled(dot_rect.center(), 5.0, dot_color);

                    let clean_status = pipeline
                        .status_message
                        .trim_start_matches(|c: char| !c.is_ascii_alphanumeric() && c != '(')
                        .replace("->", "->");
                    let xrun_tip = format!(
                        "Xrun (underflow/overflow): In {} | Out {}",
                        pipeline.in_xruns(),
                        pipeline.out_xruns()
                    );
                    ui.label(
                        RichText::new(clean_status)
                            .size(12.0)
                            .color(Palette::TEXT_SECONDARY),
                    )
                    .on_hover_text(xrun_tip);

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let (dsp_lvl, dsp_txt, dsp_col, dsp_tip) = if !is_running {
                            (
                                0.0f32,
                                "--%".to_string(),
                                Palette::TEXT_MUTED,
                                "Line inactive".to_string(),
                            )
                        } else if pipeline.shared.is_in_failsafe() {
                            (
                                0.0f32,
                                "MUTED".to_string(),
                                Palette::ACCENT_CORAL,
                                "Safe Mode (Fail-Closed) active: output muted due to a VST plugin error".to_string(),
                            )
                        } else {
                            let raw_load = pipeline.dsp_load() * 100.0;
                            let peak = pipeline.dsp_peak() * 100.0;
                            let active_vst_count = pipeline
                                .vst_slots
                                .iter()
                                .filter(|s| !s.is_bypassed())
                                .count();

                            let disp_load = if active_vst_count > 0 && raw_load < 0.1 {
                                0.1
                            } else {
                                raw_load
                            };
                            let txt = if disp_load > 0.0 && disp_load < 10.0 {
                                format!("{:.1}%", disp_load)
                            } else {
                                format!("{:.0}%", disp_load)
                            };

                            let col = if disp_load < 50.0 {
                                Palette::ACCENT_GREEN
                            } else if disp_load < 80.0 {
                                Palette::ACCENT_YELLOW
                            } else {
                                Palette::ACCENT_CORAL
                            };

                            let mut vst_lines = String::new();
                            for (i, s) in pipeline.vst_slots.iter().enumerate() {
                                let us = s.last_process_ns.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1000.0;
                                let state_str = if s.is_bypassed() { "BYPASS" } else { "ON" };
                                vst_lines.push_str(&format!(
                                    "\n  [{}] {} ({}): {:.1} us / block",
                                    i + 1,
                                    s.name,
                                    state_str,
                                    us
                                ));
                            }

                            let f = pipeline.fifo_stats();
                            let tip = format!(
                                "Total DSP Load: avg {:.2}% | peak {:.2}%{}\nWASAPI/ASIO Xruns: In {} | Out {}\nFIFO: underrun {} | overrun {} | stretch {} | drift compensation {:+.0} ppm",
                                disp_load,
                                peak,
                                vst_lines,
                                pipeline.in_xruns(),
                                pipeline.out_xruns(),
                                f.underruns,
                                f.overruns,
                                f.stretches,
                                (f.ratio - 1.0) * 1e6
                            );
                            let bar_lvl = if disp_load > 0.0 {
                                (disp_load / 100.0).max(0.14).clamp(0.0, 1.0)
                            } else {
                                0.0
                            };
                            (bar_lvl, txt, col, tip)
                        };

                        ui.label(
                            RichText::new(dsp_txt)
                                .size(11.0)
                                .strong()
                                .color(dsp_col),
                        )
                        .on_hover_text(&dsp_tip);
                        draw_diagonal_meter(
                            ui,
                            Vec2::new(62.0, 15.0),
                            dsp_lvl,
                            4.5,
                            2.0,
                            3.5,
                            3.0,
                            2.0,
                        )
                        .on_hover_text(dsp_tip);
                    });
                });

                // 3. GROUPBOX: Audio Device
                draw_groupbox(ui, GroupIcon::Mic, "Audio Device", |ui| {
                    let full_w = ui.available_width();

                    if pipeline.driver_backend == DriverBackend::AsioNative {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("ASIO Driver (Hardware / Virtual):")
                                    .size(12.0)
                                    .strong()
                                    .color(Palette::TEXT_SECONDARY),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let btn_scan = egui::Button::new(
                                    RichText::new("Scan Devices")
                                        .size(11.0)
                                        .color(Palette::TEXT_PRIMARY),
                                )
                                .fill(Palette::BG_INPUT)
                                .stroke(Stroke::new(1.0_f32, Palette::BORDER))
                                .rounding(Rounding::same(4.0));
                                if ui.add_sized([105.0, 22.0], btn_scan).clicked() {
                                    do_refresh_devices = true;
                                }
                            });
                        });

                        let asio_items: Vec<(String, String)> = asio_drivers
                            .iter()
                            .map(|d| (d.name.clone(), format!("{} [ASIO]", d.name)))
                            .collect();
                        let mut sel_asio = pipeline.asio_driver_name.clone().unwrap_or_default();
                        if draw_v122_device_dropdown(
                            ui,
                            &format!("dd_asio_{}", title),
                            &mut sel_asio,
                            "(None)",
                            &asio_items,
                            full_w,
                        ) {
                            pipeline.asio_driver_name = if sel_asio.is_empty() {
                                None
                            } else {
                                Some(sel_asio)
                            };
                            pipeline.restart_stream();
                            auto_save = true;
                        }

                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(" ASIO Channel & Buffer Settings:")
                                .size(12.0)
                                .strong()
                                .color(Palette::TEXT_SECONDARY),
                        );
                        let btn_panel = egui::Button::new(
                            RichText::new("Open ASIO Control Panel")
                                .size(12.0)
                                .strong()
                                .color(Palette::TEXT_PRIMARY),
                        )
                        .fill(Palette::BG_INPUT)
                        .stroke(Stroke::new(1.0_f32, Palette::BORDER))
                        .rounding(Rounding::same(6.0));
                        if ui.add_sized([full_w, 28.0], btn_panel).clicked() {
                            if let Some(ref drv) = pipeline.asio_driver_name {
                                if let Err(e) = open_asio_control_panel(drv) {
                                    show_info_message_box("ASIO Control Panel", &e);
                                }
                            } else {
                                show_info_message_box(
                                    "ASIO Control Panel",
                                    "Please select an ASIO driver first.",
                                );
                            }
                        }
                    } else if pipeline.driver_backend == DriverBackend::AsioExperimental {
                        // MODE ASIO EXPERIMENTAL (TRIPLE ASIO):
                        // Tidak menampilkan dropdown Input/Output di panel utama, melainkan ringkasan
                        // status ASIO Line ini + tombol untuk membuka Jendela "ASIO Setting (Triple ASIO)"!
                        let prof = &pipeline.asio_profile;
                        let in_disp = if prof.input_device.is_empty() {
                            "(Not selected in ASIO Settings)"
                        } else {
                            &prof.input_device
                        };
                        let out_disp = if prof.output_device.is_empty() {
                            "(Not selected in ASIO Settings)"
                        } else {
                            &prof.output_device
                        };
                        let vc_in_disp = if vc_prof_snap.input_device.is_empty() {
                            "(Not selected)"
                        } else {
                            &vc_prof_snap.input_device
                        };
                        let vc_out_disp = if vc_prof_snap.output_device.is_empty() {
                            "(Not selected)"
                        } else {
                            &vc_prof_snap.output_device
                        };

                        egui::Frame::none()
                            .fill(Palette::BG_INPUT)
                            .stroke(Stroke::new(1.0_f32, Palette::BORDER))
                            .rounding(Rounding::same(6.0))
                            .inner_margin(Margin::symmetric(8.0, 6.0))
                            .show(ui, |ui| {
                                ui.set_min_width((full_w - 16.0).max(80.0));
                                ui.label(
                                    RichText::new(format!(
                                        "🚀 ASIO {} ({} smp | In:{:.1}, Out:{:.1} | AutoConvert:{})",
                                        title,
                                        prof.buffer_size,
                                        prof.in_latency_val,
                                        prof.out_latency_val,
                                        if prof.wasapi_in_autoconvert && prof.wasapi_out_autoconvert {
                                            "ON"
                                        } else {
                                            "Custom"
                                        }
                                    ))
                                    .size(11.5)
                                    .strong()
                                    .color(Palette::ACCENT_PINK),
                                );
                                ui.label(
                                    RichText::new(format!("• In  : {}", in_disp))
                                        .size(11.0)
                                        .color(Palette::TEXT_SECONDARY),
                                );
                                ui.label(
                                    RichText::new(format!("• Out : {}", out_disp))
                                        .size(11.0)
                                        .color(Palette::TEXT_SECONDARY),
                                );
                                ui.add_space(2.0);
                                ui.label(
                                    RichText::new(format!(
                                        "🎙 Dedicated VC ASIO ({}) : {} -> {}",
                                        RINKA_ASIO_DRIVER_NAME, vc_in_disp, vc_out_disp
                                    ))
                                    .size(10.5)
                                    .color(Palette::TEXT_MUTED),
                                );
                            });

                        ui.add_space(6.0);
                        let btn_open_asio = egui::Button::new(
                            RichText::new("⚙️ Open ASIO Settings (Triple ASIO)")
                                .size(12.5)
                                .strong()
                                .color(Color32::WHITE),
                        )
                        .fill(Palette::ACCENT_BLUE_DEEP)
                        .stroke(Stroke::new(1.0_f32, Palette::ACCENT_PINK))
                        .rounding(Rounding::same(6.0));
                        if ui
                            .add_sized([full_w, 32.0], btn_open_asio)
                            .on_hover_text(
                                "Open the Triple ASIO Settings window to configure Input, Output, Buffer Size (smp), Input/Output Latency, and WASAPI Set/Exclusive/AutoConvert for:\n1. ASIO Line A\n2. ASIO Line B\n3. Dedicated VC ASIO ('Rinka SoundBridge ASIO')",
                            )
                            .clicked()
                        {
                            open_asio_setting_modal = true;
                        }
                    } else if pipeline.driver_backend == DriverBackend::RsbAsio {
                        // MODE RSB ASIO (BUILT-IN FLEXASIO — INTERNAL LOOPBACK TANPA VIRTUAL CABLE):
                        // WASAPI In A -> VST A -> ASIO Out A ("RSB ASIO") -> Loopback In B -> WASAPI Loopback B -> VST B -> WASAPI Out B
                        if is_line_a {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new("Mic Input A (WASAPI Ultra-Optimized):")
                                        .size(12.0)
                                        .strong()
                                        .color(Palette::TEXT_SECONDARY),
                                );
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    let btn_scan = egui::Button::new(
                                        RichText::new("🔄 Scan Devices")
                                            .size(11.0)
                                            .color(Palette::TEXT_PRIMARY),
                                    )
                                    .fill(Palette::BG_INPUT)
                                    .stroke(Stroke::new(1.0_f32, Palette::BORDER))
                                    .rounding(Rounding::same(4.0));
                                    if ui.add_sized([112.0, 22.0], btn_scan).clicked() {
                                        do_refresh_devices = true;
                                    }
                                });
                            });

                            let in_items: Vec<(String, String)> = wasapi_inputs
                                .iter()
                                .map(|d| (d.name.clone(), format!("{} [{} In]", d.name, d.max_channels)))
                                .collect();
                            let mut sel_in = pipeline.input_device_name.clone().unwrap_or_default();
                            if draw_v122_device_dropdown(
                                ui,
                                &format!("dd_rsb_asio_in_{}", title),
                                &mut sel_in,
                                "(Select Mic Input A, e.g. Microphone / SSL Input 1)",
                                &in_items,
                                full_w,
                            ) {
                                pipeline.input_device_name = if sel_in.is_empty() {
                                    None
                                } else {
                                    Some(sel_in.clone())
                                };
                                pipeline.asio_profile.input_device = sel_in.clone();
                                pipeline.rsb_asio_config.input_device = sel_in;
                                pipeline.restart_stream();
                                auto_save = true;
                            }

                            ui.add_space(5.0);
                            ui.label(
                                RichText::new("Line A Output (Internal Loopback — No Virtual Cable Needed):")
                                    .size(11.5)
                                    .strong()
                                    .color(Palette::TEXT_MUTED),
                            );
                            egui::Frame::none()
                                .fill(Color32::from_rgb(0x16, 0x22, 0x2C))
                                .stroke(Stroke::new(1.0_f32, Palette::ACCENT_BLUE_DEEP))
                                .rounding(Rounding::same(6.0))
                                .inner_margin(Margin::symmetric(8.0, 5.0))
                                .show(ui, |ui| {
                                    ui.set_min_width((full_w - 16.0).max(80.0));
                                    ui.label(
                                        RichText::new("🔒 Internal Loopback: VST Out A ➔ ASIO Out A (To Vonovox Input)")
                                            .size(11.5)
                                            .strong()
                                            .color(Palette::ACCENT_PINK),
                                    );
                                    ui.label(
                                        RichText::new(format!(
                                            "WASAPI In A -> VST A -> Select ASIO in Vonovox: \"{}\"",
                                            RSB_ASIO_DRIVER_NAME
                                        ))
                                        .size(11.0)
                                        .color(Palette::TEXT_SECONDARY),
                                    );
                                });
                        } else {
                            ui.label(
                                RichText::new("Line B Input (Internal Loopback — No Virtual Cable Needed):")
                                    .size(11.5)
                                    .strong()
                                    .color(Palette::TEXT_MUTED),
                            );
                            egui::Frame::none()
                                .fill(Color32::from_rgb(0x16, 0x22, 0x2C))
                                .stroke(Stroke::new(1.0_f32, Palette::ACCENT_BLUE_DEEP))
                                .rounding(Rounding::same(6.0))
                                .inner_margin(Margin::symmetric(8.0, 5.0))
                                .show(ui, |ui| {
                                    ui.set_min_width((full_w - 16.0).max(80.0));
                                    ui.label(
                                        RichText::new("🔒 Internal Loopback: ASIO Loopback In B ➔ WASAPI Loopback B")
                                            .size(11.5)
                                            .strong()
                                            .color(Palette::ACCENT_PINK),
                                    );
                                    ui.label(
                                        RichText::new(format!(
                                            "From Vonovox (\"{}\") -> WASAPI Loopback B -> VST B",
                                            RSB_ASIO_DRIVER_NAME
                                        ))
                                        .size(11.0)
                                        .color(Palette::TEXT_SECONDARY),
                                    );
                                });

                            ui.add_space(5.0);
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new("Speaker Output B (WASAPI Ultra-Optimized):")
                                        .size(12.0)
                                        .strong()
                                        .color(Palette::TEXT_SECONDARY),
                                );
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    let btn_scan = egui::Button::new(
                                        RichText::new("🔄 Scan Devices")
                                            .size(11.0)
                                            .color(Palette::TEXT_PRIMARY),
                                    )
                                    .fill(Palette::BG_INPUT)
                                    .stroke(Stroke::new(1.0_f32, Palette::BORDER))
                                    .rounding(Rounding::same(4.0));
                                    if ui.add_sized([112.0, 22.0], btn_scan).clicked() {
                                        do_refresh_devices = true;
                                    }
                                });
                            });

                            let out_items: Vec<(String, String)> = wasapi_outputs
                                .iter()
                                .map(|d| (d.name.clone(), format!("{} [{} Out]", d.name, d.max_channels)))
                                .collect();
                            let mut sel_out = pipeline.output_device_name.clone().unwrap_or_default();
                            if draw_v122_device_dropdown(
                                ui,
                                &format!("dd_rsb_asio_out_{}", title),
                                &mut sel_out,
                                "(Select Speaker Output B, e.g. Headphones / Speakers)",
                                &out_items,
                                full_w,
                            ) {
                                pipeline.output_device_name = if sel_out.is_empty() {
                                    None
                                } else {
                                    Some(sel_out.clone())
                                };
                                pipeline.asio_profile.output_device = sel_out.clone();
                                pipeline.rsb_asio_config.output_device = sel_out;
                                pipeline.restart_stream();
                                auto_save = true;
                            }
                        }

                        ui.add_space(6.0);
                        let rcfg = &pipeline.rsb_asio_config;
                        let eff_smp = rcfg.effective_buffer_size(pipeline.buffer_size);
                        let smp_badge = if rcfg.sync_buffer_size_with_rsb {
                            format!("{} smp [Auto-Sync]", eff_smp)
                        } else if rcfg.set_buffer_size_enabled {
                            format!("{} smp", eff_smp)
                        } else {
                            "Auto smp".to_string()
                        };
                        let lat_in_str = if rcfg.set_in_latency_enabled {
                            format!("In:{:.1}s", rcfg.in_latency_val)
                        } else {
                            "In:Auto".to_string()
                        };
                        let lat_out_str = if rcfg.set_out_latency_enabled {
                            format!("Out:{:.1}s", rcfg.out_latency_val)
                        } else {
                            "Out:Auto".to_string()
                        };
                        let btn_open_rsb_asio = egui::Button::new(
                            RichText::new(format!(
                                "⚙️ RSB ASIO Settings — Built-in FlexASIO GUI ({} | {} / {})",
                                smp_badge, lat_in_str, lat_out_str
                            ))
                            .size(11.5)
                            .strong()
                            .color(Color32::WHITE),
                        )
                        .fill(Palette::ACCENT_BLUE_DEEP)
                        .stroke(Stroke::new(1.0_f32, Palette::ACCENT_PINK))
                        .rounding(Rounding::same(6.0));
                        if ui
                            .add_sized([full_w, 30.0], btn_open_rsb_asio)
                            .on_hover_text(
                                "Open RSB ASIO (Built-in FlexASIO GUI):
                                 • Driver Name in Vonovox: 'ASIO: RSB ASIO' (Never conflicts with external FlexASIO)
                                 • Pure FlexASIO GUI v0.35 layout + Auto-Sync Buffer Size with RSB
                                 • Input/Output Device, Set Input/Output Latency (0.0s), WASAPI Exclusive/AutoConvert, Channels & TOML Output Preview",
                            )
                            .clicked()
                        {
                            open_rsb_asio_setting_modal = true;
                        }
                    } else if pipeline.driver_backend == DriverBackend::AsioDspRsb {
                        // MODE SoundBridge ASIO (PURE INTERNAL DSP):
                        // - Line A: Hanya pilih Input (Mic / SSL Input 1) -> VST -> otomatis ke DSP A Out ("DSP RSB Exp ASIO").
                        // - Line B: Otomatis dari DSP B In ("DSP RSB Exp ASIO") -> VST -> hanya pilih Output (Speaker / Otogai).
                        if is_line_a {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new("Line A Input (ASIO — Mic / Interface In):")
                                        .size(12.0)
                                        .strong()
                                        .color(Palette::TEXT_SECONDARY),
                                );
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    let btn_scan = egui::Button::new(
                                        RichText::new("🔄 Scan Devices")
                                            .size(11.0)
                                            .color(Palette::TEXT_PRIMARY),
                                    )
                                    .fill(Palette::BG_INPUT)
                                    .stroke(Stroke::new(1.0_f32, Palette::BORDER))
                                    .rounding(Rounding::same(4.0));
                                    if ui.add_sized([112.0, 22.0], btn_scan).clicked() {
                                        do_refresh_devices = true;
                                    }
                                });
                            });

                            let in_items: Vec<(String, String)> = wasapi_inputs
                                .iter()
                                .map(|d| (d.name.clone(), format!("{} [{} In]", d.name, d.max_channels)))
                                .collect();
                            let mut sel_in = pipeline.input_device_name.clone().unwrap_or_default();
                            if draw_v122_device_dropdown(
                                ui,
                                &format!("dd_dsp_rsb_in_{}", title),
                                &mut sel_in,
                                "(Select Line A ASIO Input, e.g. SSL Input 1)",
                                &in_items,
                                full_w,
                            ) {
                                pipeline.input_device_name = if sel_in.is_empty() {
                                    None
                                } else {
                                    Some(sel_in.clone())
                                };
                                pipeline.asio_profile.input_device = sel_in;
                                pipeline.restart_stream();
                                auto_save = true;
                            }

                            ui.add_space(5.0);
                            ui.label(
                                RichText::new("Line A Output (Automatic — Locked to ASIO Bridge):")
                                    .size(11.5)
                                    .strong()
                                    .color(Palette::TEXT_MUTED),
                            );
                            egui::Frame::none()
                                .fill(Color32::from_rgb(0x16, 0x26, 0x1E))
                                .stroke(Stroke::new(1.0_f32, Palette::ACCENT_GREEN_DEEP))
                                .rounding(Rounding::same(6.0))
                                .inner_margin(Margin::symmetric(8.0, 5.0))
                                .show(ui, |ui| {
                                    ui.set_min_width((full_w - 16.0).max(80.0));
                                    ui.label(
                                        RichText::new("🔒 Automatic: Line A ASIO Out -> ASIO Bridge (To Vonovox Input)")
                                            .size(11.5)
                                            .strong()
                                            .color(Palette::ACCENT_GREEN),
                                    );
                                    ui.label(
                                        RichText::new(format!(
                                            "A In [ASIO] -> Line A VST -> ASIO: \"{}\"",
                                            DSP_RSB_ASIO_DRIVER_NAME
                                        ))
                                        .size(11.0)
                                        .color(Palette::TEXT_SECONDARY),
                                    );
                                });
                        } else {
                            ui.label(
                                RichText::new("Line B Input (Automatic — Locked to ASIO Bridge):")
                                    .size(11.5)
                                    .strong()
                                    .color(Palette::TEXT_MUTED),
                            );
                            egui::Frame::none()
                                .fill(Color32::from_rgb(0x16, 0x26, 0x1E))
                                .stroke(Stroke::new(1.0_f32, Palette::ACCENT_GREEN_DEEP))
                                .rounding(Rounding::same(6.0))
                                .inner_margin(Margin::symmetric(8.0, 5.0))
                                .show(ui, |ui| {
                                    ui.set_min_width((full_w - 16.0).max(80.0));
                                    ui.label(
                                        RichText::new("🔒 Automatic: ASIO Bridge (From Vonovox Output) -> Line B ASIO In")
                                            .size(11.5)
                                            .strong()
                                            .color(Palette::ACCENT_GREEN),
                                    );
                                    ui.label(
                                        RichText::new(format!(
                                            "ASIO: \"{}\" -> Line B VST -> B Out [ASIO]",
                                            DSP_RSB_ASIO_DRIVER_NAME
                                        ))
                                        .size(11.0)
                                        .color(Palette::TEXT_SECONDARY),
                                    );
                                });

                            ui.add_space(5.0);
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new("Line B Output (ASIO — Speaker / Headphones):")
                                        .size(12.0)
                                        .strong()
                                        .color(Palette::TEXT_SECONDARY),
                                );
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    let btn_scan = egui::Button::new(
                                        RichText::new("🔄 Scan Devices")
                                            .size(11.0)
                                            .color(Palette::TEXT_PRIMARY),
                                    )
                                    .fill(Palette::BG_INPUT)
                                    .stroke(Stroke::new(1.0_f32, Palette::BORDER))
                                    .rounding(Rounding::same(4.0));
                                    if ui.add_sized([112.0, 22.0], btn_scan).clicked() {
                                        do_refresh_devices = true;
                                    }
                                });
                            });

                            let out_items: Vec<(String, String)> = wasapi_outputs
                                .iter()
                                .map(|d| (d.name.clone(), format!("{} [{} Out]", d.name, d.max_channels)))
                                .collect();
                            let mut sel_out = pipeline.output_device_name.clone().unwrap_or_default();
                            if draw_v122_device_dropdown(
                                ui,
                                &format!("dd_dsp_rsb_out_{}", title),
                                &mut sel_out,
                                "(Select Line B ASIO Output, e.g. Speaker / Headphones)",
                                &out_items,
                                full_w,
                            ) {
                                pipeline.output_device_name = if sel_out.is_empty() {
                                    None
                                } else {
                                    Some(sel_out.clone())
                                };
                                pipeline.asio_profile.output_device = sel_out;
                                pipeline.restart_stream();
                                auto_save = true;
                            }
                        }

                        ui.add_space(6.0);
                        let dsp_cfg = &pipeline.dsp_rsb_config;
                        let eff_smp = dsp_cfg.effective_buffer_size(pipeline.buffer_size);
                        let smp_badge = if dsp_cfg.sync_buffer_size_with_rsb {
                            format!("{} smp [Auto RSB]", eff_smp)
                        } else {
                            format!("{} smp [Manual]", eff_smp)
                        };
                        let cpu_badge = if dsp_cfg.asio_cpu_cores == 0 {
                            "CPU: Auto".to_string()
                        } else {
                            format!("CPU: {}C", dsp_cfg.asio_cpu_cores)
                        };
                        let live_vono_ms = get_dsp_rsb_bus_b_in().get_measured_gpu_stall_ms();
                        let extra_ms = dsp_cfg.adaptive_extra_ms.clamp(0.0, 250.0);
                        let eff_vono_ms = if live_vono_ms >= 1.0 { live_vono_ms + extra_ms } else { 0.0 };
                        let (act_cush_smp, _) = get_dsp_rsb_bus_b_in().get_dsp_b_params();
                        let act_cush_ms = (act_cush_smp as f32 / (pipeline.effective_sample_rate().max(44100) as f32)) * 1000.0;
                        let mode_badge = if live_vono_ms >= 1.0 {
                            if extra_ms >= 0.5 {
                                format!("GPU: {:.0}+{:.0}={:.0}ms | Cushion: {:.0}ms", live_vono_ms, extra_ms, eff_vono_ms, act_cush_ms)
                            } else {
                                format!("Vonovox GPU: {:.0}ms | Cushion: {:.0}ms", live_vono_ms, act_cush_ms)
                            }
                        } else if dsp_cfg.adaptive_cushion {
                            if extra_ms >= 0.5 {
                                format!("Adaptive +{:.0}ms | Cushion: {:.0}ms", extra_ms, act_cush_ms)
                            } else {
                                format!("Adaptive Cushion {:.0}ms", act_cush_ms)
                            }
                        } else {
                            format!("Cushion {:.0}ms (+{:.0}ms)", dsp_cfg.dsp_b_cushion_ms, extra_ms)
                        };
                        let btn_open_dsp_setting = egui::Button::new(
                            RichText::new(format!(
                                "⚙️ SoundBridge ASIO Settings ({} | {} | {})",
                                smp_badge, mode_badge, cpu_badge
                            ))
                            .size(11.5)
                            .strong()
                            .color(Color32::WHITE),
                        )
                        .fill(Palette::ACCENT_GREEN_DEEP)
                        .stroke(Stroke::new(1.0_f32, Palette::ACCENT_GREEN))
                        .rounding(Rounding::same(6.0));
                        if ui
                            .add_sized([full_w, 30.0], btn_open_dsp_setting)
                            .on_hover_text(
                                "Open SoundBridge ASIO quality settings ('Rinka SoundBridge ASIO'):\n\
                                 - Buffer Size (smp): Auto-sync with Rinka SoundBridge OR separate Manual selection (64, 128, 256, 512, 1024, 2048 smp)\n\
                                 - Set Input/Output Latency, WASAPI Set, Exclusive Mode, AutoConvert & Channels\n\
                                 - GPU Anti-Stutter Cushion & Catmull-Rom Cubic Micro-Stretch Rescue",
                            )
                            .clicked()
                        {
                            open_dsp_rsb_setting_modal = true;
                        }
                    } else {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("Input (Mic / Line-In):")
                                    .size(12.0)
                                    .strong()
                                    .color(Palette::TEXT_SECONDARY),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let btn_scan = egui::Button::new(
                                    RichText::new("🔄 Scan Devices")
                                        .size(11.0)
                                        .color(Palette::TEXT_PRIMARY),
                                )
                                .fill(Palette::BG_INPUT)
                                .stroke(Stroke::new(1.0_f32, Palette::BORDER))
                                .rounding(Rounding::same(4.0));
                                if ui.add_sized([112.0, 22.0], btn_scan).clicked() {
                                    do_refresh_devices = true;
                                }
                            });
                        });

                        let mut in_items: Vec<(String, String)> = Vec::new();
                        for d in &wasapi_inputs {
                            in_items.push((d.name.clone(), format!("{} [{} In]", d.name, d.max_channels)));
                        }

                        let mut sel_in = pipeline.input_device_name.clone().unwrap_or_default();
                        let in_changed = draw_v122_device_dropdown(
                            ui,
                            &format!("dd_in_{}", title),
                            &mut sel_in,
                            "(None)",
                            &in_items,
                            full_w,
                        );

                        ui.add_space(4.0);
                        ui.label(
                            RichText::new("Output (Speaker / Cable Input):")
                                .size(12.0)
                                .strong()
                                .color(Palette::TEXT_SECONDARY),
                        );

                        let mut out_items: Vec<(String, String)> = Vec::new();
                        for d in &wasapi_outputs {
                            out_items.push((d.name.clone(), format!("{} [{} Out]", d.name, d.max_channels)));
                        }
                        let mut sel_out = pipeline.output_device_name.clone().unwrap_or_default();
                        let out_changed = draw_v122_device_dropdown(
                            ui,
                            &format!("dd_out_{}", title),
                            &mut sel_out,
                            "(None)",
                            &out_items,
                            full_w,
                        );

                        if in_changed || out_changed {
                            pipeline.input_device_name = if sel_in.is_empty() {
                                None
                            } else {
                                Some(sel_in.clone())
                            };
                            pipeline.output_device_name = if sel_out.is_empty() {
                                None
                            } else {
                                Some(sel_out.clone())
                            };
                            pipeline.asio_profile.input_device = sel_in;
                            pipeline.asio_profile.output_device = sel_out;
                            pipeline.restart_stream();
                            auto_save = true;
                        }
                    }
                });

                // 4. GROUPBOX: VST Chain (Kotak besar #232323 selalu tampil utuh + tombol penuh "+ Add VST3")
                draw_groupbox(ui, GroupIcon::Plug, "VST Chain", |ui| {
                    let full_w = ui.available_width();
                    // Hitung tinggi dinamis agar mengisi ruang vertikal persis seperti v1.22
                    let vst_box_h = (ui.available_height() - 274.0).max(140.0);

                    egui::Frame::none()
                        .fill(Palette::BG_SCROLL)
                        .stroke(Stroke::new(1.0_f32, Palette::BORDER))
                        .rounding(Rounding::same(8.0))
                        .inner_margin(Margin::same(6.0))
                        .show(ui, |ui| {
                            let inner_w = (full_w - 14.0).max(80.0);
                            let inner_h = (vst_box_h - 14.0).max(160.0);
                            ui.set_min_size(Vec2::new(inner_w, inner_h));
                            ui.set_max_width(inner_w);

                            egui::ScrollArea::vertical()
                                .id_source(format!("vst_scroll_{}", title))
                                .max_height(inner_h)
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    ui.set_min_width(inner_w);
                                    let mut move_act: Option<(usize, isize)> = None;
                                    let mut del_act: Option<usize> = None;
                                    let mut edit_slot: Option<
                                        Arc<rinka_soundbridge_core::vst3_host::VSTSlot>,
                                    > = None;

                                    for (idx, slot) in pipeline.vst_slots.iter().enumerate() {
                                        egui::Frame::none()
                                            .fill(Palette::BG_PANEL)
                                            .stroke(Stroke::new(1.0_f32, Palette::BORDER))
                                            .rounding(Rounding::same(8.0))
                                            .inner_margin(Margin::symmetric(6.0, 4.0))
                                            .show(ui, |ui| {
                                                ui.horizontal(|ui| {
                                                    let tint = parse_hex_color(&slot.tint_color);
                                                    let title_btn = egui::Button::new(
                                                        RichText::new(format!(
                                                            "[{}] {}",
                                                            idx + 1,
                                                            slot.name
                                                        ))
                                                        .strong()
                                                        .color(Palette::TEXT_PRIMARY),
                                                    )
                                                    .fill(tint)
                                                    .stroke(Stroke::new(1.0_f32, Palette::BORDER))
                                                    .rounding(Rounding::same(6.0));

                                                    let avail_title_w =
                                                        (ui.available_width() - 176.0).max(60.0);
                                                    if ui
                                                        .add_sized([avail_title_w, 28.0], title_btn)
                                                        .on_hover_text({
                                                            let lat_smp = slot.latency_samples.load(std::sync::atomic::Ordering::Relaxed);
                                                            let sr = pipeline.effective_sample_rate().max(44100) as f64;
                                                            let raw_lat_ms = (lat_smp as f64 / sr) * 1000.0;
                                                            let eff_lat_ms = raw_lat_ms.min(40.0);
                                                            let us = slot.last_process_ns.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1000.0;
                                                            let lat_note = if raw_lat_ms > 45.0 {
                                                                format!("~{:.1} ms real-time vocal (static DAW PDC: {:.0} ms / {} smp)", eff_lat_ms, raw_lat_ms, lat_smp)
                                                            } else {
                                                                format!("{:.1} ms ({} samples)", raw_lat_ms, lat_smp)
                                                            };
                                                            format!(
                                                                "Click to open GUI editor: {}
File: {}
DSP Load: {:.1} us/block | VST Lookahead Delay: {}",
                                                                slot.name, slot.file_path, us, lat_note
                                                            )
                                                        })
                                                        .clicked()
                                                    {
                                                        edit_slot = Some(Arc::clone(slot));
                                                    }

                                                    if let Some((cnt, msg)) =
                                                        slot.has_recent_error(4.0)
                                                    {
                                                        let (er_rect, er_resp) = ui
                                                            .allocate_exact_size(
                                                                Vec2::new(14.0, 14.0),
                                                                Sense::hover(),
                                                            );
                                                        ui.painter().circle_filled(
                                                            er_rect.center(),
                                                            5.5,
                                                            Palette::ACCENT_CORAL,
                                                        );
                                                        er_resp.on_hover_text(format!(
                                                            "VST processing failed ({}x)\nError: {}",
                                                            cnt, msg
                                                        ));
                                                    }

                                                    let byp = slot.is_bypassed();
                                                    let (b_txt, b_col) = if byp {
                                                        ("BYPASS", Palette::ACCENT_CORAL)
                                                    } else {
                                                        ("ON", Palette::ACCENT_GREEN)
                                                    };
                                                    if ui
                                                        .add_sized(
                                                            [72.0, 24.0],
                                                            egui::Button::new(
                                                                RichText::new(b_txt)
                                                                    .size(11.0)
                                                                    .strong()
                                                                    .color(Color32::WHITE),
                                                            )
                                                            .fill(b_col)
                                                            .rounding(Rounding::same(5.0)),
                                                        )
                                                        .clicked()
                                                    {
                                                        slot.set_bypassed(!byp);
                                                        auto_save = true;
                                                    }

                                                    if ui
                                                        .add_sized(
                                                            [24.0, 24.0],
                                                            egui::Button::new("▲")
                                                                .fill(Palette::BG_PANEL_ALT),
                                                        )
                                                        .clicked()
                                                    {
                                                        move_act = Some((idx, -1));
                                                    }
                                                    if ui
                                                        .add_sized(
                                                            [24.0, 24.0],
                                                            egui::Button::new("▼")
                                                                .fill(Palette::BG_PANEL_ALT),
                                                        )
                                                        .clicked()
                                                    {
                                                        move_act = Some((idx, 1));
                                                    }
                                                    if ui
                                                        .add_sized(
                                                            [24.0, 24.0],
                                                            egui::Button::new(
                                                                RichText::new("X").strong()
                                                                    .color(Color32::WHITE),
                                                            )
                                                            .fill(Palette::ACCENT_CORAL)
                                                            .rounding(Rounding::same(5.0)),
                                                        )
                                                        .clicked()
                                                    {
                                                        del_act = Some(idx);
                                                    }
                                                });
                                            });
                                        ui.add_space(4.0);
                                    }

                                    if let Some((idx, dir)) = move_act {
                                        pipeline.move_vst(idx, dir);
                                        auto_save = true;
                                    }
                                    if let Some(idx) = del_act {
                                        pipeline.remove_vst(idx);
                                        auto_save = true;
                                    }
                                    if let Some(slot) = edit_slot {
                                        if let Err(e) = slot.open_editor() {
                                            show_info_message_box("VST GUI Info", &e);
                                        }
                                    }
                                });
                        });

                    ui.add_space(6.0);

                    // Baris Bawah VST Chain: [ + Add VST3 ]  [ DC Filter: ON ]  [ Mode Aman: ON ]
                    ui.horizontal(|ui| {
                        let add_w = (full_w - 236.0).max(110.0);
                        let add_btn = egui::Button::new(
                            RichText::new("+ Add VST3")
                                .font(FontId::new(12.5, FontFamily::Proportional))
                                .strong()
                                .color(Color32::WHITE),
                        )
                        .fill(Palette::ACCENT_LAVENDER)
                        .rounding(Rounding::same(8.0));

                        if ui.add_sized([add_w, 30.0], add_btn).clicked() {
                            open_vst_picker = true;
                        }

                        // Tombol Pre-VST 20Hz DC Filter
                        let dc_on = pipeline.dc_blocker_enabled();
                        let (dc_txt, dc_fill, dc_border, dc_col) = if dc_on {
                            (
                                "DC Filter: ON",
                                Color32::from_rgb(0x16, 0x29, 0x36),
                                Palette::ACCENT_BLUE_DEEP,
                                Palette::ACCENT_BLUE,
                            )
                        } else {
                            (
                                "DC Filter: OFF",
                                Palette::BG_INPUT,
                                Palette::BORDER,
                                Palette::TEXT_MUTED,
                            )
                        };
                        if ui
                            .add_sized(
                                [104.0, 30.0],
                                egui::Button::new(
                                    RichText::new(dc_txt).size(11.0).strong().color(dc_col),
                                )
                                .fill(dc_fill)
                                .stroke(Stroke::new(1.0_f32, dc_border))
                                .rounding(Rounding::same(6.0)),
                            )
                            .on_hover_text(
                                "Pre-VST 20Hz DC Blocker (Zero-Latency):
Removes microphone capsule DC offset & sub-sonic rumble (<20Hz) before entering the VST3 chain for cleaner pitch detection and voice conversion.",
                            )
                            .clicked()
                        {
                            pipeline.set_dc_blocker_enabled(!dc_on);
                            auto_save = true;
                        }

                        // Tombol Mode Aman (Fail-Closed / Safe Mode)
                        let fc = pipeline.fail_closed();
                        let (fc_txt, fc_fill, fc_border, fc_col) = if fc {
                            (
                                "Safe Mode: ON",
                                Color32::from_rgb(0x17, 0x2B, 0x1E),
                                Palette::ACCENT_GREEN_DEEP,
                                Palette::ACCENT_GREEN,
                            )
                        } else {
                            (
                                "Safe Mode: OFF",
                                Palette::BG_INPUT,
                                Palette::BORDER,
                                Palette::TEXT_MUTED,
                            )
                        };
                        let (fc_rect, fc_resp) =
                            ui.allocate_exact_size(Vec2::new(118.0, 30.0), Sense::click());
                        if ui.is_rect_visible(fc_rect) {
                            let painter = ui.painter();
                            painter.rect(
                                fc_rect,
                                Rounding::same(6.0),
                                fc_fill,
                                Stroke::new(1.0_f32, fc_border),
                            );
                            paint_shield_icon(
                                &painter,
                                Pos2::new(fc_rect.min.x + 12.0, fc_rect.center().y),
                                fc,
                            );
                            painter.text(
                                Pos2::new(fc_rect.min.x + 21.0, fc_rect.center().y),
                                egui::Align2::LEFT_CENTER,
                                fc_txt,
                                FontId::new(11.0, FontFamily::Proportional),
                                fc_col,
                            );
                        }
                        if fc_resp
                            .on_hover_text(
                                "Safe Mode (Fail-Closed):
If any VST plugin errors or crashes during processing, this Line's audio output is automatically muted so your raw microphone voice never leaks to the live stream.",
                            )
                            .clicked()
                        {
                            pipeline.set_fail_closed(!fc);
                            auto_save = true;
                        }
                    });
                });

                // 5. GROUPBOX: Overlay Audio (Loop File) (Persis seperti v1.22)
                draw_groupbox(ui, GroupIcon::Folder, "Overlay Audio (Loop File)", |ui| {
                    let full_w = ui.available_width();
                    let eff_sr = pipeline.effective_sample_rate();

                    for (is_input, hdr_text, bank, labels) in [
                        (
                            true,
                            "Input",
                            &pipeline.shared.overlay_input_bank,
                            ov_in_labels,
                        ),
                        (
                            false,
                            "Output",
                            &pipeline.shared.overlay_output_bank,
                            ov_out_labels,
                        ),
                    ] {
                        ui.horizontal(|ui| {
                            let (ic_rect, _) =
                                ui.allocate_exact_size(Vec2::new(14.0, 14.0), Sense::hover());
                            paint_group_icon(
                                &ui.painter(),
                                ic_rect.center(),
                                if is_input {
                                    GroupIcon::Mic
                                } else {
                                    GroupIcon::Speaker
                                },
                            );
                            ui.label(
                                RichText::new(hdr_text)
                                    .size(11.5)
                                    .strong()
                                    .color(Palette::TEXT_PRIMARY),
                            );
                        });

                        if let Some(player) = bank.players.first() {
                            egui::Frame::none()
                                .fill(Palette::BG_SCROLL)
                                .stroke(Stroke::new(1.0_f32, Palette::BORDER))
                                .rounding(Rounding::same(6.0))
                                .inner_margin(Margin::same(3.0))
                                .show(ui, |ui| {
                                    egui::Frame::none()
                                        .fill(Palette::BG_PANEL)
                                        .stroke(Stroke::new(1.0_f32, Palette::BORDER))
                                        .rounding(Rounding::same(6.0))
                                        .inner_margin(Margin::symmetric(6.0, 3.0))
                                        .show(ui, |ui| {
                                            ui.set_min_width((full_w - 20.0).max(80.0));
                                            ui.horizontal(|ui| {
                                                let mut en = player.is_enabled();
                                                if ui.checkbox(&mut en, "").changed() {
                                                    player.set_enabled(en);
                                                    auto_save = true;
                                                }

                                                // Tombol Browse Folder dengan ikon folder kuning vektor
                                                let (f_rect, f_resp) = ui.allocate_exact_size(
                                                    Vec2::new(22.0, 20.0),
                                                    Sense::click(),
                                                );
                                                if ui.is_rect_visible(f_rect) {
                                                    ui.painter().rect(
                                                        f_rect,
                                                        Rounding::same(4.0),
                                                        if f_resp.hovered() {
                                                            Palette::ACCENT_PINK_SOFT
                                                        } else {
                                                            Palette::BG_INPUT
                                                        },
                                                        Stroke::new(1.0_f32, Palette::BORDER),
                                                    );
                                                    paint_group_icon(
                                                        &ui.painter(),
                                                        f_rect.center(),
                                                        GroupIcon::Folder,
                                                    );
                                                }
                                                if f_resp.clicked() {
                                                    if let Some(path) = rfd::FileDialog::new()
                                                        .add_filter(
                                                            "Audio Files",
                                                            &[
                                                                "mp3", "wav", "ogg", "flac", "aif",
                                                                "aiff",
                                                            ],
                                                        )
                                                        .pick_file()
                                                    {
                                                        let p_str =
                                                            path.to_string_lossy().to_string();
                                                        match player.load_file(&p_str, eff_sr) {
                                                            Ok(dur) => {
                                                                let fname = path
                                                                    .file_name()
                                                                    .and_then(|s| s.to_str())
                                                                    .unwrap_or("audio");
                                                                labels[0] = format!(
                                                                    "{} ({:.1}s)",
                                                                    fname, dur
                                                                );
                                                            }
                                                            Err(e) => {
                                                                labels[0] =
                                                                    "Failed to load file!".to_string();
                                                                show_info_message_box(
                                                                    "Overlay Audio",
                                                                    &e,
                                                                );
                                                            }
                                                        }
                                                        auto_save = true;
                                                    }
                                                }

                                                ui.label(
                                                    RichText::new(&labels[0])
                                                        .size(11.5)
                                                        .color(Palette::TEXT_MUTED),
                                                );

                                                // CompactStepper di ujung kanan persis seperti v1.22
                                                ui.with_layout(
                                                    Layout::right_to_left(Align::Center),
                                                    |ui| {
                                                        let mut pct =
                                                            (player.gain() * 100.0).round() as i32;
                                                        let btn_up = egui::Button::new(
                                                            RichText::new("▶").size(9.5),
                                                        )
                                                        .fill(Palette::BG_INPUT)
                                                        .stroke(Stroke::new(
                                                            1.0_f32,
                                                            Palette::BORDER,
                                                        ))
                                                        .rounding(Rounding::same(5.0));
                                                        if ui
                                                            .add_sized([20.0, 20.0], btn_up)
                                                            .clicked()
                                                        {
                                                            pct = (pct + 1).clamp(0, 200);
                                                            player.set_gain(pct as f32 / 100.0);
                                                            auto_save = true;
                                                        }

                                                        ui.add_sized(
                                                            [40.0, 20.0],
                                                            egui::Label::new(
                                                                RichText::new(format!("{}%", pct))
                                                                    .size(11.5)
                                                                    .strong()
                                                                    .color(
                                                                        Palette::ACCENT_BLUE_DEEP,
                                                                    ),
                                                            ),
                                                        );

                                                        let btn_dn = egui::Button::new(
                                                            RichText::new("◀").size(9.5),
                                                        )
                                                        .fill(Palette::BG_INPUT)
                                                        .stroke(Stroke::new(
                                                            1.0_f32,
                                                            Palette::BORDER,
                                                        ))
                                                        .rounding(Rounding::same(5.0));
                                                        if ui
                                                            .add_sized([20.0, 20.0], btn_dn)
                                                            .clicked()
                                                        {
                                                            pct = (pct - 1).clamp(0, 200);
                                                            player.set_gain(pct as f32 / 100.0);
                                                            auto_save = true;
                                                        }
                                                    },
                                                );
                                            });
                                        });
                                });
                        }
                        if is_input {
                            ui.add_space(2.0);
                        }
                    }
                });

                // 6. GROUPBOX: Master Volume (Slider Cyan penuh + Diagonal VU Meter seperti v1.22)
                draw_groupbox(ui, GroupIcon::Speaker, "Master Volume", |ui| {
                    let full_w = ui.available_width();
                    ui.horizontal(|ui| {
                        let muted = pipeline.is_muted();
                        let (m_txt, m_fill, m_border) = if muted {
                            ("🔇 MUTED", Palette::ACCENT_CORAL, Palette::ACCENT_CORAL_DEEP)
                        } else {
                            ("🔊 MUTE", Palette::BG_INPUT, Palette::BORDER)
                        };
                        let btn_mute = egui::Button::new(
                            RichText::new(m_txt)
                                .size(11.0)
                                .strong()
                                .color(Color32::WHITE),
                        )
                        .fill(m_fill)
                        .stroke(Stroke::new(1.0_f32, m_border))
                        .rounding(Rounding::same(6.0));
                        if ui.add_sized([70.0, 26.0], btn_mute).clicked() {
                            pipeline.set_muted(!muted);
                            auto_save = true;
                        }

                        let btn_dn = egui::Button::new(RichText::new("◀").size(10.0))
                            .fill(Palette::BG_INPUT)
                            .stroke(Stroke::new(1.0_f32, Palette::BORDER))
                            .rounding(Rounding::same(6.0));
                        if ui.add_sized([26.0, 26.0], btn_dn).clicked() {
                            *vol = (*vol - 1).clamp(0, 150);
                            pipeline.set_master_gain(*vol as f32 / 100.0);
                            auto_save = true;
                        }

                        // Slider berlapis Cyan yang merentang penuh di antara tombol < dan >
                        let slider_w = (ui.available_width() - 82.0).max(60.0);
                        if draw_v122_volume_slider(ui, vol, slider_w) {
                            pipeline.set_master_gain(*vol as f32 / 100.0);
                            auto_save = true;
                        }

                        let btn_up = egui::Button::new(RichText::new("▶").size(10.0))
                            .fill(Palette::BG_INPUT)
                            .stroke(Stroke::new(1.0_f32, Palette::BORDER))
                            .rounding(Rounding::same(6.0));
                        if ui.add_sized([26.0, 26.0], btn_up).clicked() {
                            *vol = (*vol + 1).clamp(0, 150);
                            pipeline.set_master_gain(*vol as f32 / 100.0);
                            auto_save = true;
                        }

                        ui.add_sized(
                            [42.0, 26.0],
                            egui::Label::new(
                                RichText::new(format!("{}%", *vol))
                                    .size(12.5)
                                    .strong()
                                    .color(Palette::ACCENT_PINK),
                            ),
                        );
                    });

                    ui.add_space(4.0);
                    draw_diagonal_meter(
                        ui,
                        Vec2::new(full_w, 18.0),
                        pipeline.peak_level(),
                        6.5,
                        2.5,
                        4.5,
                        6.0,
                        3.0,
                    );
                });
            });

        if open_asio_setting_modal {
            self.asio_setting_open = true;
        }
        if open_dsp_rsb_setting_modal {
            self.dsp_rsb_setting_open = true;
        }
        if open_rsb_asio_setting_modal {
            self.rsb_asio_setting_open = true;
        }
        if do_refresh_devices {
            self.refresh_audio_devices();
        }
        if open_vst_picker {
            self.vst_picker.open = true;
            self.vst_picker.target_line = target;
            self.vst_picker.selected_path = self.scanned_vst_cache.values().next().cloned();
        }
        if auto_save {
            let _ = self.sync_unified_asio_config(false);
            self.trigger_auto_save();
        }
    }

    fn render_vst_picker_modal(&mut self, ctx: &egui::Context) {
        if !self.vst_picker.open {
            return;
        }
        let mut close_modal = false;
        let mut confirm_path: Option<String> = None;

        egui::Window::new("Select VST3 Plugin - Rinka SoundBridge")
            .collapsible(false)
            .resizable(true)
            .default_size([520.0, 500.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(RichText::new("Search VST3 Plugin:").strong());
                ui.text_edit_singleline(&mut self.vst_picker.search_query);
                ui.add_space(6.0);

                let q = self.vst_picker.search_query.to_lowercase();
                egui::Frame::none()
                    .fill(Palette::BG_SCROLL)
                    .stroke(Stroke::new(1.0_f32, Palette::BORDER))
                    .rounding(Rounding::same(8.0))
                    .inner_margin(Margin::same(6.0))
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .max_height(330.0)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                for (name, path) in &self.scanned_vst_cache {
                                    if !q.is_empty() && !name.to_lowercase().contains(&q) {
                                        continue;
                                    }
                                    let selected = self.vst_picker.selected_path.as_deref()
                                        == Some(path.as_str());
                                    let resp = ui
                                        .selectable_label(selected, format!("  {}", name))
                                        .on_hover_text(path);
                                    if resp.clicked() {
                                        self.vst_picker.selected_path = Some(path.clone());
                                    }
                                    if resp.double_clicked() {
                                        confirm_path = Some(path.clone());
                                    }
                                }
                            });
                    });

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("Rescan")
                                    .strong()
                                    .color(Color32::WHITE),
                            )
                            .fill(Palette::ACCENT_BLUE_DEEP)
                            .rounding(Rounding::same(6.0)),
                        )
                        .clicked()
                    {
                        self.scanned_vst_cache = VSTScanner::scan_plugins();
                    }
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("Browse (.vst3)...")
                                    .strong()
                                    .color(Color32::WHITE),
                            )
                            .fill(Palette::ACCENT_GREEN_DEEP)
                            .rounding(Rounding::same(6.0)),
                        )
                        .clicked()
                    {
                        if let Some(p) = rfd::FileDialog::new()
                            .add_filter("VST3 Plugin", &["vst3"])
                            .pick_file()
                        {
                            confirm_path = Some(p.to_string_lossy().to_string());
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Cancel").clicked() {
                            close_modal = true;
                        }
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("Add Selected VST")
                                        .strong()
                                        .color(Color32::WHITE),
                                )
                                .fill(Palette::ACCENT_LAVENDER)
                                .rounding(Rounding::same(6.0)),
                            )
                            .clicked()
                        {
                            confirm_path = self.vst_picker.selected_path.clone();
                        }
                    });
                });
            });

        if let Some(path) = confirm_path {
            let res = match self.vst_picker.target_line {
                ActiveLineTarget::LineA => self.pipeline_a.add_vst(&path),
                ActiveLineTarget::LineB => self.pipeline_b.add_vst(&path),
            };
            if let Err(e) = res {
                show_info_message_box("Failed to Load VST", &e);
            } else {
                self.trigger_auto_save();
            }
            close_modal = true;
        }

        if close_modal {
            self.vst_picker.open = false;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_flexasio_profile_card(
        ui: &mut egui::Ui,
        card_id: &str,
        title: &str,
        subtitle: &str,
        accent_col: Color32,
        prof: &mut FlexAsioProfileConfig,
        in_items: &[(String, String)],
        out_items: &[(String, String)],
        sample_rate: u32,
    ) -> (bool, bool) {
        let prev = prof.clone();
        let mut dev_changed = false;
        let mut param_changed = false;

        egui::Frame::none()
            .fill(Palette::BG_PANEL_ALT)
            .stroke(Stroke::new(1.2_f32, accent_col))
            .rounding(Rounding::same(10.0))
            .inner_margin(Margin::symmetric(12.0, 10.0))
            .show(ui, |ui| {
                let full_w = ui.available_width();
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(title)
                            .font(FontId::new(14.0, FontFamily::Proportional))
                            .strong()
                            .color(accent_col),
                    );
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(subtitle)
                            .size(11.5)
                            .color(Palette::TEXT_MUTED),
                    );

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let eff_sr = sample_rate.max(44100) as f64;
                        let cur_ms = (prof.buffer_size as f64 / eff_sr) * 1000.0;
                        egui::ComboBox::from_id_source(format!("cb_prof_smp_{}", card_id))
                            .selected_text(format!("{} smp ({:.1} ms)", prof.buffer_size, cur_ms))
                            .width(155.0)
                            .show_ui(ui, |ui| {
                                for cand in [32u32, 64, 96, 128, 192, 256, 480, 512, 1024, 2048] {
                                    let ms = (cand as f64 / eff_sr) * 1000.0;
                                    ui.selectable_value(
                                        &mut prof.buffer_size,
                                        cand,
                                        format!("{} smp ({:.1} ms)", cand, ms),
                                    );
                                }
                            });
                        ui.label(
                            RichText::new("Buffer Size:")
                                .size(11.5)
                                .strong()
                                .color(Palette::TEXT_SECONDARY),
                        );
                    });
                });

                ui.add_space(6.0);

                // Baris 1: 2 Kolom Dropdown Input (Kiri) & Output (Kanan) seperti FlexASIO GUI
                let col_w = ((full_w - 14.0) * 0.5).max(160.0);
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(col_w);
                        ui.label(
                            RichText::new("🎤 Input Device:")
                                .size(11.5)
                                .strong()
                                .color(Palette::TEXT_SECONDARY),
                        );
                        if draw_v122_device_dropdown(
                            ui,
                            &format!("dd_prof_in_{}", card_id),
                            &mut prof.input_device,
                            "(None)",
                            in_items,
                            col_w,
                        ) {
                            dev_changed = true;
                        }
                    });

                    ui.add_space(10.0);

                    ui.vertical(|ui| {
                        ui.set_width(col_w);
                        ui.label(
                            RichText::new("🔊 Output Device:")
                                .size(11.5)
                                .strong()
                                .color(Palette::TEXT_SECONDARY),
                        );
                        if draw_v122_device_dropdown(
                            ui,
                            &format!("dd_prof_out_{}", card_id),
                            &mut prof.output_device,
                            "(None)",
                            out_items,
                            col_w,
                        ) {
                            dev_changed = true;
                        }
                    });
                });

                ui.add_space(6.0);

                // Baris 2: Latency | [x] Set Input Latency [0.0] | [x] Set Output Latency [0.0]
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [68.0, 22.0],
                        egui::Label::new(
                            RichText::new("Latency")
                                .size(11.5)
                                .strong()
                                .color(Palette::TEXT_SECONDARY),
                        ),
                    );

                    ui.allocate_ui(Vec2::new(col_w - 36.0, 22.0), |ui| {
                        ui.horizontal(|ui| {
                            ui.checkbox(
                                &mut prof.set_in_latency_enabled,
                                RichText::new("Set Input Latency")
                                    .size(11.5)
                                    .color(Palette::TEXT_PRIMARY),
                            );
                            ui.add_enabled(
                                prof.set_in_latency_enabled,
                                egui::DragValue::new(&mut prof.in_latency_val)
                                    .speed(0.1)
                                    .clamp_range(0.0..=50.0)
                                    .fixed_decimals(1),
                            );
                        });
                    });

                    ui.add_space(10.0);

                    ui.allocate_ui(Vec2::new(col_w - 36.0, 22.0), |ui| {
                        ui.horizontal(|ui| {
                            ui.checkbox(
                                &mut prof.set_out_latency_enabled,
                                RichText::new("Set Output Latency")
                                    .size(11.5)
                                    .color(Palette::TEXT_PRIMARY),
                            );
                            ui.add_enabled(
                                prof.set_out_latency_enabled,
                                egui::DragValue::new(&mut prof.out_latency_val)
                                    .speed(0.1)
                                    .clamp_range(0.0..=50.0)
                                    .fixed_decimals(1),
                            );
                        });
                    });
                });

                // Baris 3: WASAPI | [x] Set: [ ] Exclusive [x] AutoConvert | [x] Set: [ ] Exclusive [x] AutoConvert
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [68.0, 22.0],
                        egui::Label::new(
                            RichText::new("WASAPI")
                                .size(11.5)
                                .strong()
                                .color(Palette::TEXT_SECONDARY),
                        ),
                    );

                    ui.allocate_ui(Vec2::new(col_w - 36.0, 22.0), |ui| {
                        ui.horizontal(|ui| {
                            ui.checkbox(
                                &mut prof.wasapi_in_set,
                                RichText::new("Set:").size(11.5).color(Palette::TEXT_PRIMARY),
                            );
                            ui.add_enabled_ui(prof.wasapi_in_set, |ui| {
                                ui.checkbox(
                                    &mut prof.wasapi_in_exclusive,
                                    RichText::new("Exclusive")
                                        .size(11.5)
                                        .color(Palette::TEXT_PRIMARY),
                                );
                                ui.checkbox(
                                    &mut prof.wasapi_in_autoconvert,
                                    RichText::new("AutoConvert")
                                        .size(11.5)
                                        .color(Palette::TEXT_PRIMARY),
                                );
                            });
                        });
                    });

                    ui.add_space(10.0);

                    ui.allocate_ui(Vec2::new(col_w - 36.0, 22.0), |ui| {
                        ui.horizontal(|ui| {
                            ui.checkbox(
                                &mut prof.wasapi_out_set,
                                RichText::new("Set:").size(11.5).color(Palette::TEXT_PRIMARY),
                            );
                            ui.add_enabled_ui(prof.wasapi_out_set, |ui| {
                                ui.checkbox(
                                    &mut prof.wasapi_out_exclusive,
                                    RichText::new("Exclusive")
                                        .size(11.5)
                                        .color(Palette::TEXT_PRIMARY),
                                );
                                ui.checkbox(
                                    &mut prof.wasapi_out_autoconvert,
                                    RichText::new("AutoConvert")
                                        .size(11.5)
                                        .color(Palette::TEXT_PRIMARY),
                                );
                            });
                        });
                    });
                });

                // Baris 4: Channels | [ 0 ] | [ 0 ] (0 = Auto)
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [68.0, 22.0],
                        egui::Label::new(
                            RichText::new("Channels")
                                .size(11.5)
                                .strong()
                                .color(Palette::TEXT_SECONDARY),
                        ),
                    );

                    ui.allocate_ui(Vec2::new(col_w - 36.0, 22.0), |ui| {
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::DragValue::new(&mut prof.in_channels)
                                    .speed(1.0)
                                    .clamp_range(0..=16),
                            );
                            ui.label(
                                RichText::new("(0 = Auto Stereo)")
                                    .size(10.5)
                                    .color(Palette::TEXT_MUTED),
                            );
                        });
                    });

                    ui.add_space(10.0);

                    ui.allocate_ui(Vec2::new(col_w - 36.0, 22.0), |ui| {
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::DragValue::new(&mut prof.out_channels)
                                    .speed(1.0)
                                    .clamp_range(0..=16),
                            );
                            ui.label(
                                RichText::new("(0 = Auto Stereo)")
                                    .size(10.5)
                                    .color(Palette::TEXT_MUTED),
                            );
                        });
                    });
                });
            });

        if prof.buffer_size != prev.buffer_size
            || prof.set_in_latency_enabled != prev.set_in_latency_enabled
            || (prof.in_latency_val - prev.in_latency_val).abs() > 1e-4
            || prof.set_out_latency_enabled != prev.set_out_latency_enabled
            || (prof.out_latency_val - prev.out_latency_val).abs() > 1e-4
            || prof.wasapi_in_set != prev.wasapi_in_set
            || prof.wasapi_in_exclusive != prev.wasapi_in_exclusive
            || prof.wasapi_in_autoconvert != prev.wasapi_in_autoconvert
            || prof.wasapi_out_set != prev.wasapi_out_set
            || prof.wasapi_out_exclusive != prev.wasapi_out_exclusive
            || prof.wasapi_out_autoconvert != prev.wasapi_out_autoconvert
            || prof.in_channels != prev.in_channels
            || prof.out_channels != prev.out_channels
        {
            param_changed = true;
        }

        (dev_changed, param_changed)
    }

    fn copy_shared_asio_params(src: &FlexAsioProfileConfig, dst: &mut FlexAsioProfileConfig) {
        dst.buffer_size = src.buffer_size;
        dst.set_in_latency_enabled = src.set_in_latency_enabled;
        dst.in_latency_val = src.in_latency_val;
        dst.set_out_latency_enabled = src.set_out_latency_enabled;
        dst.out_latency_val = src.out_latency_val;
        dst.wasapi_in_set = src.wasapi_in_set;
        dst.wasapi_in_exclusive = src.wasapi_in_exclusive;
        dst.wasapi_in_autoconvert = src.wasapi_in_autoconvert;
        dst.wasapi_out_set = src.wasapi_out_set;
        dst.wasapi_out_exclusive = src.wasapi_out_exclusive;
        dst.wasapi_out_autoconvert = src.wasapi_out_autoconvert;
        dst.in_channels = src.in_channels;
        dst.out_channels = src.out_channels;
    }


    /// Merender panel:
    /// 1) Safe GPU Boost Clock Lock (Toggle ON/OFF + Target MHz + Live 1-Second GPU Clock Monitor + Indikator Keamanan)
    /// 2) Kotak Catatan / Notes (`Savedata\asio_performance_notes.txt`) di bagian paling bawah setiap jendela ASIO Settings.
    fn draw_gpu_boost_lock_and_notes_section(&mut self, ui: &mut egui::Ui, id_salt: &str) {
        let mut trigger_save = false;
        let auto_boost_cap_mhz = self.gpu_telemetry.max_graphics_mhz.max(1350);

        // =====================================================================
        // BAGIAN A: SAFE GPU BOOST CLOCK LOCK (DISEMBUNYIKAN SECARA DEFAULT)
        // =====================================================================
        let lock_border_col = if self.gpu_boost_lock_enabled {
            Palette::ACCENT_GREEN
        } else if self.gpu_boost_lock_ui_open {
            Palette::ACCENT_YELLOW
        } else {
            Palette::BORDER_STRONG
        };
        egui::Frame::none()
            .fill(Palette::BG_PANEL_ALT)
            .stroke(Stroke::new(1.1_f32, lock_border_col))
            .rounding(Rounding::same(10.0))
            .inner_margin(Margin::symmetric(10.0, 6.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let expand_txt = if self.gpu_boost_lock_ui_open {
                        "▾ Hide Built-in GPU Boost Clock Lock"
                    } else if self.gpu_boost_lock_enabled {
                        "▸ Built-in GPU Boost Lock: ACTIVE (Click to Open)"
                    } else {
                        "▸ Show Built-in GPU Boost Clock Lock (Optional — Hidden by Default)"
                    };
                    let btn_expand = egui::Button::new(
                        RichText::new(expand_txt)
                            .font(FontId::new(11.5, FontFamily::Proportional))
                            .strong()
                            .color(if self.gpu_boost_lock_enabled {
                                Palette::ACCENT_GREEN
                            } else {
                                Palette::ACCENT_PINK
                            }),
                    )
                    .fill(Palette::BG_INPUT)
                    .stroke(Stroke::new(1.0_f32, Palette::BORDER_STRONG))
                    .rounding(Rounding::same(6.0));

                    if ui
                        .add_sized([410.0, 24.0], btn_expand)
                        .on_hover_text(
                            "Click to show or hide the built-in GPU Boost Clock Lock panel.
                             Note: Using a 3rd-party tool (MSI Afterburner / EVGA Precision X1) is recommended; this built-in lock is provided only if needed.",
                        )
                        .clicked()
                    {
                        self.gpu_boost_lock_ui_open = !self.gpu_boost_lock_ui_open;
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let temp_str = if self.gpu_telemetry.gpu_temp_c > 0 {
                            format!(" | {}°C", self.gpu_telemetry.gpu_temp_c)
                        } else {
                            String::new()
                        };
                        let live_txt = if self.gpu_telemetry.available {
                            if self.gpu_boost_lock_enabled {
                                format!(
                                    "🟢 LIVE GPU CLOCK: {} MHz (LOCKED STABLE{})",
                                    self.gpu_telemetry.current_graphics_mhz, temp_str
                                )
                            } else {
                                format!(
                                    "⚡ LIVE GPU CLOCK: {} MHz (Dynamic ↕{})",
                                    self.gpu_telemetry.current_graphics_mhz, temp_str
                                )
                            }
                        } else {
                            "⚡ GPU Clock: External Tool (Afterburner / Precision X1)".to_string()
                        };

                        egui::Frame::none()
                            .fill(Palette::BG_INPUT)
                            .stroke(Stroke::new(
                                1.0_f32,
                                if self.gpu_boost_lock_enabled {
                                    Palette::ACCENT_GREEN
                                } else {
                                    Palette::ACCENT_BLUE
                                },
                            ))
                            .rounding(Rounding::same(6.0))
                            .inner_margin(Margin::symmetric(8.0, 3.0))
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new(live_txt)
                                        .font(FontId::new(11.0, FontFamily::Monospace))
                                        .strong()
                                        .color(if self.gpu_boost_lock_enabled {
                                            Palette::ACCENT_GREEN
                                        } else {
                                            Palette::ACCENT_YELLOW
                                        }),
                                );
                            });
                    });
                });

                if self.gpu_boost_lock_ui_open {
                    ui.add_space(6.0);

                    ui.horizontal(|ui| {
                        let (btn_txt, btn_fill, btn_stroke) = if self.gpu_boost_lock_enabled {
                            (
                                format!(
                                    "🔒 GPU BOOST LOCK: ON @ {} MHz (Click to Unlock)",
                                    self.gpu_telemetry.locked_target_mhz.max(auto_boost_cap_mhz)
                                ),
                                Palette::ACCENT_GREEN_DEEP,
                                Palette::ACCENT_GREEN,
                            )
                        } else {
                            (
                                "🔓 GPU BOOST LOCK: OFF (Click to Lock Max Boost)".to_string(),
                                Palette::BG_INPUT,
                                Palette::ACCENT_YELLOW,
                            )
                        };

                        let toggle_btn = egui::Button::new(
                            RichText::new(btn_txt)
                                .size(11.5)
                                .strong()
                                .color(Color32::WHITE),
                        )
                        .fill(btn_fill)
                        .stroke(Stroke::new(1.2_f32, btn_stroke))
                        .rounding(Rounding::same(6.0));

                        if ui
                            .add_sized([285.0, 27.0], toggle_btn)
                            .on_hover_text(
                                "Automatically locks your GPU Boost Clock to your GPU's native maximum boost capability (identical to EVGA Precision X1 Boost Lock).
                                 • ZERO OVERCLOCKING: Uses your GPU's factory VBIOS Boost ceiling — no manual frequency override.
                                 • Note: A 3rd-party GPU utility (MSI Afterburner / EVGA Precision X1) is recommended first; use this built-in lock only if needed.",
                            )
                            .clicked()
                        {
                            let target_state = !self.gpu_boost_lock_enabled;
                            match set_gpu_boost_clock_lock_with_failsafe(
                                target_state,
                                self.gpu_boost_lock_failsafe,
                            ) {
                                Ok(_msg) => {
                                    self.gpu_boost_lock_enabled = target_state;
                                    self.gpu_telemetry = poll_gpu_clock_telemetry_with_failsafe(
                                        self.gpu_boost_lock_failsafe,
                                    );
                                    self.gpu_boost_lock_mhz = self.gpu_telemetry.max_graphics_mhz;
                                    trigger_save = true;
                                }
                                Err(err_msg) => {
                                    self.gpu_boost_lock_enabled = false;
                                    self.gpu_telemetry = poll_gpu_clock_telemetry_with_failsafe(
                                        self.gpu_boost_lock_failsafe,
                                    );
                                    show_info_message_box(
                                        "GPU Boost Clock Lock — Failsafe Auto-Cancel",
                                        &err_msg,
                                    );
                                }
                            }
                        }

                        ui.add_space(6.0);

                        egui::Frame::none()
                            .fill(Palette::BG_INPUT)
                            .stroke(Stroke::new(1.0_f32, Palette::BORDER_STRONG))
                            .rounding(Rounding::same(6.0))
                            .inner_margin(Margin::symmetric(10.0, 4.0))
                            .show(ui, |ui| {
                                let cap_txt = if self.gpu_telemetry.available {
                                    format!(
                                        "🎯 Your Max GPU Boost Capability: {} MHz (Auto — No OC)",
                                        auto_boost_cap_mhz
                                    )
                                } else {
                                    "🎯 Max GPU Boost Capability: Auto-Detected via Driver".to_string()
                                };
                                ui.label(
                                    RichText::new(cap_txt)
                                        .size(11.3)
                                        .strong()
                                        .color(Palette::ACCENT_BLUE),
                                )
                                .on_hover_text(
                                    "Read-only indicator showing your GPU's factory maximum Boost Clock capability.
                                     Just like EVGA Precision X1 Boost Lock, this is 100% automatic (no manual overclocking).",
                                );
                            });
                    });

                    ui.add_space(4.0);

                    egui::Frame::none()
                        .fill(Palette::BG_INPUT)
                        .stroke(Stroke::new(1.0_f32, Palette::BORDER))
                        .rounding(Rounding::same(6.0))
                        .inner_margin(Margin::symmetric(8.0, 4.0))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let fs_col = if self.gpu_boost_lock_failsafe {
                                    Palette::ACCENT_GREEN
                                } else {
                                    Palette::ACCENT_YELLOW
                                };
                                if ui
                                    .checkbox(
                                        &mut self.gpu_boost_lock_failsafe,
                                        RichText::new(
                                            "🛡️ Auto Failsafe Protection (Auto-Cancel & Unlock on GPU Error / Thermal >= 83°C / Driver Reset)",
                                        )
                                        .size(11.2)
                                        .strong()
                                        .color(fs_col),
                                    )
                                    .on_hover_text(
                                        "Recommended ON (Default):
                                         • Continuously monitors GPU health, driver status, and temperature every 1 second.
                                         • If any GPU error, abnormal clock collapse, or high temperature (>= 83°C) occurs during Boost Lock, Rinka SoundBridge immediately cancels the lock and restores normal dynamic GPU clocking.",
                                    )
                                    .changed()
                                {
                                    trigger_save = true;
                                }
                            });

                            ui.label(
                                RichText::new(format!(
                                    "🛡️ GPU: {} | Status: {} | Tip: Turn Boost Lock OFF after using Voice Changer to save idle GPU power.",
                                    self.gpu_telemetry.gpu_name, self.gpu_telemetry.last_status_msg
                                ))
                                .size(10.3)
                                .color(Palette::TEXT_SECONDARY),
                            );
                        });
                }
            });

        ui.add_space(6.0);

        // =====================================================================
        // BAGIAN B: KOTAK CATATAN MANUAL (Savedata\asio_performance_notes.txt)
        // =====================================================================
        egui::Frame::none()
            .fill(Palette::BG_PANEL_ALT)
            .stroke(Stroke::new(1.0_f32, Palette::BORDER_STRONG))
            .rounding(Rounding::same(10.0))
            .inner_margin(Margin::symmetric(10.0, 6.0))
            .show(ui, |ui| {
                let notes_path = get_asio_notes_txt_path();
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("📝 ASIO Performance & Delay Notes (Savedata\\asio_performance_notes.txt)")
                            .font(FontId::new(11.8, FontFamily::Proportional))
                            .strong()
                            .color(Palette::ACCENT_PINK),
                    );

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .small_button("📂 Open .txt in Notepad")
                            .on_hover_text(format!(
                                "Open {} directly in Windows Notepad for manual editing",
                                notes_path.to_string_lossy()
                            ))
                            .clicked()
                        {
                            save_asio_notes_txt(&self.asio_notes_text);
                            open_file_in_notepad(&notes_path);
                        }

                        if ui
                            .small_button("🔄 Reload .txt")
                            .on_hover_text("Reload text after editing Savedata\\asio_performance_notes.txt manually")
                            .clicked()
                        {
                            self.asio_notes_text = load_or_create_asio_notes_txt();
                        }

                        if ui
                            .small_button("↺ Reset Default Tip")
                            .on_hover_text("Restore default English recommendation about GPU Clock Boost locking")
                            .clicked()
                        {
                            self.asio_notes_text = DEFAULT_ASIO_NOTES_TEXT.to_string();
                            save_asio_notes_txt(&self.asio_notes_text);
                        }
                    });
                });

                ui.add_space(3.0);

                let te_resp = ui.add_sized(
                    [ui.available_width(), 56.0],
                    egui::TextEdit::multiline(&mut self.asio_notes_text)
                        .id_source(format!("asio_notes_txt_edit_{}", id_salt))
                        .font(FontId::new(11.0, FontFamily::Proportional))
                        .desired_rows(3),
                );
                if te_resp.changed() {
                    save_asio_notes_txt(&self.asio_notes_text);
                }
            });

        if trigger_save {
            self.trigger_auto_save();
        }
    }

    fn render_triple_asio_setting_modal(&mut self, ctx: &egui::Context) {
        if !self.asio_setting_open {
            return;
        }

        let mut close_win = false;
        let mut do_scan = false;
        let mut do_apply_with_popup = false;
        let mut any_changed = false;
        let mut changed_source_idx: Option<usize> = None;

        let in_items: Vec<(String, String)> = self
            .wasapi_inputs
            .iter()
            .map(|d| (d.name.clone(), format!("{} [{} In • WASAPI]", d.name, d.max_channels)))
            .collect();
        let out_items: Vec<(String, String)> = self
            .wasapi_outputs
            .iter()
            .map(|d| (d.name.clone(), format!("{} [{} Out • WASAPI]", d.name, d.max_channels)))
            .collect();

        let sr = self.pipeline_a.sample_rate;
        let mut prof_a = self.pipeline_a.asio_profile.clone();
        let mut prof_b = self.pipeline_b.asio_profile.clone();
        let mut prof_vc = self.asio_vc_profile.clone();

        let screen_rect = ctx.screen_rect();
        let triple_w = (screen_rect.width() - 28.0).clamp(680.0, 920.0);
        let triple_h = (screen_rect.height() - 32.0).clamp(580.0, 880.0);
        let triple_pos = Pos2::new(
            (screen_rect.center().x - triple_w * 0.5).max(12.0),
            (screen_rect.center().y - triple_h * 0.5).max(12.0),
        );
        egui::Window::new("⚙️ ASIO Settings — Triple ASIO Manager")
            .collapsible(false)
            .resizable(true)
            .constrain(true)
            .default_pos(triple_pos)
            .default_size([triple_w, triple_h])
            .show(ctx, |ui| {
                ui.set_width(triple_w);
                ui.set_max_width(triple_w);
                // Header Bar Triple ASIO Setting
                egui::Frame::none()
                    .fill(Palette::BG_INPUT)
                    .stroke(Stroke::new(1.0_f32, Palette::BORDER))
                    .rounding(Rounding::same(8.0))
                    .inner_margin(Margin::symmetric(10.0, 8.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui
                                .checkbox(
                                    &mut self.sync_triple_asio,
                                    RichText::new(
                                        "🔗 Sync Unified Settings (Buffer smp, Latency & WASAPI stay synchronized across all 3 ASIO engines)",
                                    )
                                    .size(12.0)
                                    .strong()
                                    .color(Palette::ACCENT_PINK),
                                )
                                .changed()
                            {
                                if self.sync_triple_asio {
                                    changed_source_idx = Some(0);
                                }
                                any_changed = true;
                            }

                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let btn_scan = egui::Button::new(
                                    RichText::new("🔄 Rescan Devices")
                                        .size(11.5)
                                        .strong()
                                        .color(Color32::WHITE),
                                )
                                .fill(Palette::ACCENT_GREEN_DEEP)
                                .rounding(Rounding::same(6.0));
                                if ui.add_sized([155.0, 26.0], btn_scan).clicked() {
                                    do_scan = true;
                                }
                            });
                        });
                    });

                ui.add_space(6.0);

                egui::ScrollArea::vertical()
                    .id_source("scroll_triple_asio_modal")
                    .max_height(500.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        // 1. ASIO LINE A
                        let (dev_a, par_a) = Self::draw_flexasio_profile_card(
                            ui,
                            "line_a",
                            "1. ASIO Line A (Internal RSB)",
                            "Mic Input -> Loopback 1 Output path (e.g. Input 1 -> Output 1/2)",
                            Palette::ACCENT_PINK,
                            &mut prof_a,
                            &in_items,
                            &out_items,
                            sr,
                        );
                        if dev_a || par_a {
                            any_changed = true;
                        }
                        if par_a {
                            changed_source_idx = Some(0);
                        }

                        ui.add_space(8.0);

                        // 2. ASIO LINE B
                        let (dev_b, par_b) = Self::draw_flexasio_profile_card(
                            ui,
                            "line_b",
                            "2. ASIO Line B (Internal RSB)",
                            "Loopback 2 Input -> Speaker / Stream path (e.g. Loopback 2 L/R -> Headphones)",
                            Palette::ACCENT_GREEN,
                            &mut prof_b,
                            &in_items,
                            &out_items,
                            sr,
                        );
                        if dev_b || par_b {
                            any_changed = true;
                        }
                        if par_b {
                            changed_source_idx = Some(1);
                        }

                        ui.add_space(8.0);

                        // 3. ASIO KHUSUS VC ("Rinka SoundBridge ASIO")
                        let (dev_vc, par_vc) = Self::draw_flexasio_profile_card(
                            ui,
                            "asio_vc",
                            "3. Dedicated VC ASIO — Driver: \"Rinka SoundBridge ASIO\"",
                            "Select dedicated In & Out for Voice Changer / Vonovox (e.g. In = Loopback 1 L/R -> Out = Output 3/4)",
                            Palette::ACCENT_LAVENDER,
                            &mut prof_vc,
                            &in_items,
                            &out_items,
                            sr,
                        );
                        if dev_vc || par_vc {
                            any_changed = true;
                        }
                        if par_vc {
                            changed_source_idx = Some(2);
                        }

                        ui.add_space(8.0);
                        self.draw_gpu_boost_lock_and_notes_section(ui, "triple_asio");
                    });

                ui.add_space(8.0);

                ui.horizontal(|ui| {
                    let btn_apply = egui::Button::new(
                        RichText::new("🔗 Save & Apply to All 3 ASIO Engines (Update 'Rinka SoundBridge ASIO')")
                            .size(12.5)
                            .strong()
                            .color(Color32::WHITE),
                    )
                    .fill(Palette::ACCENT_BLUE_DEEP)
                    .stroke(Stroke::new(1.0_f32, Palette::ACCENT_PINK))
                    .rounding(Rounding::same(6.0));
                    if ui.add_sized([460.0, 32.0], btn_apply).clicked() {
                        do_apply_with_popup = true;
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let btn_close = egui::Button::new(
                            RichText::new("Close Window")
                                .size(12.0)
                                .strong()
                                .color(Palette::TEXT_PRIMARY),
                        )
                        .fill(Palette::BG_INPUT)
                        .stroke(Stroke::new(1.0_f32, Palette::BORDER))
                        .rounding(Rounding::same(6.0));
                        if ui.add_sized([130.0, 32.0], btn_close).clicked() {
                            close_win = true;
                        }
                    });
                });
            });

        if let Some(src_idx) = changed_source_idx {
            if self.sync_triple_asio {
                match src_idx {
                    0 => {
                        Self::copy_shared_asio_params(&prof_a, &mut prof_b);
                        Self::copy_shared_asio_params(&prof_a, &mut prof_vc);
                    }
                    1 => {
                        Self::copy_shared_asio_params(&prof_b, &mut prof_a);
                        Self::copy_shared_asio_params(&prof_b, &mut prof_vc);
                    }
                    _ => {
                        Self::copy_shared_asio_params(&prof_vc, &mut prof_a);
                        Self::copy_shared_asio_params(&prof_vc, &mut prof_b);
                    }
                }
            }
        }

        if any_changed || do_apply_with_popup {
            self.pipeline_a.asio_profile = prof_a.clone();
            self.pipeline_a.input_device_name = if prof_a.input_device.is_empty() {
                None
            } else {
                Some(prof_a.input_device.clone())
            };
            self.pipeline_a.output_device_name = if prof_a.output_device.is_empty() {
                None
            } else {
                Some(prof_a.output_device.clone())
            };
            self.pipeline_a.buffer_size = prof_a.buffer_size;

            self.pipeline_b.asio_profile = prof_b.clone();
            self.pipeline_b.input_device_name = if prof_b.input_device.is_empty() {
                None
            } else {
                Some(prof_b.input_device.clone())
            };
            self.pipeline_b.output_device_name = if prof_b.output_device.is_empty() {
                None
            } else {
                Some(prof_b.output_device.clone())
            };
            self.pipeline_b.buffer_size = prof_b.buffer_size;

            self.asio_vc_profile = prof_vc;

            self.pipeline_a.restart_stream();
            self.pipeline_b.restart_stream();
            self.save_config_now();

            if do_apply_with_popup {
                match self.sync_unified_asio_config(true) {
                    Ok(msg) => show_info_message_box("Triple ASIO — Rinka SoundBridge ASIO", &msg),
                    Err(e) => show_info_message_box("Triple ASIO — Rinka SoundBridge ASIO", &e),
                }
            } else {
                let _ = self.sync_unified_asio_config(false);
            }
        }

        if do_scan {
            self.refresh_audio_devices();
        }
        if close_win {
            self.asio_setting_open = false;
        }
    }

    fn draw_tactile_pill(
        ui: &mut egui::Ui,
        label: &str,
        active: bool,
        accent_col: Color32,
        min_w: f32,
    ) -> Response {
        let fill = if active {
            Color32::from_rgb(0x16, 0x36, 0x4A)
        } else {
            Palette::BG_INPUT
        };
        let border = if active {
            Stroke::new(1.4_f32, accent_col)
        } else {
            Stroke::new(1.0_f32, Palette::BORDER)
        };
        let text_col = if active {
            accent_col
        } else {
            Palette::TEXT_SECONDARY
        };
        let btn = egui::Button::new(
            RichText::new(label)
                .size(11.0)
                .strong()
                .color(text_col),
        )
        .fill(fill)
        .stroke(border)
        .rounding(Rounding::same(5.0));
        ui.add_sized([min_w, 22.0], btn)
    }

    fn render_dsp_rsb_setting_modal(&mut self, ctx: &egui::Context) {
        if !self.dsp_rsb_setting_open {
            return;
        }

        let mut close_win = false;
        let mut do_scan = false;
        let mut do_apply_popup = false;
        let mut reset_optimal = false;

        let in_items: Vec<(String, String)> = self
            .wasapi_inputs
            .iter()
            .map(|d| (d.name.clone(), format!("{} [{} In]", d.name, d.max_channels)))
            .collect();
        let out_items: Vec<(String, String)> = self
            .wasapi_outputs
            .iter()
            .map(|d| (d.name.clone(), format!("{} [{} Out]", d.name, d.max_channels)))
            .collect();

        let rsb_global_sr = self.pipeline_a.sample_rate;
        let rsb_global_buf = self.pipeline_a.buffer_size;
        let prev_cfg = self.dsp_rsb_config.clone();
        let mut cfg = self.dsp_rsb_config.clone();
        let mut sel_in_a = self.pipeline_a.input_device_name.clone().unwrap_or_default();
        let mut sel_out_b = self.pipeline_b.output_device_name.clone().unwrap_or_default();
        let prev_in_a = sel_in_a.clone();
        let prev_out_b = sel_out_b.clone();

        // Hitung ukuran modal dari ukuran layar utama (`screen_rect`) agar TIDAK PERNAH membesar sendiri
        // (zero feedback loop) dan TIDAK PERNAH terpotong (crop) di sisi kiri/kanan/bawah!
        let screen_rect = ctx.screen_rect();
        let modal_w = (screen_rect.width() - 28.0).clamp(660.0, 880.0);
        let max_scroll_h = (screen_rect.height() - 84.0).clamp(540.0, 840.0);
        let default_pos = Pos2::new(
            (screen_rect.center().x - modal_w * 0.5).max(10.0),
            (screen_rect.center().y - (max_scroll_h + 44.0) * 0.5).max(10.0),
        );

        egui::Window::new("⚙️ SoundBridge ASIO — Adaptive Engine & Extra Buffer")
            .collapsible(false)
            .resizable(true)
            .constrain(true)
            .default_pos(default_pos)
            .default_size([modal_w, max_scroll_h + 44.0])
            .show(ctx, |ui| {
                ui.set_width(modal_w);
                ui.set_max_width(modal_w);

                let eff_sr = rsb_global_sr.max(44100) as f64;
                let rsb_ms = (rsb_global_buf as f64 / eff_sr) * 1000.0;

                let live_cush_smp = (((cfg.dsp_b_cushion_ms.clamp(20.0, 450.0) as f64) / 1000.0) * eff_sr).round() as u32;
                get_dsp_rsb_bus_b_in().enforce_rsb_dsp_b_config(
                    rsb_global_sr.max(44100),
                    cfg.effective_buffer_size(rsb_global_buf),
                    live_cush_smp.min(24000),
                    cfg.adaptive_cushion,
                    cfg.adaptive_reported_latency,
                    cfg.gpu_stall_multiplier,
                    cfg.adaptive_mode as u8,
                    cfg.adaptive_extra_ms,
                );

                let live_raw_ms = get_dsp_rsb_bus_b_in().get_measured_gpu_stall_ms();
                let extra_ms = cfg.adaptive_extra_ms.clamp(0.0, 250.0);
                let eff_assumed_ms = if live_raw_ms >= 1.0 {
                    live_raw_ms + extra_ms
                } else {
                    cfg.dsp_b_cushion_ms + extra_ms
                };
                let (active_cush_smp, _) = get_dsp_rsb_bus_b_in().get_dsp_b_params();
                let active_cush_ms = (active_cush_smp as f64 / eff_sr) * 1000.0;
                let is_cush_unsafe = live_raw_ms > 1.0 && (active_cush_ms as f32) <= live_raw_ms;

                egui::ScrollArea::vertical()
                    .id_source("scroll_dsp_rsb_modal")
                    .max_height(max_scroll_h)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        let inner_w = (modal_w - 8.0).max(440.0);
                        ui.set_width(inner_w);
                        ui.set_max_width(inner_w);

                        // =========================================================================
                        // 0. TOP LIVE TELEMETRY & FORMULA BANNER
                        // =========================================================================
                        egui::Frame::none()
                            .fill(Color32::from_rgb(0x18, 0x22, 0x29))
                            .stroke(Stroke::new(
                                1.2_f32,
                                if is_cush_unsafe {
                                    Palette::ACCENT_CORAL
                                } else {
                                    Palette::ACCENT_GREEN
                                },
                            ))
                            .rounding(Rounding::same(8.0))
                            .inner_margin(Margin::symmetric(10.0, 6.0))
                            .show(ui, |ui| {
                                ui.set_max_width(inner_w - 20.0);
                                ui.horizontal(|ui| {
                                    let is_active = live_raw_ms >= 1.0;
                                    let dot_col = if is_cush_unsafe {
                                        Palette::ACCENT_CORAL
                                    } else if is_active {
                                        Palette::ACCENT_GREEN
                                    } else {
                                        Palette::ACCENT_BLUE
                                    };
                                    let (dot_r, _) = ui.allocate_exact_size(Vec2::new(10.0, 14.0), Sense::hover());
                                    ui.painter().circle_filled(dot_r.center(), 4.2, dot_col);

                                    let phys_fill_ms = get_dsp_rsb_bus_b_in().get_physical_fill_ms();
                                    let sensor_str = match cfg.adaptive_mode {
                                        AdaptiveCushionMode::PySyncInjection => {
                                            let pk = (get_dsp_rsb_bus_b_in().get_stall_peak_samples() as f64 / eff_sr) * 1000.0;
                                            if pk > (live_raw_ms as f64) + 1.5 {
                                                format!("PySync {:.0}ms (Pk {:.0}ms)", live_raw_ms, pk)
                                            } else {
                                                format!("PySync {:.0}ms", live_raw_ms)
                                            }
                                        }
                                        AdaptiveCushionMode::AsioStopwatchOutlier => {
                                            format!("Stopwatch {:.0}ms", live_raw_ms)
                                        }
                                        AdaptiveCushionMode::DecoupledPacing => {
                                            format!("Paced P95 {:.0}ms", live_raw_ms)
                                        }
                                        AdaptiveCushionMode::HybridDualFusion => {
                                            let (py_ms, sw_ms, _) = get_dsp_rsb_bus_b_in().get_hybrid_dual_sensor_ms();
                                            if py_ms > 0.5 && sw_ms > 0.5 {
                                                format!("Hybrid [Py:{:.0}+SW:{:.0}] {:.0}ms", py_ms, sw_ms, live_raw_ms)
                                            } else {
                                                format!("Hybrid {:.0}ms", live_raw_ms)
                                            }
                                        }
                                        AdaptiveCushionMode::PredictivePreemptivePlc => {
                                            let (py_ms, sw_ms, _) = get_dsp_rsb_bus_b_in().get_hybrid_dual_sensor_ms();
                                            if py_ms > 0.5 && sw_ms > 0.5 {
                                                format!("Beta2 [Py:{:.0}+SW:{:.0}] {:.0}ms", py_ms, sw_ms, live_raw_ms)
                                            } else {
                                                format!("Beta2 Pred {:.0}ms", live_raw_ms)
                                            }
                                        }
                                    };

                                    let formula_str = if is_active {
                                        format!(
                                            "{} + {:.0}ms = {:.0}ms ➔ Cushion: {:.0}ms (Fill: {:.0}ms)",
                                            sensor_str, extra_ms, eff_assumed_ms, active_cush_ms, phys_fill_ms
                                        )
                                    } else {
                                        format!(
                                            "Standby | Base {:.0}ms + Extra {:.0}ms ➔ Ready Cushion: {:.0}ms",
                                            cfg.dsp_b_cushion_ms, extra_ms, active_cush_ms
                                        )
                                    };

                                    ui.label(
                                        RichText::new(formula_str)
                                            .size(11.3)
                                            .strong()
                                            .color(if is_cush_unsafe {
                                                Palette::ACCENT_CORAL
                                            } else {
                                                Palette::TEXT_PRIMARY
                                            }),
                                    )
                                    .on_hover_text(
                                        "Live Adaptive Formula:\n\
                                         Effective Assumed Delay = Sensor Delay + Extra Buffer (e.g., 50ms + 25ms = 75ms).\n\
                                         Active Cushion automatically adjusts above that value.",
                                    );

                                    ui.add_space(4.0);
                                    let btn_drop = egui::Button::new(
                                        RichText::new("↻ Drop Peak")
                                            .size(10.8)
                                            .strong()
                                            .color(Palette::ACCENT_BLUE),
                                    )
                                    .fill(Palette::BG_INPUT)
                                    .stroke(Stroke::new(1.0_f32, Palette::BORDER))
                                    .rounding(Rounding::same(5.0));
                                    if ui
                                        .add_sized([78.0, 20.0], btn_drop)
                                        .on_hover_text("Drop held spike peak & resync physical ring-buffer")
                                        .clicked()
                                    {
                                        get_dsp_rsb_bus_b_in().reset_spike_and_resync_cushion();
                                    }
                                });
                            });

                        ui.add_space(5.0);

                        // =========================================================================
                        // 1. ADAPTIVE ENGINE & MANUAL EXTRA BUFFER CARD (STACKED CLEAN ROWS)
                        // =========================================================================
                        egui::Frame::none()
                            .fill(Palette::BG_PANEL_ALT)
                            .stroke(Stroke::new(1.0_f32, Palette::BORDER_STRONG))
                            .rounding(Rounding::same(8.0))
                            .inner_margin(Margin::symmetric(10.0, 7.0))
                            .show(ui, |ui| {
                                let card_w = (inner_w - 22.0).max(420.0);
                                ui.set_width(card_w);
                                ui.set_max_width(card_w);

                                // ROW 1: Auto Adaptive + Engine Mode Dropdown + Rescan
                                ui.horizontal(|ui| {
                                    cfg.sync_buffer_size_with_rsb = true;
                                    let adap_col = if cfg.adaptive_cushion {
                                        Palette::ACCENT_GREEN
                                    } else {
                                        Palette::TEXT_SECONDARY
                                    };
                                    ui.checkbox(
                                        &mut cfg.adaptive_cushion,
                                        RichText::new("🛡️ Auto Adaptive")
                                            .size(11.3)
                                            .strong()
                                            .color(adap_col),
                                    );

                                    ui.add_space(4.0);
                                    ui.label(
                                        RichText::new("⚡ Mode:")
                                            .size(11.3)
                                            .strong()
                                            .color(Palette::TEXT_SECONDARY),
                                    );
                                    let sel_label = match cfg.adaptive_mode {
                                        AdaptiveCushionMode::PySyncInjection => "1. Mode 1 — Python [PySync]",
                                        AdaptiveCushionMode::AsioStopwatchOutlier => "2. Mode 2 — ASIO Stopwatch + Outlier",
                                        AdaptiveCushionMode::DecoupledPacing => "3. Mode 3 — Decoupled Pacing [P95]",
                                        AdaptiveCushionMode::HybridDualFusion => "4. Mode 4 — Hybrid [PySync + SW] (Default) ★",
                                        AdaptiveCushionMode::PredictivePreemptivePlc => "5. Mode 5 — Predictive Pre-Emptive + PLC [Beta2]",
                                    };
                                    egui::ComboBox::from_id_source("combo_adaptive_engine_mode")
                                        .width(235.0)
                                        .selected_text(
                                            RichText::new(sel_label)
                                                .size(11.3)
                                                .strong()
                                                .color(Palette::ACCENT_BLUE),
                                        )
                                        .show_ui(ui, |ui| {
                                            ui.selectable_value(
                                                &mut cfg.adaptive_mode,
                                                AdaptiveCushionMode::PySyncInjection,
                                                "1. Mode 1 — In-Process Python [PySync + SpikeShield]",
                                            );
                                            ui.selectable_value(
                                                &mut cfg.adaptive_mode,
                                                AdaptiveCushionMode::AsioStopwatchOutlier,
                                                "2. Mode 2 — Pro ASIO Stopwatch + Outlier Filter",
                                            );
                                            ui.selectable_value(
                                                &mut cfg.adaptive_mode,
                                                AdaptiveCushionMode::DecoupledPacing,
                                                "3. Mode 3 — Decoupled Pacing Ring Buffer [P95]",
                                            );
                                            ui.selectable_value(
                                                &mut cfg.adaptive_mode,
                                                AdaptiveCushionMode::HybridDualFusion,
                                                "4. Mode 4 — Hybrid Dual-Sensor Fusion [PySync + SW] (Default) ★",
                                            );
                                            ui.selectable_value(
                                                &mut cfg.adaptive_mode,
                                                AdaptiveCushionMode::PredictivePreemptivePlc,
                                                "5. Mode 5 — Predictive Pre-Emptive + Zero-Drop PLC [Beta2]",
                                            );
                                        });

                                    ui.add_space(4.0);
                                    let btn_scan = egui::Button::new(
                                        RichText::new("🔄 Rescan")
                                            .size(10.8)
                                            .strong()
                                            .color(Color32::WHITE),
                                    )
                                    .fill(Palette::ACCENT_GREEN_DEEP)
                                    .rounding(Rounding::same(5.0));
                                    if ui.add_sized([74.0, 21.0], btn_scan).clicked() {
                                        do_scan = true;
                                    }
                                });

                                ui.add_space(5.0);

                                // ROW 2: Manual Extra Buffer (+ms Offset — Highlighted Tactile Strip)
                                egui::Frame::none()
                                    .fill(Color32::from_rgb(0x1B, 0x24, 0x2E))
                                    .stroke(Stroke::new(1.0_f32, Palette::ACCENT_BLUE_DEEP))
                                    .rounding(Rounding::same(6.0))
                                    .inner_margin(Margin::symmetric(8.0, 5.0))
                                    .show(ui, |ui| {
                                        ui.set_max_width(card_w - 16.0);
                                        ui.horizontal(|ui| {
                                            ui.label(
                                                RichText::new("➕ Extra Buffer:")
                                                    .size(11.3)
                                                    .strong()
                                                    .color(Palette::ACCENT_BLUE),
                                            )
                                            .on_hover_text(
                                                "Manual Extra Buffer Offset (Mode 1, 2, 3 & 4):\n\
                                                 • 50ms + 25ms = 75ms\n\
                                                 • 70ms + 25ms = 95ms\n\
                                                 • 90ms + 25ms = 115ms\n\
                                                 Active Cushion auto-adjusts assuming GPU Delay has reached (Sensor + Extra).",
                                            );

                                            let btn_ex_dn = egui::Button::new(RichText::new("◀").size(9.0))
                                                .fill(Palette::BG_INPUT)
                                                .stroke(Stroke::new(1.0_f32, Palette::BORDER))
                                                .rounding(Rounding::same(4.0));
                                            if ui.add_sized([19.0, 21.0], btn_ex_dn).clicked() {
                                                cfg.adaptive_extra_ms = (cfg.adaptive_extra_ms - 5.0).clamp(0.0, 250.0);
                                            }

                                            ui.add_sized(
                                                [58.0, 21.0],
                                                egui::DragValue::new(&mut cfg.adaptive_extra_ms)
                                                    .speed(1.0)
                                                    .clamp_range(0.0..=250.0)
                                                    .fixed_decimals(0)
                                                    .prefix("+")
                                                    .suffix(" ms"),
                                            );

                                            let btn_ex_up = egui::Button::new(RichText::new("▶").size(9.0))
                                                .fill(Palette::BG_INPUT)
                                                .stroke(Stroke::new(1.0_f32, Palette::BORDER))
                                                .rounding(Rounding::same(4.0));
                                            if ui.add_sized([19.0, 21.0], btn_ex_up).clicked() {
                                                cfg.adaptive_extra_ms = (cfg.adaptive_extra_ms + 5.0).clamp(0.0, 250.0);
                                            }

                                            for (val, lbl) in [
                                                (0.0_f32, "+0"),
                                                (15.0, "+15"),
                                                (25.0, "+25 ★"),
                                                (35.0, "+35"),
                                                (50.0, "+50"),
                                                (75.0, "+75"),
                                                (100.0, "+100"),
                                            ] {
                                                let is_sel = (cfg.adaptive_extra_ms - val).abs() < 0.4;
                                                if Self::draw_tactile_pill(
                                                    ui,
                                                    lbl,
                                                    is_sel,
                                                    Palette::ACCENT_BLUE,
                                                    if val == 25.0 { 44.0 } else { 34.0 },
                                                )
                                                .clicked()
                                                {
                                                    cfg.adaptive_extra_ms = val;
                                                }
                                            }
                                        });
                                    });

                                ui.add_space(5.0);

                                // ROW 3: Base Cushion Floor (Full clean row — zero squeezing)
                                ui.horizontal(|ui| {
                                    ui.add_sized(
                                        [118.0, 21.0],
                                        egui::Label::new(
                                            RichText::new("⚓ Base Cushion Floor:")
                                                .size(11.2)
                                                .strong()
                                                .color(Palette::TEXT_SECONDARY),
                                        ),
                                    );
                                    let btn_b_dn = egui::Button::new(RichText::new("◀").size(9.0))
                                        .fill(Palette::BG_INPUT)
                                        .stroke(Stroke::new(1.0_f32, Palette::BORDER))
                                        .rounding(Rounding::same(4.0));
                                    if ui.add_sized([19.0, 21.0], btn_b_dn).clicked() {
                                        cfg.dsp_b_cushion_ms = (cfg.dsp_b_cushion_ms - 5.0).clamp(20.0, 450.0);
                                    }
                                    ui.add_sized(
                                        [58.0, 21.0],
                                        egui::DragValue::new(&mut cfg.dsp_b_cushion_ms)
                                            .speed(1.0)
                                            .clamp_range(20.0..=450.0)
                                            .fixed_decimals(0)
                                            .suffix(" ms"),
                                    );
                                    let btn_b_up = egui::Button::new(RichText::new("▶").size(9.0))
                                        .fill(Palette::BG_INPUT)
                                        .stroke(Stroke::new(1.0_f32, Palette::BORDER))
                                        .rounding(Rounding::same(4.0));
                                    if ui.add_sized([19.0, 21.0], btn_b_up).clicked() {
                                        cfg.dsp_b_cushion_ms = (cfg.dsp_b_cushion_ms + 5.0).clamp(20.0, 450.0);
                                    }

                                    for (preset_b, b_lbl) in [
                                        (40.0_f32, "40ms"),
                                        (60.0, "60ms ★"),
                                        (70.0, "70ms"),
                                        (90.0, "90ms"),
                                        (100.0, "100ms"),
                                    ] {
                                        let is_sel = (cfg.dsp_b_cushion_ms - preset_b).abs() < 0.5;
                                        if Self::draw_tactile_pill(
                                            ui,
                                            b_lbl,
                                            is_sel,
                                            Palette::ACCENT_GREEN,
                                            if preset_b == 60.0 { 48.0 } else { 40.0 },
                                        )
                                        .clicked()
                                        {
                                            cfg.dsp_b_cushion_ms = preset_b;
                                        }
                                    }
                                });

                                ui.add_space(4.0);

                                // ROW 4: Stall Multiplier (Full clean row — zero squeezing)
                                ui.add_enabled_ui(cfg.adaptive_cushion, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.add_sized(
                                            [118.0, 21.0],
                                            egui::Label::new(
                                                RichText::new("📈 Stall Multiplier:")
                                                    .size(11.2)
                                                    .strong()
                                                    .color(Palette::TEXT_SECONDARY),
                                            ),
                                        );
                                        let btn_m_dn = egui::Button::new(RichText::new("◀").size(9.0))
                                            .fill(Palette::BG_INPUT)
                                            .stroke(Stroke::new(1.0_f32, Palette::BORDER))
                                            .rounding(Rounding::same(4.0));
                                        if ui.add_sized([19.0, 21.0], btn_m_dn).clicked() {
                                            cfg.gpu_stall_multiplier =
                                                (((cfg.gpu_stall_multiplier - 0.01) * 100.0).round() / 100.0)
                                                    .clamp(1.00, 1.30);
                                        }
                                        ui.add_sized(
                                            [58.0, 21.0],
                                            egui::DragValue::new(&mut cfg.gpu_stall_multiplier)
                                                .speed(0.01)
                                                .clamp_range(1.00..=1.30)
                                                .fixed_decimals(2)
                                                .suffix("x"),
                                        );
                                        let btn_m_up = egui::Button::new(RichText::new("▶").size(9.0))
                                            .fill(Palette::BG_INPUT)
                                            .stroke(Stroke::new(1.0_f32, Palette::BORDER))
                                            .rounding(Rounding::same(4.0));
                                        if ui.add_sized([19.0, 21.0], btn_m_up).clicked() {
                                            cfg.gpu_stall_multiplier =
                                                (((cfg.gpu_stall_multiplier + 0.01) * 100.0).round() / 100.0)
                                                    .clamp(1.00, 1.30);
                                        }

                                        for (m_val, m_lbl) in [
                                            (1.00_f32, "1.00x"),
                                            (1.05, "1.05x"),
                                            (1.15, "1.15x ★"),
                                            (1.25, "1.25x"),
                                        ] {
                                            let is_sel = (cfg.gpu_stall_multiplier - m_val).abs() < 0.005;
                                            if Self::draw_tactile_pill(
                                                ui,
                                                m_lbl,
                                                is_sel,
                                                Palette::ACCENT_PINK,
                                                if (m_val - 1.15).abs() < 0.005 { 50.0 } else { 42.0 },
                                            )
                                            .clicked()
                                            {
                                                cfg.gpu_stall_multiplier = m_val;
                                            }
                                        }

                                        ui.label(
                                            RichText::new("(+15ms guard)")
                                                .size(10.5)
                                                .color(Palette::TEXT_MUTED),
                                        );
                                    });
                                });

                                ui.add_space(4.0);

                                // ROW 5: Tactile CPU Core Isolation + Synced Buffer Info
                                let host_logical_cpus = get_logical_cpu_count().max(1);
                                ui.horizontal(|ui| {
                                    ui.add_sized(
                                        [118.0, 21.0],
                                        egui::Label::new(
                                            RichText::new("⚡ ASIO CPU Cores:")
                                                .size(11.2)
                                                .strong()
                                                .color(Palette::TEXT_SECONDARY),
                                        ),
                                    );
                                    if ui
                                        .add_sized(
                                            [66.0, 21.0],
                                            egui::DragValue::new(&mut cfg.asio_cpu_cores)
                                                .speed(1.0)
                                                .clamp_range(0..=host_logical_cpus)
                                                .custom_formatter(|n, _| {
                                                    let v = n.round() as u16;
                                                    if v == 0 {
                                                        "Auto".to_string()
                                                    } else {
                                                        format!("{} Cores", v)
                                                    }
                                                }),
                                        )
                                        .changed()
                                    {
                                        cfg.cpu_cores_user_configured = true;
                                    }

                                    for preset_c in [2u16, 4, 6, 8] {
                                        if preset_c <= host_logical_cpus {
                                            let lbl = if preset_c == 2 {
                                                "2C ★".to_string()
                                            } else {
                                                format!("{}C", preset_c)
                                            };
                                            if Self::draw_tactile_pill(
                                                ui,
                                                &lbl,
                                                cfg.asio_cpu_cores == preset_c,
                                                Palette::ACCENT_GREEN,
                                                36.0,
                                            )
                                            .clicked()
                                            {
                                                cfg.asio_cpu_cores = preset_c;
                                                cfg.cpu_cores_user_configured = true;
                                            }
                                        }
                                    }
                                    if Self::draw_tactile_pill(
                                        ui,
                                        &format!("Auto ({}C)", host_logical_cpus),
                                        cfg.asio_cpu_cores == 0,
                                        Palette::ACCENT_BLUE,
                                        62.0,
                                    )
                                    .clicked()
                                    {
                                        cfg.asio_cpu_cores = 0;
                                        cfg.cpu_cores_user_configured = true;
                                    }

                                    ui.label(
                                        RichText::new(format!(
                                            "| 🔗 {} smp ({:.1}ms)",
                                            rsb_global_buf, rsb_ms
                                        ))
                                        .size(10.8)
                                        .strong()
                                        .color(Palette::ACCENT_GREEN),
                                    );
                                });
                            });

                        ui.add_space(5.0);

                        // =========================================================================
                        // 2. COMPACT 2-COLUMN AUDIO ROUTING CARD (USING ui.columns(2) — ZERO OVERFLOW)
                        // =========================================================================
                        egui::Frame::none()
                            .fill(Palette::BG_PANEL_ALT)
                            .stroke(Stroke::new(1.0_f32, Palette::BORDER_STRONG))
                            .rounding(Rounding::same(8.0))
                            .inner_margin(Margin::symmetric(10.0, 7.0))
                            .show(ui, |ui| {
                                let card_w = (inner_w - 22.0).max(420.0);
                                ui.set_width(card_w);
                                ui.set_max_width(card_w);

                                ui.columns(2, |cols| {
                                    // LEFT COLUMN: Line A Input
                                    let w0 = cols[0].available_width().max(160.0);
                                    cols[0].horizontal(|ui| {
                                        ui.label(
                                            RichText::new("🎤 Line A In (To Vonovox):")
                                                .size(11.2)
                                                .strong()
                                                .color(Palette::ACCENT_PINK),
                                        );
                                        ui.add_sized(
                                            [52.0, 18.0],
                                            egui::DragValue::new(&mut cfg.in_channels)
                                                .speed(1.0)
                                                .clamp_range(0..=16)
                                                .custom_formatter(|n, _| {
                                                    let v = n.round() as u16;
                                                    if v == 0 { "Ch:Auto".into() } else { format!("{} Ch", v) }
                                                }),
                                        );
                                    });
                                    draw_v122_device_dropdown(
                                        &mut cols[0],
                                        "dd_modal_dsp_in_a",
                                        &mut sel_in_a,
                                        "(Select Line A Input)",
                                        &in_items,
                                        w0,
                                    );

                                    // RIGHT COLUMN: Line B Output
                                    let w1 = cols[1].available_width().max(160.0);
                                    cols[1].horizontal(|ui| {
                                        ui.label(
                                            RichText::new("🔊 Line B Out (From Vonovox):")
                                                .size(11.2)
                                                .strong()
                                                .color(Palette::ACCENT_GREEN),
                                        );
                                        ui.add_sized(
                                            [52.0, 18.0],
                                            egui::DragValue::new(&mut cfg.out_channels)
                                                .speed(1.0)
                                                .clamp_range(0..=16)
                                                .custom_formatter(|n, _| {
                                                    let v = n.round() as u16;
                                                    if v == 0 { "Ch:Auto".into() } else { format!("{} Ch", v) }
                                                }),
                                        );
                                    });
                                    draw_v122_device_dropdown(
                                        &mut cols[1],
                                        "dd_modal_dsp_out_b",
                                        &mut sel_out_b,
                                        "(Select Line B Output)",
                                        &out_items,
                                        w1,
                                    );
                                });
                            });

                        ui.add_space(6.0);
                        self.draw_gpu_boost_lock_and_notes_section(ui, "dsp_rsb_asio");
                    });

                ui.add_space(5.0);

                // =========================================================================
                // 3. COMPACT FOOTER ACTION BAR (100% WITHIN modal_w)
                // =========================================================================
                ui.horizontal(|ui| {
                    let btn_apply = egui::Button::new(
                        RichText::new("💾 Save & Apply SoundBridge ASIO")
                            .size(11.8)
                            .strong()
                            .color(Color32::WHITE),
                    )
                    .fill(Palette::ACCENT_BLUE_DEEP)
                    .stroke(Stroke::new(1.0_f32, Palette::ACCENT_GREEN))
                    .rounding(Rounding::same(6.0));
                    if ui.add_sized([250.0, 29.0], btn_apply).clicked() {
                        do_apply_popup = true;
                    }

                    let btn_opt = egui::Button::new(
                        RichText::new("⚡ Reset Defaults")
                            .size(11.3)
                            .strong()
                            .color(Palette::TEXT_PRIMARY),
                    )
                    .fill(Palette::BG_INPUT)
                    .stroke(Stroke::new(1.0_f32, Palette::BORDER))
                    .rounding(Rounding::same(6.0));
                    if ui.add_sized([130.0, 29.0], btn_opt).clicked() {
                        reset_optimal = true;
                    }

                    let btn_close = egui::Button::new(
                        RichText::new("✕ Close")
                            .size(11.3)
                            .strong()
                            .color(Palette::TEXT_PRIMARY),
                    )
                    .fill(Palette::BG_INPUT)
                    .stroke(Stroke::new(1.0_f32, Palette::BORDER))
                    .rounding(Rounding::same(6.0));
                    if ui.add_sized([90.0, 29.0], btn_close).clicked() {
                        close_win = true;
                    }
                });
            });

        if reset_optimal {
            cfg = DspRsbAsioConfig::default();
        }
        cfg.sync_buffer_size_with_rsb = true;
        cfg.sync_sample_rate_with_rsb = true;
        cfg.manual_sample_rate = rsb_global_sr;
        cfg.buffer_size = cfg.effective_buffer_size(rsb_global_buf);

        let dev_changed = sel_in_a != prev_in_a || sel_out_b != prev_out_b;
        let hw_stream_changed = dev_changed
            || cfg.in_channels != prev_cfg.in_channels
            || cfg.out_channels != prev_cfg.out_channels
            || cfg.buffer_size != prev_cfg.buffer_size;
        let cfg_changed = cfg != prev_cfg;

        if dev_changed || cfg_changed || do_apply_popup || reset_optimal {
            set_asio_cpu_cores(cfg.asio_cpu_cores);
            cfg.micro_stretch_rescue = true;
            cfg.dsp_b_cushion_ms = cfg.dsp_b_cushion_ms.clamp(20.0, 450.0);
            cfg.adaptive_extra_ms = cfg.adaptive_extra_ms.clamp(0.0, 250.0);
            cfg.gpu_stall_multiplier = cfg.gpu_stall_multiplier.clamp(1.00, 1.30);

            self.dsp_rsb_config = cfg.clone();
            self.pipeline_a.dsp_rsb_config = cfg.clone();
            self.pipeline_b.dsp_rsb_config = cfg.clone();

            let eff_cush_ms = cfg.dsp_b_cushion_ms;
            let dsp_cush_smp = (((eff_cush_ms as f64) / 1000.0) * (rsb_global_sr.max(44100) as f64)).round() as u32;
            get_dsp_rsb_bus_b_in().set_adaptive_flags_with_mult(
                cfg.adaptive_cushion,
                cfg.adaptive_reported_latency,
                cfg.gpu_stall_multiplier,
            );
            get_dsp_rsb_bus_b_in().set_adaptive_mode(cfg.adaptive_mode as u8);
            get_dsp_rsb_bus_b_in().set_user_extra_buffer_ms(cfg.adaptive_extra_ms);
            get_dsp_rsb_bus_b_in().sync_config(
                rsb_global_sr.max(44100),
                cfg.buffer_size,
                1,
                dsp_cush_smp.min(24000),
            );

            if dev_changed {
                self.pipeline_a.input_device_name = if sel_in_a.is_empty() {
                    None
                } else {
                    Some(sel_in_a.clone())
                };
                self.pipeline_a.asio_profile.input_device = sel_in_a;

                self.pipeline_b.output_device_name = if sel_out_b.is_empty() {
                    None
                } else {
                    Some(sel_out_b.clone())
                };
                self.pipeline_b.asio_profile.output_device = sel_out_b;
            }

            if hw_stream_changed || reset_optimal {
                let _ = self.sync_unified_asio_config(false);
                self.pipeline_a.restart_stream();
                self.pipeline_b.restart_stream();
            } else if do_apply_popup {
                let _ = self.sync_unified_asio_config(true);
            } else {
                let _ = self.sync_unified_asio_config(false);
            }
            self.save_config_now();

            if do_apply_popup {
                let cpu_str = if cfg.asio_cpu_cores == 0 {
                    format!("Auto (All {} Logical CPUs)", get_logical_cpu_count())
                } else {
                    format!("{} Cores (Core 0 DPC-Isolated)", cfg.asio_cpu_cores)
                };
                let (active_cush_smp, _) = get_dsp_rsb_bus_b_in().get_dsp_b_params();
                let active_cush_ms = (active_cush_smp as f64 / (rsb_global_sr.max(44100) as f64)) * 1000.0;
                show_info_message_box(
                    "SoundBridge ASIO — Rinka SoundBridge ASIO",
                    &format!(
                        "SoundBridge ASIO settings saved & applied without audio interruption!\n\n\
                         • VC Driver in Vonovox : ASIO: {}\n\
                         • Buffer Size (smp) : {} smp (Auto-Synced with RSB @ {} Hz)\n\
                         • ASIO CPU Cores : {}\n\
                         • Adaptive Cushion : {} (Base {:.0} ms | Extra +{:.0} ms | Active {:.1} ms)\n\
                         • Line A Input : {}\n\
                         • Line B Output : {}",
                        DSP_RSB_ASIO_DRIVER_NAME,
                        cfg.buffer_size,
                        rsb_global_sr,
                        cpu_str,
                        if cfg.adaptive_cushion {
                            format!("ON (x{:.2} + 15 ms | Extra +{:.0} ms)", cfg.gpu_stall_multiplier, cfg.adaptive_extra_ms)
                        } else {
                            format!("Manual (+{:.0} ms Extra)", cfg.adaptive_extra_ms)
                        },
                        cfg.dsp_b_cushion_ms,
                        cfg.adaptive_extra_ms,
                        active_cush_ms,
                        self.pipeline_a.input_device_name.as_deref().unwrap_or("(None)"),
                        self.pipeline_b.output_device_name.as_deref().unwrap_or("(None)"),
                    ),
                );
            }
        }

        if do_scan {
            self.refresh_audio_devices();
        }
        if close_win {
            self.dsp_rsb_setting_open = false;
        }
    }

    /// Jendela **🎛️ RSB ASIO — Built-in FlexASIO GUI v0.35**:
    /// - Desain & fungsi identik 1:1 dengan **FlexASIO GUI v0.35** (`Backend: Windows WASAPI`,
    ///   `Set Buffer Size`, `Refresh Devices`, `Input Device`, `Output Device`, `Output Preview`,
    ///   `Latency: Set Input Latency [0.0] / Set Output Latency [0.0]`,
    ///   `WASAPI: Set / Exclusive / AutoConvert`, dan `Channels: 0`),
    ///   ditambah fitur **`Auto-Sync Buffer Size with RSB`**.
    /// - Tanpa buffer/cushion buatan dan tanpa Auto Report kustom; terdaftar sebagai `"RSB ASIO"`
    ///   agar tidak pernah konflik dengan FlexASIO eksternal.
    fn render_rsb_asio_setting_modal(&mut self, ctx: &egui::Context) {
        if !self.rsb_asio_setting_open {
            return;
        }

        let mut close_win = false;
        let mut do_apply_popup = false;
        let mut do_scan = false;
        let mut reset_defaults = false;

        let in_items: Vec<(String, String)> = self
            .wasapi_inputs
            .iter()
            .map(|d| (d.name.clone(), d.name.clone()))
            .collect();
        let out_items: Vec<(String, String)> = self
            .wasapi_outputs
            .iter()
            .map(|d| (d.name.clone(), d.name.clone()))
            .collect();

        let _rsb_global_sr = self.pipeline_a.sample_rate.max(44100);
        let rsb_global_buf = self.pipeline_a.buffer_size.clamp(32, 2048);
        let prev_cfg = self.rsb_asio_config.clone();
        let mut cfg = self.rsb_asio_config.clone();

        let mut sel_in_a = if !cfg.input_device.is_empty() {
            cfg.input_device.clone()
        } else {
            self.pipeline_a.input_device_name.clone().unwrap_or_default()
        };
        let mut sel_out_b = if !cfg.output_device.is_empty() {
            cfg.output_device.clone()
        } else {
            self.pipeline_b.output_device_name.clone().unwrap_or_default()
        };
        let prev_in_a = sel_in_a.clone();
        let prev_out_b = sel_out_b.clone();

        let screen_rect = ctx.screen_rect();
        let modal_w = (screen_rect.width() - 28.0).clamp(680.0, 880.0);
        let max_scroll_h = (screen_rect.height() - 84.0).clamp(560.0, 840.0);
        let default_pos = Pos2::new(
            (screen_rect.center().x - modal_w * 0.5).max(10.0),
            (screen_rect.center().y - (max_scroll_h + 44.0) * 0.5).max(10.0),
        );

        egui::Window::new("🎛️ RSB ASIO — Built-in FlexASIO GUI (\"ASIO: RSB ASIO\")")
            .collapsible(false)
            .resizable(true)
            .constrain(true)
            .default_pos(default_pos)
            .default_size([modal_w, max_scroll_h + 44.0])
            .show(ctx, |ui| {
                ui.set_width(modal_w);
                ui.set_max_width(modal_w);

                egui::ScrollArea::vertical()
                    .id_source("scroll_rsb_asio_flex_modal")
                    .max_height(max_scroll_h)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        let inner_w = (modal_w - 10.0).max(480.0);
                        ui.set_width(inner_w);
                        ui.set_max_width(inner_w);

                        // =========================================================================
                        // BARIS 1 (TOP BAR PERSIS FLEXASIO GUI v0.35 + AUTO-SYNC RSB):
                        // Backend: [Windows WASAPI] | [x] Auto-Sync | [x] Set Buffer Size [256] | [Refresh Devices]
                        // =========================================================================
                        egui::Frame::none()
                            .fill(Palette::BG_PANEL_ALT)
                            .stroke(Stroke::new(1.0_f32, Palette::BORDER_STRONG))
                            .rounding(Rounding::same(8.0))
                            .inner_margin(Margin::symmetric(10.0, 7.0))
                            .show(ui, |ui| {
                                ui.set_max_width(inner_w - 20.0);
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new("Backend:")
                                            .size(11.3)
                                            .strong()
                                            .color(Palette::TEXT_SECONDARY),
                                    );
                                    egui::ComboBox::from_id_source("cb_rsb_flex_backend")
                                        .width(135.0)
                                        .selected_text(
                                            RichText::new("Windows WASAPI")
                                                .size(11.3)
                                                .strong()
                                                .color(Palette::TEXT_PRIMARY),
                                        )
                                        .show_ui(ui, |ui| {
                                            let mut dummy = 0usize;
                                            ui.selectable_value(&mut dummy, 0, "Windows WASAPI");
                                        });

                                    ui.add_space(4.0);
                                    let sync_col = if cfg.sync_buffer_size_with_rsb {
                                        Palette::ACCENT_GREEN
                                    } else {
                                        Palette::TEXT_PRIMARY
                                    };
                                    ui.checkbox(
                                        &mut cfg.sync_buffer_size_with_rsb,
                                        RichText::new("🔗 Auto-Sync RSB")
                                            .size(11.2)
                                            .strong()
                                            .color(sync_col),
                                    )
                                    .on_hover_text(
                                        "Automatically synchronizes Buffer Size (samples) with Rinka SoundBridge's global Buffer Size.",
                                    );

                                    ui.add_enabled_ui(!cfg.sync_buffer_size_with_rsb, |ui| {
                                        ui.checkbox(
                                            &mut cfg.set_buffer_size_enabled,
                                            RichText::new("Set Buffer Size")
                                                .size(11.2)
                                                .color(Palette::TEXT_PRIMARY),
                                        );
                                        ui.add_enabled(
                                            cfg.set_buffer_size_enabled,
                                            egui::DragValue::new(&mut cfg.manual_buffer_size)
                                                .speed(16.0)
                                                .clamp_range(32..=4096),
                                        );
                                    });

                                    if cfg.sync_buffer_size_with_rsb {
                                        ui.label(
                                            RichText::new(format!("[{} smp]", rsb_global_buf))
                                                .size(11.2)
                                                .strong()
                                                .color(Palette::ACCENT_GREEN),
                                        );
                                    }

                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        let btn_ref = egui::Button::new(
                                            RichText::new("Refresh Devices")
                                                .size(11.0)
                                                .strong()
                                                .color(Palette::TEXT_PRIMARY),
                                        )
                                        .fill(Palette::BG_INPUT)
                                        .stroke(Stroke::new(1.0_f32, Palette::BORDER))
                                        .rounding(Rounding::same(5.0));
                                        if ui.add_sized([106.0, 22.0], btn_ref).clicked() {
                                            do_scan = true;
                                        }
                                    });
                                });
                            });

                        ui.add_space(6.0);

                        // =========================================================================
                        // BARIS 2 (3 KOLOM PERSIS FLEXASIO GUI v0.35):
                        // [ Input Device + Controls ] | [ Output Device + Controls ] | [ Output Preview ]
                        // =========================================================================
                        egui::Frame::none()
                            .fill(Palette::BG_PANEL_ALT)
                            .stroke(Stroke::new(1.0_f32, Palette::BORDER_STRONG))
                            .rounding(Rounding::same(8.0))
                            .inner_margin(Margin::symmetric(10.0, 8.0))
                            .show(ui, |ui| {
                                let card_w = (inner_w - 20.0).max(460.0);
                                ui.set_width(card_w);
                                ui.set_max_width(card_w);

                                let label_col_w = 56.0;
                                let io_col_w = ((card_w - label_col_w - 195.0) * 0.5).max(135.0);
                                let prev_col_w = (card_w - label_col_w - io_col_w * 2.0 - 18.0).max(165.0);

                                ui.horizontal(|ui| {
                                    // Kolom Kiri (Input & Output Controls)
                                    ui.vertical(|ui| {
                                        ui.set_width(label_col_w + io_col_w * 2.0 + 10.0);

                                        // Header: [kosong] | Input Device | Output Device
                                        ui.horizontal(|ui| {
                                            ui.add_sized([label_col_w, 18.0], egui::Label::new(""));
                                            ui.add_sized(
                                                [io_col_w, 18.0],
                                                egui::Label::new(
                                                    RichText::new("Input Device")
                                                        .size(11.5)
                                                        .strong()
                                                        .color(Palette::TEXT_PRIMARY),
                                                ),
                                            );
                                            ui.add_space(6.0);
                                            ui.add_sized(
                                                [io_col_w, 18.0],
                                                egui::Label::new(
                                                    RichText::new("Output Device")
                                                        .size(11.5)
                                                        .strong()
                                                        .color(Palette::TEXT_PRIMARY),
                                                ),
                                            );
                                        });

                                        ui.add_space(3.0);

                                        // Row 1: Dropdown Input Device & Output Device
                                        ui.horizontal(|ui| {
                                            ui.add_sized([label_col_w, 24.0], egui::Label::new(""));
                                            ui.allocate_ui(Vec2::new(io_col_w, 24.0), |ui| {
                                                draw_v122_device_dropdown(
                                                    ui,
                                                    "dd_rsb_flex_gui_in",
                                                    &mut sel_in_a,
                                                    "(None)",
                                                    &in_items,
                                                    io_col_w,
                                                );
                                            });
                                            ui.add_space(6.0);
                                            ui.allocate_ui(Vec2::new(io_col_w, 24.0), |ui| {
                                                draw_v122_device_dropdown(
                                                    ui,
                                                    "dd_rsb_flex_gui_out",
                                                    &mut sel_out_b,
                                                    "(None)",
                                                    &out_items,
                                                    io_col_w,
                                                );
                                            });
                                        });

                                        ui.add_space(8.0);

                                        // Row 2: Latency | [x] Set Input Latency [0.0] | [x] Set Output Latency [0.0]
                                        ui.horizontal(|ui| {
                                            ui.add_sized(
                                                [label_col_w, 22.0],
                                                egui::Label::new(
                                                    RichText::new("Latency")
                                                        .size(11.2)
                                                        .color(Palette::TEXT_SECONDARY),
                                                ),
                                            );
                                            ui.allocate_ui(Vec2::new(io_col_w, 22.0), |ui| {
                                                ui.horizontal(|ui| {
                                                    ui.checkbox(
                                                        &mut cfg.set_in_latency_enabled,
                                                        RichText::new("Set Input Latency")
                                                            .size(11.0)
                                                            .color(Palette::TEXT_PRIMARY),
                                                    );
                                                    ui.add_enabled(
                                                        cfg.set_in_latency_enabled,
                                                        egui::DragValue::new(&mut cfg.in_latency_val)
                                                            .speed(0.01)
                                                            .clamp_range(0.0..=0.45)
                                                            .fixed_decimals(2),
                                                    );
                                                });
                                            });
                                            ui.add_space(6.0);
                                            ui.allocate_ui(Vec2::new(io_col_w, 22.0), |ui| {
                                                ui.horizontal(|ui| {
                                                    ui.checkbox(
                                                        &mut cfg.set_out_latency_enabled,
                                                        RichText::new("Set Output Latency")
                                                            .size(11.0)
                                                            .color(Palette::TEXT_PRIMARY),
                                                    );
                                                    ui.add_enabled(
                                                        cfg.set_out_latency_enabled,
                                                        egui::DragValue::new(&mut cfg.out_latency_val)
                                                            .speed(0.01)
                                                            .clamp_range(0.0..=0.45)
                                                            .fixed_decimals(2),
                                                    );
                                                });
                                            });
                                        });

                                        ui.add_space(8.0);

                                        // Row 3: WASAPI | [x] Set: [ ] Exclusive [x] AutoConvert | [x] Set: [ ] Exclusive [x] AutoConvert
                                        ui.horizontal(|ui| {
                                            ui.add_sized(
                                                [label_col_w, 38.0],
                                                egui::Label::new(
                                                    RichText::new("WASAPI")
                                                        .size(11.2)
                                                        .color(Palette::TEXT_SECONDARY),
                                                ),
                                            );
                                            ui.allocate_ui(Vec2::new(io_col_w, 38.0), |ui| {
                                                ui.horizontal(|ui| {
                                                    ui.checkbox(
                                                        &mut cfg.wasapi_in_set,
                                                        RichText::new("Set:").size(11.0),
                                                    );
                                                    ui.add_enabled_ui(cfg.wasapi_in_set, |ui| {
                                                        ui.vertical(|ui| {
                                                            ui.checkbox(
                                                                &mut cfg.wasapi_in_exclusive,
                                                                RichText::new("Exclusive").size(10.8),
                                                            );
                                                            ui.checkbox(
                                                                &mut cfg.wasapi_in_autoconvert,
                                                                RichText::new("AutoConvert").size(10.8),
                                                            );
                                                        });
                                                    });
                                                });
                                            });
                                            ui.add_space(6.0);
                                            ui.allocate_ui(Vec2::new(io_col_w, 38.0), |ui| {
                                                ui.horizontal(|ui| {
                                                    ui.checkbox(
                                                        &mut cfg.wasapi_out_set,
                                                        RichText::new("Set:").size(11.0),
                                                    );
                                                    ui.add_enabled_ui(cfg.wasapi_out_set, |ui| {
                                                        ui.vertical(|ui| {
                                                            ui.checkbox(
                                                                &mut cfg.wasapi_out_exclusive,
                                                                RichText::new("Exclusive").size(10.8),
                                                            );
                                                            ui.checkbox(
                                                                &mut cfg.wasapi_out_autoconvert,
                                                                RichText::new("AutoConvert").size(10.8),
                                                            );
                                                        });
                                                    });
                                                });
                                            });
                                        });

                                        ui.add_space(8.0);

                                        // Row 4: Channels | [ 0 ] | [ 0 ]
                                        ui.horizontal(|ui| {
                                            ui.add_sized(
                                                [label_col_w, 22.0],
                                                egui::Label::new(
                                                    RichText::new("Channels")
                                                        .size(11.2)
                                                        .color(Palette::TEXT_SECONDARY),
                                                ),
                                            );
                                            ui.allocate_ui(Vec2::new(io_col_w, 22.0), |ui| {
                                                ui.horizontal(|ui| {
                                                    ui.add_sized(
                                                        [56.0, 20.0],
                                                        egui::DragValue::new(&mut cfg.in_channels)
                                                            .speed(1.0)
                                                            .clamp_range(0..=16),
                                                    );
                                                    ui.label(
                                                        RichText::new("(0 = Auto)")
                                                            .size(10.3)
                                                            .color(Palette::TEXT_MUTED),
                                                    );
                                                });
                                            });
                                            ui.add_space(6.0);
                                            ui.allocate_ui(Vec2::new(io_col_w, 22.0), |ui| {
                                                ui.horizontal(|ui| {
                                                    ui.add_sized(
                                                        [56.0, 20.0],
                                                        egui::DragValue::new(&mut cfg.out_channels)
                                                            .speed(1.0)
                                                            .clamp_range(0..=16),
                                                    );
                                                    ui.label(
                                                        RichText::new("(0 = Auto)")
                                                            .size(10.3)
                                                            .color(Palette::TEXT_MUTED),
                                                    );
                                                });
                                            });
                                        });
                                    });

                                    ui.add_space(6.0);

                                    // Kolom Kanan: Output Preview (Live FlexASIO TOML Preview)
                                    ui.vertical(|ui| {
                                        ui.set_width(prev_col_w);
                                        ui.label(
                                            RichText::new("Output Preview")
                                                .size(11.5)
                                                .strong()
                                                .color(Palette::TEXT_PRIMARY),
                                        );
                                        ui.add_space(3.0);
                                        let preview_toml = cfg.render_toml_preview(
                                            rsb_global_buf,
                                            &sel_in_a,
                                            &sel_out_b,
                                        );
                                        egui::Frame::none()
                                            .fill(Palette::BG_INPUT)
                                            .stroke(Stroke::new(1.0_f32, Palette::BORDER))
                                            .rounding(Rounding::same(5.0))
                                            .inner_margin(Margin::symmetric(8.0, 6.0))
                                            .show(ui, |ui| {
                                                ui.set_min_size(Vec2::new(prev_col_w - 16.0, 142.0));
                                                ui.label(
                                                    RichText::new(preview_toml)
                                                        .font(FontId::new(10.2, FontFamily::Monospace))
                                                        .color(Palette::ACCENT_BLUE),
                                                );
                                            });
                                    });
                                });
                            });

                        ui.add_space(6.0);
                        self.draw_gpu_boost_lock_and_notes_section(ui, "rsb_asio");
                    });

                ui.add_space(6.0);

                // =========================================================================
                // FOOTER ACTION BAR
                // =========================================================================
                ui.horizontal(|ui| {
                    let btn_apply = egui::Button::new(
                        RichText::new("💾 Save to RSB ASIO (\"ASIO: RSB ASIO\")")
                            .size(11.8)
                            .strong()
                            .color(Color32::WHITE),
                    )
                    .fill(Palette::ACCENT_BLUE_DEEP)
                    .stroke(Stroke::new(1.0_f32, Palette::ACCENT_PINK))
                    .rounding(Rounding::same(6.0));
                    if ui.add_sized([260.0, 29.0], btn_apply).clicked() {
                        do_apply_popup = true;
                    }

                    let btn_rst = egui::Button::new(
                        RichText::new("⚡ Reset Defaults")
                            .size(11.3)
                            .strong()
                            .color(Palette::TEXT_PRIMARY),
                    )
                    .fill(Palette::BG_INPUT)
                    .stroke(Stroke::new(1.0_f32, Palette::BORDER))
                    .rounding(Rounding::same(6.0));
                    if ui.add_sized([130.0, 29.0], btn_rst).clicked() {
                        reset_defaults = true;
                    }

                    let btn_close = egui::Button::new(
                        RichText::new("✕ Close")
                            .size(11.3)
                            .strong()
                            .color(Palette::TEXT_PRIMARY),
                    )
                    .fill(Palette::BG_INPUT)
                    .stroke(Stroke::new(1.0_f32, Palette::BORDER))
                    .rounding(Rounding::same(6.0));
                    if ui.add_sized([90.0, 29.0], btn_close).clicked() {
                        close_win = true;
                    }
                });
            });

        if reset_defaults {
            cfg = RsbAsioConfig::default();
        }
        cfg.input_device = sel_in_a.clone();
        cfg.output_device = sel_out_b.clone();
        cfg.buffer_size = cfg.effective_buffer_size(rsb_global_buf);

        let dev_changed = sel_in_a != prev_in_a || sel_out_b != prev_out_b;
        let cfg_changed = cfg != prev_cfg;

        if dev_changed || cfg_changed || do_apply_popup || reset_defaults {
            self.rsb_asio_config = cfg.clone();
            self.pipeline_a.rsb_asio_config = cfg.clone();
            self.pipeline_b.rsb_asio_config = cfg.clone();

            if dev_changed && self.pipeline_a.driver_backend == DriverBackend::RsbAsio {
                self.pipeline_a.input_device_name = if sel_in_a.is_empty() {
                    None
                } else {
                    Some(sel_in_a.clone())
                };
                self.pipeline_a.asio_profile.input_device = sel_in_a.clone();

                self.pipeline_b.output_device_name = if sel_out_b.is_empty() {
                    None
                } else {
                    Some(sel_out_b.clone())
                };
                self.pipeline_b.asio_profile.output_device = sel_out_b.clone();
            }

            let _ = self.sync_unified_asio_config(do_apply_popup);
            if self.pipeline_a.driver_backend == DriverBackend::RsbAsio {
                self.pipeline_a.restart_stream();
                self.pipeline_b.restart_stream();
            }
            self.save_config_now();

            if do_apply_popup {
                show_info_message_box(
                    "RSB ASIO — Built-in FlexASIO GUI",
                    &format!(
                        "RSB ASIO (Built-in FlexASIO GUI) configuration saved!

                         • Driver Name in Vonovox : ASIO: {}
                         • Backend : Windows WASAPI
                         • Buffer Size : {} samples ({})
                         • Input Device : {} (Latency: {})
                         • Output Device : {} (Latency: {})",
                        RSB_ASIO_DRIVER_NAME,
                        cfg.buffer_size,
                        if cfg.sync_buffer_size_with_rsb { "Auto-Synced with RSB" } else { "Manual" },
                        if sel_in_a.is_empty() { "(None)" } else { &sel_in_a },
                        if cfg.set_in_latency_enabled { format!("{:.1} s", cfg.in_latency_val) } else { "Auto".into() },
                        if sel_out_b.is_empty() { "(None)" } else { &sel_out_b },
                        if cfg.set_out_latency_enabled { format!("{:.1} s", cfg.out_latency_val) } else { "Auto".into() },
                    ),
                );
            }
        }

        if do_scan {
            self.refresh_audio_devices();
        }
        if close_win {
            self.rsb_asio_setting_open = false;
        }
    }
}

impl eframe::App for RinkaSoundBridgeApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        enable_cpu_denormal_flush();
        restore_host_gl_context();
        self.periodic_background_tasks(ctx);
        restore_host_gl_context();
        ctx.request_repaint_after(Duration::from_millis(33));

        // Panel Bawah: Indikator Delay Voice Changer & Live2D (L2D) dengan fitur Toggle Hide/Show
        // Dirender menggunakan TopBottomPanel::bottom agar ruangnya selalu dipesan lebih dulu dan TIDAK PERNAH terpotong!
        egui::TopBottomPanel::bottom("footer_delay_panel")
            .frame(
                egui::Frame::none()
                    .fill(Palette::BG_APP)
                    .inner_margin(Margin {
                        left: 14.0,
                        right: 14.0,
                        top: 2.0,
                        bottom: 8.0,
                    }),
            )
            .show_separator_line(false)
            .show(ctx, |ui| {
                let sep_rect = ui.available_rect_before_wrap();
                ui.painter().hline(
                    sep_rect.x_range(),
                    sep_rect.min.y,
                    Stroke::new(1.0_f32, Palette::BORDER),
                );
                ui.add_space(5.0);

                if self.show_delay_bar {
                    let est_a = self.pipeline_a.estimated_line_latency_ms();
                    let est_b = self.pipeline_b.estimated_line_latency_ms();
                    let is_sb_asio = self.pipeline_a.driver_backend == DriverBackend::AsioDspRsb;
                    let is_rsb_asio = self.pipeline_a.driver_backend == DriverBackend::RsbAsio;
                    let bridge_cush_ms = if is_rsb_asio {
                        None
                    } else if is_sb_asio {
                        let sr_f = self.pipeline_b.effective_sample_rate().max(44100) as f64;
                        let (c_smp, _) = get_dsp_rsb_bus_b_in().get_dsp_b_params();
                        Some((c_smp as f64 / sr_f) * 1000.0)
                    } else {
                        None
                    };

                    // Nilai utama In A -> Out B: prioritaskan hasil korelasi suara aktual; jika belum bicara,
                    // tampilkan estimasi gabungan (Line A + Line B + Cushion) untuk acuan Live2D!
                    let (main_lat_str, mode_badge) = if let Some(ms) = self.lat_peak_ms {
                        (format!("{:.1} ms", ms), "Measured")
                    } else if let (Some(a), Some(b)) = (est_a, est_b) {
                        let c = bridge_cush_ms.unwrap_or(0.0);
                        (format!("~{:.1} ms", a + b + c), "Estimated")
                    } else if let Some(a) = est_a {
                        (format!("~{:.1} ms", a), "Line A")
                    } else if let Some(b) = est_b {
                        (format!("~{:.1} ms", b), "Line B")
                    } else {
                        ("-- ms".to_string(), "Idle")
                    };

                    let detail_str = if let Some(c_ms) = bridge_cush_ms {
                        format!(
                            "Line A: {} | Line B: {} | Cushion: {:.1} ms",
                            est_a.map(|v| format!("{:.1} ms", v)).unwrap_or_else(|| "-- ms".into()),
                            est_b.map(|v| format!("{:.1} ms", v)).unwrap_or_else(|| "-- ms".into()),
                            c_ms,
                        )
                    } else {
                        format!(
                            "Line A: {} | Line B: {}",
                            est_a.map(|v| format!("{:.1} ms", v)).unwrap_or_else(|| "-- ms".into()),
                            est_b.map(|v| format!("{:.1} ms", v)).unwrap_or_else(|| "-- ms".into()),
                        )
                    };

                    let l2d_tip = format!(
                        "Live2D / VTube Studio / OBS Delay Calibration Guide:
                        - Total Delay (In A -> Out B): {} ({})
                        - Line A Pipeline Estimate (Buffer + FIFO + VST): {}
                        - Line B Pipeline Estimate (Buffer + FIFO + VST): {}

                        Tip: Enter this ms value into your Audio/Lip-Sync Delay setting in Live2D / VTube Studio
                        so your character's mouth movement stays in sync with the processed voice.",
                        main_lat_str,
                        mode_badge,
                        est_a.map(|v| format!("{:.1} ms", v)).unwrap_or_else(|| "Off".into()),
                        est_b.map(|v| format!("{:.1} ms", v)).unwrap_or_else(|| "Off".into()),
                    );

                    ui.horizontal(|ui| {
                        // Tombol Toggle Hide di kiri
                        let btn_hide = egui::Button::new(
                            RichText::new("▼ Hide Delay Bar")
                                .size(11.0)
                                .color(Palette::TEXT_SECONDARY),
                        )
                        .fill(Palette::BG_PANEL_ALT)
                        .stroke(Stroke::new(1.0_f32, Palette::BORDER))
                        .rounding(Rounding::same(6.0));
                        if ui
                            .add_sized([132.0, 26.0], btn_hide)
                            .on_hover_text("Hide the delay indicator bar to expand the VST Chain view")
                            .clicked()
                        {
                            self.show_delay_bar = false;
                            self.trigger_auto_save();
                        }

                        // Pill utama di tengah
                        let avail_mid = (ui.available_width() - 78.0).max(200.0);
                        ui.allocate_ui_with_layout(
                            Vec2::new(avail_mid, 28.0),
                            Layout::centered_and_justified(egui::Direction::LeftToRight),
                            |ui| {
                                egui::Frame::none()
                                    .fill(Palette::BG_PANEL_ALT)
                                    .stroke(Stroke::new(1.0_f32, Palette::BORDER))
                                    .rounding(Rounding::same(8.0))
                                    .inner_margin(Margin::symmetric(14.0, 4.0))
                                    .show(ui, |ui| {
                                        ui.horizontal(|ui| {
                                            let (ic_rect, _) = ui.allocate_exact_size(
                                                Vec2::new(14.0, 16.0),
                                                Sense::hover(),
                                            );
                                            paint_group_icon(
                                                &ui.painter(),
                                                ic_rect.center(),
                                                GroupIcon::Mic,
                                            );
                                            ui.label(
                                                RichText::new(
                                                    "Total Voice Changer Delay (In A -> Out B):",
                                                )
                                                .size(12.5)
                                                .strong()
                                                .color(Palette::TEXT_SECONDARY),
                                            )
                                            .on_hover_text(&l2d_tip);

                                            ui.label(
                                                RichText::new(&main_lat_str)
                                                    .size(15.0)
                                                    .strong()
                                                    .color(Palette::ACCENT_PINK),
                                            )
                                            .on_hover_text(&l2d_tip);

                                            ui.add_space(6.0);
                                            ui.label(
                                                RichText::new(format!(
                                                    "[{} | {}]",
                                                    mode_badge, detail_str
                                                ))
                                                .size(11.0)
                                                .color(Palette::TEXT_MUTED),
                                            )
                                            .on_hover_text(&l2d_tip);
                                        });
                                    });
                            },
                        );

                        // Tombol Reset Pengukuran Delay di kanan
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let btn_rst = egui::Button::new(
                                RichText::new("Reset ms")
                                    .size(11.0)
                                    .color(Palette::TEXT_SECONDARY),
                            )
                            .fill(Palette::BG_PANEL_ALT)
                            .stroke(Stroke::new(1.0_f32, Palette::BORDER))
                            .rounding(Rounding::same(6.0));
                            if ui
                                .add_sized([68.0, 26.0], btn_rst)
                                .on_hover_text("Reset measured peak delay (ms) & resync Cushion to Vonovox")
                                .clicked()
                            {
                                self.lat_peak_ms = None;
                                self.lat_current_ms = None;
                                get_dsp_rsb_bus_b_in().reset_spike_and_resync_cushion();
                                rinka_soundbridge_core::asio_host::inject_probe_into_running_vonovox_if_needed();
                            }
                        });
                    });
                } else {
                    // Saat disembunyikan: hanya tampilkan baris tipis dengan tombol Show
                    ui.horizontal(|ui| {
                        let btn_show = egui::Button::new(
                            RichText::new("▲ Show Voice Changer / Live2D Delay Indicator (ms)")
                                .size(11.0)
                                .strong()
                                .color(Palette::ACCENT_PINK),
                        )
                        .fill(Palette::BG_PANEL_ALT)
                        .stroke(Stroke::new(1.0_f32, Palette::BORDER))
                        .rounding(Rounding::same(6.0));
                        if ui.add_sized([ui.available_width(), 20.0], btn_show).clicked() {
                            self.show_delay_bar = true;
                            self.trigger_auto_save();
                        }
                    });
                }
            });

        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(Palette::BG_APP)
                    .inner_margin(Margin {
                        left: 14.0,
                        right: 14.0,
                        top: 12.0,
                        bottom: 4.0,
                    }),
            )
            .show(ctx, |ui| {
                self.render_header(ui, ctx);
                ui.add_space(8.0);

                ui.columns(2, |cols| {
                    self.render_line_panel(&mut cols[0], ActiveLineTarget::LineA);
                    self.render_line_panel(&mut cols[1], ActiveLineTarget::LineB);
                });
            });

        self.render_vst_picker_modal(ctx);
        self.render_triple_asio_setting_modal(ctx);
        self.render_dsp_rsb_setting_modal(ctx);
        self.render_rsb_asio_setting_modal(ctx);
        restore_host_gl_context();
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.clean_shutdown();
        if is_tray_quit_requested() {
            std::process::exit(0);
        }
    }
}

fn main() -> eframe::Result<()> {
    install_crash_handlers();

    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--register-asio") {
        ensure_asio_requirements_installed(false);
        let _ = register_virtual_asio_drivers(true);
        return Ok(());
    }
    let is_restarted = args.iter().any(|a| a == "--restarted");
    if !acquire_single_instance(if is_restarted { 10.0 } else { 0.0 }) {
        show_info_message_box(
            "Rinka SoundBridge",
            "Rinka SoundBridge is already running.\n\nPlease check the system tray or taskbar icon. Running two instances simultaneously will double-process audio.",
        );
        return Ok(());
    }

    let icon_data = egui::IconData {
        rgba: APP_ICON_RGBA.to_vec(),
        width: 32,
        height: 32,
    };

    let saved_cfg = AppConfig::load();
    let init_w = saved_cfg
        .as_ref()
        .and_then(|c| c.global.window_w)
        .unwrap_or(1020)
        .max(700) as f32;
    let init_h = saved_cfg
        .as_ref()
        .and_then(|c| c.global.window_h)
        .unwrap_or(940)
        .max(580) as f32;

    let mut viewport = egui::ViewportBuilder::default()
        .with_title(APP_WINDOW_TITLE)
        .with_inner_size([init_w, init_h])
        .with_min_inner_size([700.0, 580.0])
        .with_icon(Arc::new(icon_data));

    if let Some(ref c) = saved_cfg {
        if let (Some(x), Some(y)) = (c.global.window_x, c.global.window_y) {
            if x > -10000 && y > -10000 {
                viewport = viewport.with_position(Pos2::new(x as f32, y as f32));
            }
        }
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        APP_WINDOW_TITLE,
        options,
        Box::new(|cc| Box::new(RinkaSoundBridgeApp::new(cc))),
    )
}
