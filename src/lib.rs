//! Rinka SoundBridge v2.0 - Full Native Rust Core Engine
//! Mencakup:
//! - `win32`: Interop Win32 API (MMCSS Pro Audio, EcoQoS Power Throttling Disabler, FTZ/DAZ Denormal Flush,
//!   Single-Instance Mutex, System Tray, Registry & COM helpers)
//! - `logger`: Asynchronous daily rotating logger (14-day auto-cleanup) + native panic/crash handler
//! - `fifo`: Adaptive cubic-resampling FIFO (drift compensation, elastic stretch, smooth underrun tail) + Classic FIFO
//! - `dsp`: Zero-latency 20Hz DC Blocker, ADAA Anti-Aliasing Soft Limiter (`tanh`), AudioOverlayPlayer (Symphonia + 80ms loop crossfade), FFT LatencyProbe
//! - `asio_host`: Native Windows ASIO Driver Host (Registry `HKLM\SOFTWARE\ASIO` scanner, COM `IASIO` engine, & Control Panel launcher)
//! - `wasapi_exclusive`: Native Windows WASAPI Exclusive Mode COM Engine (`IMMDeviceEnumerator` + `IAudioClient` Event-Driven)
//! - `vst3_host`: Native Steinberg VST3 COM Host (`IComponent`, `IAudioProcessor`, `IEditController`, `IPlugView` HWND GUI Editor)
//! - `pipeline`: Dual-line multi-driver audio pipeline (WASAPI Shared, WASAPI Exclusive, ASIO Native, Zero-Allocation Scratch Buffers, Fail-Closed Mode Aman)
//! - `config`: Kompatibilitas penuh dengan `rinka_soundbridge_config.json` (mendukung migrasi mulus dari v1.22/v1.3 ke v2.0)

pub mod win32;
pub mod logger;
pub mod fifo;
pub mod dsp;
pub mod asio_host;
pub mod wasapi_exclusive;
pub mod vst3_host;
pub mod pipeline;
pub mod config;
