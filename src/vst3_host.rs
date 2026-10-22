//! Native Steinberg VST3 COM Host Lengkap (`src/vst3_host.rs`)
//! Mengimplementasikan spesifikasi penuh Steinberg VST3 Host ABI untuk kompatibilitas
//! maksimal dengan plugin komersial modern (FabFilter Pro-Q 3, Supertone Clear, Waves,
//! iZotope, JUCE, dan nih-plug / ONNX):
//! - Dual UID Matcher (`uid_win_com` & `uid_be`) + fallback `FUnknown::queryInterface`
//! - `IBStream` In-Memory Stream (`IComponent::getState` -> `IEditController::setComponentState`)
//! - `IConnectionPoint` Bidirectional Connection (`IComponent` <-> `IEditController`)
//! - `IHostApplication` dengan dukungan penuh instansiasi `IMessage` & `IAttributeList`
//!   (wajib untuk grafik spectrum analyzer real-time FabFilter Pro-Q 3 & JUCE)
//! - `IComponentHandler` + `IParameterChanges` & `IParamValueQueue` real-time queue
//!   (meneruskan perubahan knob/EQ band dari GUI langsung ke `IAudioProcessor::process`)
//! - `IPlugFrame` (`resizeView`) untuk jendela GUI VST3 yang dapat diubah ukurannya
//! - Multi-Bus (Main + Sidechain) & Mono/Stereo adaptif + Non-Blocking Win32 HWND Editor
//!   (audio & grafik spektrum tetap berjalan lancar saat jendela editor VST terbuka)

#![allow(non_snake_case, non_camel_case_types, non_upper_case_globals, dead_code)]

use crate::logger::{log_info, reassert_crash_handlers, record_breadcrumb};
use crate::win32::{restore_host_gl_context, trim_process_working_set, OpenGlContextGuard};
use libloading::Library;
use parking_lot::Mutex;
use rand::seq::SliceRandom;
use std::collections::{BTreeMap, HashMap};
use std::ffi::{c_char, c_void, CStr};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Cache modul `.vst3` (`Arc<Library>`) selama proses berjalan.
/// Mencegah DLL `.vst3` di-unload (`FreeLibrary`) di tengah sesi ketika masih ada vtable/timer/OpenGL
/// atau instance kedua di Line lain, serta memastikan `InitDll` hanya dipanggil satu kali per DLL!
static LOADED_VST3_MODULES: OnceLock<Mutex<HashMap<PathBuf, Arc<Library>>>> = OnceLock::new();

/// Daftar editor GUI VST yang sudah ditutup/dihapus namun menggunakan konteks OpenGL/child HWND
/// yang diparkir dalam keadaan tersembunyi (`SW_HIDE`) hingga aplikasi keluar agar tidak memicu
/// crash destructor OpenGL (`baseview` / `nih-plug` / `JUCE`) di tengah sesi.
static RETIRED_VST_EDITORS: OnceLock<Mutex<Vec<(usize, usize, usize)>>> = OnceLock::new();

fn get_or_load_vst3_library(bin_path: &Path) -> Result<Arc<Library>, String> {
    let cache = LOADED_VST3_MODULES.get_or_init(|| Mutex::new(HashMap::new()));
    let key = std::fs::canonicalize(bin_path).unwrap_or_else(|_| bin_path.to_path_buf());
    let mut guard = cache.lock();
    if let Some(existing) = guard.get(&key) {
        return Ok(Arc::clone(existing));
    }
    unsafe {
        let lib = Arc::new(
            Library::new(bin_path)
                .map_err(|e| format!("Failed to load VST3 DLL ({}): {}", bin_path.display(), e))?,
        );
        if let Ok(init_fn) = lib.get::<unsafe extern "system" fn() -> bool>(b"InitDll\0") {
            let _ = init_fn();
        }
        reassert_crash_handlers();
        guard.insert(key, Arc::clone(&lib));
        Ok(lib)
    }
}

pub const DIM_VST_COLORS: [&str; 10] = [
    "#22333B", // Slate Blue Redup
    "#2E2433", // Plum / Ungu Redup
    "#1E3329", // Forest / Hijau Redup
    "#362922", // Amber / Cokelat Redup
    "#1F3136", // Deep Teal Redup
    "#342227", // Wine / Marun Redup
    "#2D3121", // Olive Redup
    "#262338", // Indigo Redup
    "#332532", // Mauve Redup
    "#223324", // Moss Redup
];

// ==============================================================================
// 1. VST3 SCANNER
// ==============================================================================

pub struct VSTScanner;

impl VSTScanner {
    pub fn get_vst_directories() -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        let common_pf = std::env::var("CommonProgramFiles")
            .unwrap_or_else(|_| "C:\\Program Files\\Common Files".to_string());
        dirs.push(PathBuf::from(&common_pf).join("VST3"));

        let pf = std::env::var("ProgramFiles")
            .unwrap_or_else(|_| "C:\\Program Files".to_string());
        dirs.push(PathBuf::from(&pf).join("VSTPlugins"));
        dirs.push(PathBuf::from(&pf).join("Steinberg").join("VSTPlugins"));

        dirs.into_iter().filter(|d| d.exists()).collect()
    }

    pub fn scan_plugins() -> BTreeMap<String, String> {
        let mut vst_map = BTreeMap::new();
        for base_dir in Self::get_vst_directories() {
            Self::walk_dir(&base_dir, &mut vst_map);
        }
        vst_map
    }

    fn walk_dir(dir: &Path, vst_map: &mut BTreeMap<String, String>) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_vst3 = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|s| s.eq_ignore_ascii_case("vst3"))
                .unwrap_or(false);

            if is_vst3 {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    vst_map.insert(stem.to_string(), path.to_string_lossy().to_string());
                }
            } else if path.is_dir() {
                Self::walk_dir(&path, vst_map);
            }
        }
    }
}

pub fn resolve_vst3_binary(path: &Path) -> Result<PathBuf, String> {
    if path.is_file() {
        return Ok(path.to_path_buf());
    }
    if path.is_dir() {
        let win64_dir = path.join("Contents").join("x86_64-win");
        if win64_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&win64_dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_file() {
                        if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                            if ext.eq_ignore_ascii_case("vst3") || ext.eq_ignore_ascii_case("dll") {
                                return Ok(p);
                            }
                        }
                    }
                }
            }
        }
    }
    Err(format!("VST3 x86_64 binary not found in: {}", path.display()))
}

// ==============================================================================
// 2. STEINBERG VST3 COM ABI & DUAL-UID DEFINITIONS
// ==============================================================================

pub type TResult = i32;
pub const K_RESULT_OK: TResult = 0;
pub const K_RESULT_FALSE: TResult = 1;
pub const K_NO_INTERFACE: TResult = -1;
pub const K_INVALID_ARGUMENT: TResult = -2;
pub const K_NOT_IMPLEMENTED: TResult = -4;

pub type TUID = [u8; 16];

/// Format GUID COM Windows (COM_COMPATIBLE = 1).
pub const fn uid_win_com(l1: u32, l2: u32, l3: u32, l4: u32) -> TUID {
    [
        (l1 & 0xFF) as u8,
        ((l1 >> 8) & 0xFF) as u8,
        ((l1 >> 16) & 0xFF) as u8,
        ((l1 >> 24) & 0xFF) as u8,
        ((l2 >> 16) & 0xFF) as u8,
        ((l2 >> 24) & 0xFF) as u8,
        (l2 & 0xFF) as u8,
        ((l2 >> 8) & 0xFF) as u8,
        ((l3 >> 24) & 0xFF) as u8,
        ((l3 >> 16) & 0xFF) as u8,
        ((l3 >> 8) & 0xFF) as u8,
        (l3 & 0xFF) as u8,
        ((l4 >> 24) & 0xFF) as u8,
        ((l4 >> 16) & 0xFF) as u8,
        ((l4 >> 8) & 0xFF) as u8,
        (l4 & 0xFF) as u8,
    ]
}

/// Format TUID Big-Endian murni (digunakan oleh beberapa plugin non-COM).
pub const fn uid_be(l1: u32, l2: u32, l3: u32, l4: u32) -> TUID {
    [
        ((l1 >> 24) & 0xFF) as u8,
        ((l1 >> 16) & 0xFF) as u8,
        ((l1 >> 8) & 0xFF) as u8,
        (l1 & 0xFF) as u8,
        ((l2 >> 24) & 0xFF) as u8,
        ((l2 >> 16) & 0xFF) as u8,
        ((l2 >> 8) & 0xFF) as u8,
        (l2 & 0xFF) as u8,
        ((l3 >> 24) & 0xFF) as u8,
        ((l3 >> 16) & 0xFF) as u8,
        ((l3 >> 8) & 0xFF) as u8,
        (l3 & 0xFF) as u8,
        ((l4 >> 24) & 0xFF) as u8,
        ((l4 >> 16) & 0xFF) as u8,
        ((l4 >> 8) & 0xFF) as u8,
        (l4 & 0xFF) as u8,
    ]
}

#[derive(Clone, Copy)]
pub struct DualIid {
    pub com: TUID,
    pub be: TUID,
}

impl DualIid {
    pub const fn new(l1: u32, l2: u32, l3: u32, l4: u32) -> Self {
        Self {
            com: uid_win_com(l1, l2, l3, l4),
            be: uid_be(l1, l2, l3, l4),
        }
    }
    #[inline]
    pub fn matches(&self, other: &TUID) -> bool {
        *other == self.com || *other == self.be
    }
}

pub const IID_FUNKNOWN: DualIid = DualIid::new(0x00000000, 0x00000000, 0xC0000000, 0x00000046);
pub const IID_IPLUGINBASE: DualIid = DualIid::new(0x22888DDB, 0x156E45AE, 0x8358B348, 0x08190625);
pub const IID_ICOMPONENT: DualIid = DualIid::new(0xE831FF31, 0xF2D54301, 0x928EBBEE, 0x25697802);
pub const IID_IAUDIOPROCESSOR: DualIid = DualIid::new(0x42043F99, 0xB7DA453C, 0xA569E79D, 0x9AAEC33D);
pub const IID_IEDITCONTROLLER: DualIid = DualIid::new(0xDCD7BBE3, 0x7742448D, 0xA874AACC, 0x979C759E);
pub const IID_ICONNECTIONPOINT: DualIid = DualIid::new(0x70A4156F, 0x6E6E4026, 0x989148BF, 0xAA60D8D1);
pub const IID_IHOSTAPPLICATION: DualIid = DualIid::new(0x58E595CC, 0xDB2D4969, 0x8B6AAF8C, 0x36A664E5);
pub const IID_ICOMPONENTHANDLER: DualIid = DualIid::new(0x93A0BEA3, 0x0BD045DB, 0x8E890B0C, 0xC1E46AC6);
pub const IID_ICOMPONENTHANDLER2: DualIid = DualIid::new(0xF040B4B3, 0xA36045EC, 0xABCDC045, 0xB4D5A2CC);
pub const IID_IPLUGFRAME: DualIid = DualIid::new(0x367FAF01, 0xAFA94693, 0x8D4DA2A0, 0xED0882A3);
pub const IID_IBSTREAM: DualIid = DualIid::new(0xC3BF6EA2, 0x30994752, 0x9B6BF990, 0x1EE33E9B);
pub const IID_IPARAMETERCHANGES: DualIid = DualIid::new(0xA4779663, 0x0BB64A56, 0xB44384A8, 0x466FEB9D);
pub const IID_IPARAMVALUEQUEUE: DualIid = DualIid::new(0x01263A18, 0xED074F6F, 0x98C9D356, 0x4686F9BA);
pub const IID_IMESSAGE: DualIid = DualIid::new(0x936F033B, 0xC6C047DB, 0xBB0882F8, 0x13C1E613);
pub const IID_IATTRIBUTELIST: DualIid = DualIid::new(0x1E5F0AEB, 0xCC7F4533, 0xA2544011, 0x38AD5EE4);

#[repr(C)]
pub struct FUnknownVtbl {
    pub query_interface: unsafe extern "system" fn(this: *mut c_void, iid: *const TUID, obj: *mut *mut c_void) -> TResult,
    pub add_ref: unsafe extern "system" fn(this: *mut c_void) -> u32,
    pub release: unsafe extern "system" fn(this: *mut c_void) -> u32,
}

unsafe fn query_interface_dual(obj: *mut c_void, dual: &DualIid) -> *mut c_void {
    if obj.is_null() {
        return ptr::null_mut();
    }
    let vtbl = &**(obj as *mut *const FUnknownVtbl);
    let mut out: *mut c_void = ptr::null_mut();
    if (vtbl.query_interface)(obj, &dual.com, &mut out) == K_RESULT_OK && !out.is_null() {
        return out;
    }
    out = ptr::null_mut();
    if (vtbl.query_interface)(obj, &dual.be, &mut out) == K_RESULT_OK && !out.is_null() {
        return out;
    }
    ptr::null_mut()
}

#[repr(C)]
pub struct PFactoryInfo {
    pub vendor: [c_char; 64],
    pub url: [c_char; 256],
    pub email: [c_char; 128],
    pub flags: i32,
}

#[repr(C)]
pub struct PClassInfo {
    pub cid: TUID,
    pub cardinality: i32,
    pub category: [c_char; 32],
    pub name: [c_char; 64],
}

#[repr(C)]
pub struct IPluginFactoryVtbl {
    pub unknown: FUnknownVtbl,
    pub get_factory_info: unsafe extern "system" fn(this: *mut c_void, info: *mut PFactoryInfo) -> TResult,
    pub count_classes: unsafe extern "system" fn(this: *mut c_void) -> i32,
    pub get_class_info: unsafe extern "system" fn(this: *mut c_void, index: i32, info: *mut PClassInfo) -> TResult,
    pub create_instance: unsafe extern "system" fn(
        this: *mut c_void,
        cid: *const TUID,
        iid: *const TUID,
        obj: *mut *mut c_void,
    ) -> TResult,
}

unsafe fn factory_create_dual(
    factory: *mut c_void,
    f_vtbl: &IPluginFactoryVtbl,
    cid: &TUID,
    target_iid: &DualIid,
) -> *mut c_void {
    let mut obj: *mut c_void = ptr::null_mut();
    if (f_vtbl.create_instance)(factory, cid, &target_iid.com, &mut obj) == K_RESULT_OK && !obj.is_null() {
        return obj;
    }
    obj = ptr::null_mut();
    if (f_vtbl.create_instance)(factory, cid, &target_iid.be, &mut obj) == K_RESULT_OK && !obj.is_null() {
        return obj;
    }
    // Fallback: buat sebagai FUnknown / IPluginBase lalu queryInterface ke target_iid
    for base_iid in [&IID_IPLUGINBASE.com, &IID_FUNKNOWN.com, &IID_IPLUGINBASE.be, &IID_FUNKNOWN.be] {
        let mut base_obj: *mut c_void = ptr::null_mut();
        if (f_vtbl.create_instance)(factory, cid, base_iid, &mut base_obj) == K_RESULT_OK && !base_obj.is_null() {
            let queried = query_interface_dual(base_obj, target_iid);
            let b_vtbl = &**(base_obj as *mut *const FUnknownVtbl);
            (b_vtbl.release)(base_obj);
            if !queried.is_null() {
                return queried;
            }
        }
    }
    ptr::null_mut()
}

#[repr(C)]
pub struct IPluginBaseVtbl {
    pub unknown: FUnknownVtbl,
    pub initialize: unsafe extern "system" fn(this: *mut c_void, context: *mut c_void) -> TResult,
    pub terminate: unsafe extern "system" fn(this: *mut c_void) -> TResult,
}

#[repr(C)]
pub struct BusInfo {
    pub media_type: i32,
    pub direction: i32,
    pub channel_count: i32,
    pub name: [u16; 128],
    pub bus_type: i32,
    pub flags: u32,
}

#[repr(C)]
pub struct RoutingInfo {
    pub media_type: i32,
    pub bus_index: i32,
    pub channel: i32,
}

#[repr(C)]
pub struct IComponentVtbl {
    pub base: IPluginBaseVtbl,
    pub get_controller_class_id: unsafe extern "system" fn(this: *mut c_void, class_id: *mut TUID) -> TResult,
    pub set_io_mode: unsafe extern "system" fn(this: *mut c_void, mode: i32) -> TResult,
    pub get_bus_count: unsafe extern "system" fn(this: *mut c_void, media_type: i32, dir: i32) -> i32,
    pub get_bus_info: unsafe extern "system" fn(this: *mut c_void, media_type: i32, dir: i32, index: i32, bus: *mut BusInfo) -> TResult,
    pub get_routing_info: unsafe extern "system" fn(this: *mut c_void, in_info: *mut RoutingInfo, out_info: *mut RoutingInfo) -> TResult,
    pub activate_bus: unsafe extern "system" fn(this: *mut c_void, media_type: i32, dir: i32, index: i32, state: u8) -> TResult,
    pub set_active: unsafe extern "system" fn(this: *mut c_void, state: u8) -> TResult,
    pub set_state: unsafe extern "system" fn(this: *mut c_void, state: *mut c_void) -> TResult,
    pub get_state: unsafe extern "system" fn(this: *mut c_void, state: *mut c_void) -> TResult,
}

#[repr(C)]
pub struct IConnectionPointVtbl {
    pub unknown: FUnknownVtbl,
    pub connect: unsafe extern "system" fn(this: *mut c_void, other: *mut c_void) -> TResult,
    pub disconnect: unsafe extern "system" fn(this: *mut c_void, other: *mut c_void) -> TResult,
    pub notify: unsafe extern "system" fn(this: *mut c_void, message: *mut c_void) -> TResult,
}

#[repr(C)]
pub struct ProcessSetup {
    pub process_mode: i32,
    pub symbolic_sample_size: i32,
    pub max_samples_per_block: i32,
    pub sample_rate: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AudioBusBuffers {
    pub num_channels: i32,
    pub silence_flags: u64,
    pub channel_buffers_32: *mut *mut f32,
}

#[repr(C)]
#[derive(Default)]
pub struct ProcessContext {
    pub state: u32,
    pub sample_rate: f64,
    pub project_time_samples: i64,
    pub system_time: i64,
    pub continous_time_samples: i64,
    pub project_time_music: f64,
    pub bar_position_music: f64,
    pub cycle_start_music: f64,
    pub cycle_end_music: f64,
    pub tempo: f64,
    pub time_sig_numerator: i32,
    pub time_sig_denominator: i32,
    pub chord: i32,
    pub smpte_offset_subframes: i32,
    pub frame_rate: u32,
    pub samples_to_next_clock: i32,
}

#[repr(C)]
pub struct ProcessData {
    pub process_mode: i32,
    pub symbolic_sample_size: i32,
    pub num_samples: i32,
    pub num_inputs: i32,
    pub num_outputs: i32,
    pub inputs: *mut AudioBusBuffers,
    pub outputs: *mut AudioBusBuffers,
    pub input_parameter_changes: *mut c_void,
    pub output_parameter_changes: *mut c_void,
    pub input_events: *mut c_void,
    pub output_events: *mut c_void,
    pub process_context: *mut ProcessContext,
}

#[repr(C)]
pub struct IAudioProcessorVtbl {
    pub unknown: FUnknownVtbl,
    pub set_bus_arrangements: unsafe extern "system" fn(
        this: *mut c_void,
        inputs: *mut u64,
        num_ins: i32,
        outputs: *mut u64,
        num_outs: i32,
    ) -> TResult,
    pub get_bus_arrangement: unsafe extern "system" fn(
        this: *mut c_void,
        dir: i32,
        index: i32,
        arr: *mut u64,
    ) -> TResult,
    pub can_process_sample_size: unsafe extern "system" fn(this: *mut c_void, symbolic_sample_size: i32) -> TResult,
    pub get_latency_samples: unsafe extern "system" fn(this: *mut c_void) -> u32,
    pub setup_processing: unsafe extern "system" fn(this: *mut c_void, setup: *mut ProcessSetup) -> TResult,
    pub set_processing: unsafe extern "system" fn(this: *mut c_void, state: u8) -> TResult,
    pub process: unsafe extern "system" fn(this: *mut c_void, data: *mut ProcessData) -> TResult,
    pub get_tail_samples: unsafe extern "system" fn(this: *mut c_void) -> u32,
}

#[repr(C)]
pub struct ParameterInfo {
    pub id: u32,
    pub title: [u16; 128],
    pub short_title: [u16; 128],
    pub units: [u16; 128],
    pub step_count: i32,
    pub default_normalized_value: f64,
    pub unit_id: i32,
    pub flags: i32,
}

#[repr(C)]
pub struct IEditControllerVtbl {
    pub base: IPluginBaseVtbl,
    pub set_component_state: unsafe extern "system" fn(this: *mut c_void, state: *mut c_void) -> TResult,
    pub set_state: unsafe extern "system" fn(this: *mut c_void, state: *mut c_void) -> TResult,
    pub get_state: unsafe extern "system" fn(this: *mut c_void, state: *mut c_void) -> TResult,
    pub get_parameter_count: unsafe extern "system" fn(this: *mut c_void) -> i32,
    pub get_parameter_info: unsafe extern "system" fn(this: *mut c_void, param_index: i32, info: *mut ParameterInfo) -> TResult,
    pub get_param_string_by_value: unsafe extern "system" fn(this: *mut c_void, id: u32, value_normalized: f64, string: *mut u16) -> TResult,
    pub get_param_value_by_string: unsafe extern "system" fn(this: *mut c_void, id: u32, string: *const u16, value_normalized: *mut f64) -> TResult,
    pub normalized_param_to_plain: unsafe extern "system" fn(this: *mut c_void, id: u32, value_normalized: f64) -> f64,
    pub plain_param_to_normalized: unsafe extern "system" fn(this: *mut c_void, id: u32, plain_value: f64) -> f64,
    pub get_param_normalized: unsafe extern "system" fn(this: *mut c_void, id: u32) -> f64,
    pub set_param_normalized: unsafe extern "system" fn(this: *mut c_void, id: u32, value: f64) -> TResult,
    pub set_component_handler: unsafe extern "system" fn(this: *mut c_void, handler: *mut c_void) -> TResult,
    pub create_view: unsafe extern "system" fn(this: *mut c_void, name: *const c_char) -> *mut c_void,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct ViewRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[repr(C)]
pub struct IPlugViewVtbl {
    pub unknown: FUnknownVtbl,
    pub is_platform_type_supported: unsafe extern "system" fn(this: *mut c_void, platform_type: *const c_char) -> TResult,
    pub attached: unsafe extern "system" fn(this: *mut c_void, parent: *mut c_void, platform_type: *const c_char) -> TResult,
    pub removed: unsafe extern "system" fn(this: *mut c_void) -> TResult,
    pub on_wheel: unsafe extern "system" fn(this: *mut c_void, distance: f32) -> TResult,
    pub on_key_down: unsafe extern "system" fn(this: *mut c_void, key: u16, key_code: i16, modifiers: i16) -> TResult,
    pub on_key_up: unsafe extern "system" fn(this: *mut c_void, key: u16, key_code: i16, modifiers: i16) -> TResult,
    pub get_size: unsafe extern "system" fn(this: *mut c_void, size: *mut ViewRect) -> TResult,
    pub on_size: unsafe extern "system" fn(this: *mut c_void, new_size: *mut ViewRect) -> TResult,
    pub on_focus: unsafe extern "system" fn(this: *mut c_void, state: u8) -> TResult,
    pub set_frame: unsafe extern "system" fn(this: *mut c_void, frame: *mut c_void) -> TResult,
    pub can_resize: unsafe extern "system" fn(this: *mut c_void) -> TResult,
    pub check_size_constraint: unsafe extern "system" fn(this: *mut c_void, rect: *mut ViewRect) -> TResult,
}

// ==============================================================================
// 3. IMPLEMENTASI COM IBSTREAM (UNTUK SINKRONISASI STATE COMPONENT -> CONTROLLER)
// ==============================================================================

#[repr(C)]
struct IBStreamVtbl {
    unknown: FUnknownVtbl,
    read: unsafe extern "system" fn(this: *mut c_void, buffer: *mut c_void, num_bytes: i32, bytes_read: *mut i32) -> TResult,
    write: unsafe extern "system" fn(this: *mut c_void, buffer: *const c_void, num_bytes: i32, bytes_written: *mut i32) -> TResult,
    seek: unsafe extern "system" fn(this: *mut c_void, pos: i64, mode: i32, result: *mut i64) -> TResult,
    tell: unsafe extern "system" fn(this: *mut c_void, pos: *mut i64) -> TResult,
}

#[repr(C)]
pub struct MemoryBStream {
    vtbl: *const IBStreamVtbl,
    pub data: Vec<u8>,
    pub cursor: usize,
}

unsafe extern "system" fn bstream_qi(this: *mut c_void, iid: *const TUID, obj: *mut *mut c_void) -> TResult {
    if iid.is_null() || obj.is_null() {
        return K_NO_INTERFACE;
    }
    let id = &*iid;
    if IID_FUNKNOWN.matches(id) || IID_IBSTREAM.matches(id) {
        *obj = this;
        return K_RESULT_OK;
    }
    *obj = ptr::null_mut();
    K_NO_INTERFACE
}
unsafe extern "system" fn noop_add_ref(_this: *mut c_void) -> u32 { 1 }
unsafe extern "system" fn noop_release(_this: *mut c_void) -> u32 { 1 }

unsafe extern "system" fn bstream_read(
    this: *mut c_void,
    buffer: *mut c_void,
    num_bytes: i32,
    bytes_read: *mut i32,
) -> TResult {
    if buffer.is_null() || num_bytes < 0 {
        return K_INVALID_ARGUMENT;
    }
    let s = &mut *(this as *mut MemoryBStream);
    let avail = s.data.len().saturating_sub(s.cursor);
    let to_read = (num_bytes as usize).min(avail);
    if to_read > 0 {
        ptr::copy_nonoverlapping(s.data.as_ptr().add(s.cursor), buffer as *mut u8, to_read);
        s.cursor += to_read;
    }
    if !bytes_read.is_null() {
        *bytes_read = to_read as i32;
    }
    K_RESULT_OK
}

unsafe extern "system" fn bstream_write(
    this: *mut c_void,
    buffer: *const c_void,
    num_bytes: i32,
    bytes_written: *mut i32,
) -> TResult {
    if buffer.is_null() || num_bytes < 0 {
        return K_INVALID_ARGUMENT;
    }
    let s = &mut *(this as *mut MemoryBStream);
    let n = num_bytes as usize;
    let end = s.cursor + n;
    if end > s.data.len() {
        s.data.resize(end, 0);
    }
    if n > 0 {
        ptr::copy_nonoverlapping(buffer as *const u8, s.data.as_mut_ptr().add(s.cursor), n);
        s.cursor += n;
    }
    if !bytes_written.is_null() {
        *bytes_written = n as i32;
    }
    K_RESULT_OK
}

unsafe extern "system" fn bstream_seek(
    this: *mut c_void,
    pos: i64,
    mode: i32,
    result: *mut i64,
) -> TResult {
    let s = &mut *(this as *mut MemoryBStream);
    let base: i64 = match mode {
        0 => 0,                      // kIBSeekSet
        1 => s.cursor as i64,        // kIBSeekCur
        2 => s.data.len() as i64,    // kIBSeekEnd
        _ => return K_INVALID_ARGUMENT,
    };
    let new_pos = (base + pos).max(0) as usize;
    s.cursor = new_pos.min(s.data.len());
    if !result.is_null() {
        *result = s.cursor as i64;
    }
    K_RESULT_OK
}

unsafe extern "system" fn bstream_tell(this: *mut c_void, pos: *mut i64) -> TResult {
    if pos.is_null() {
        return K_INVALID_ARGUMENT;
    }
    let s = &*(this as *mut MemoryBStream);
    *pos = s.cursor as i64;
    K_RESULT_OK
}

static BSTREAM_VTBL: IBStreamVtbl = IBStreamVtbl {
    unknown: FUnknownVtbl {
        query_interface: bstream_qi,
        add_ref: noop_add_ref,
        release: noop_release,
    },
    read: bstream_read,
    write: bstream_write,
    seek: bstream_seek,
    tell: bstream_tell,
};

impl MemoryBStream {
    pub fn new() -> Self {
        Self {
            vtbl: &BSTREAM_VTBL,
            data: Vec::with_capacity(4096),
            cursor: 0,
        }
    }
}

// ==============================================================================
// 4. IMPLEMENTASI COM IMESSAGE & IATTRIBUTELIST (UNTUK ANALYZER FABFILTER & JUCE)
// ==============================================================================

#[repr(C)]
struct IAttributeListVtbl {
    unknown: FUnknownVtbl,
    set_int: unsafe extern "system" fn(this: *mut c_void, id: *const c_char, value: i64) -> TResult,
    get_int: unsafe extern "system" fn(this: *mut c_void, id: *const c_char, value: *mut i64) -> TResult,
    set_float: unsafe extern "system" fn(this: *mut c_void, id: *const c_char, value: f64) -> TResult,
    get_float: unsafe extern "system" fn(this: *mut c_void, id: *const c_char, value: *mut f64) -> TResult,
    set_string: unsafe extern "system" fn(this: *mut c_void, id: *const c_char, string: *const u16) -> TResult,
    get_string: unsafe extern "system" fn(this: *mut c_void, id: *const c_char, string: *mut u16, size_in_bytes: u32) -> TResult,
    set_binary: unsafe extern "system" fn(this: *mut c_void, id: *const c_char, data: *const c_void, size_in_bytes: u32) -> TResult,
    get_binary: unsafe extern "system" fn(this: *mut c_void, id: *const c_char, data: *mut *const c_void, size_in_bytes: *mut u32) -> TResult,
}

#[derive(Default)]
struct AttrInner {
    ints: HashMap<String, i64>,
    floats: HashMap<String, f64>,
    strings: HashMap<String, Vec<u16>>,
    binaries: HashMap<String, Box<[u8]>>,
}

#[repr(C)]
struct HostAttributeList {
    vtbl: *const IAttributeListVtbl,
    ref_count: AtomicU32,
    inner: Mutex<AttrInner>,
}

unsafe fn attr_key(id: *const c_char) -> String {
    if id.is_null() {
        String::new()
    } else {
        CStr::from_ptr(id).to_string_lossy().into_owned()
    }
}

unsafe extern "system" fn attr_qi(this: *mut c_void, iid: *const TUID, obj: *mut *mut c_void) -> TResult {
    if iid.is_null() || obj.is_null() {
        return K_NO_INTERFACE;
    }
    let id = &*iid;
    if IID_FUNKNOWN.matches(id) || IID_IATTRIBUTELIST.matches(id) {
        attr_add_ref(this);
        *obj = this;
        return K_RESULT_OK;
    }
    *obj = ptr::null_mut();
    K_NO_INTERFACE
}
unsafe extern "system" fn attr_add_ref(this: *mut c_void) -> u32 {
    let a = &*(this as *mut HostAttributeList);
    a.ref_count.fetch_add(1, Ordering::Relaxed) + 1
}
unsafe extern "system" fn attr_release(this: *mut c_void) -> u32 {
    let a = &*(this as *mut HostAttributeList);
    let prev = a.ref_count.fetch_sub(1, Ordering::Release);
    if prev == 1 {
        std::sync::atomic::fence(Ordering::Acquire);
        let _ = Box::from_raw(this as *mut HostAttributeList);
        0
    } else {
        prev - 1
    }
}
unsafe extern "system" fn attr_set_int(this: *mut c_void, id: *const c_char, value: i64) -> TResult {
    let a = &*(this as *mut HostAttributeList);
    a.inner.lock().ints.insert(attr_key(id), value);
    K_RESULT_OK
}
unsafe extern "system" fn attr_get_int(this: *mut c_void, id: *const c_char, value: *mut i64) -> TResult {
    if value.is_null() {
        return K_INVALID_ARGUMENT;
    }
    let a = &*(this as *mut HostAttributeList);
    if let Some(&v) = a.inner.lock().ints.get(&attr_key(id)) {
        *value = v;
        K_RESULT_OK
    } else {
        K_RESULT_FALSE
    }
}
unsafe extern "system" fn attr_set_float(this: *mut c_void, id: *const c_char, value: f64) -> TResult {
    let a = &*(this as *mut HostAttributeList);
    a.inner.lock().floats.insert(attr_key(id), value);
    K_RESULT_OK
}
unsafe extern "system" fn attr_get_float(this: *mut c_void, id: *const c_char, value: *mut f64) -> TResult {
    if value.is_null() {
        return K_INVALID_ARGUMENT;
    }
    let a = &*(this as *mut HostAttributeList);
    if let Some(&v) = a.inner.lock().floats.get(&attr_key(id)) {
        *value = v;
        K_RESULT_OK
    } else {
        K_RESULT_FALSE
    }
}
unsafe extern "system" fn attr_set_string(this: *mut c_void, id: *const c_char, s: *const u16) -> TResult {
    if s.is_null() {
        return K_INVALID_ARGUMENT;
    }
    let mut len = 0usize;
    while len < 4096 && *s.add(len) != 0 {
        len += 1;
    }
    let mut vec = Vec::with_capacity(len + 1);
    for i in 0..len {
        vec.push(*s.add(i));
    }
    vec.push(0);
    let a = &*(this as *mut HostAttributeList);
    a.inner.lock().strings.insert(attr_key(id), vec);
    K_RESULT_OK
}
unsafe extern "system" fn attr_get_string(this: *mut c_void, id: *const c_char, s: *mut u16, sz: u32) -> TResult {
    if s.is_null() || sz < 2 {
        return K_INVALID_ARGUMENT;
    }
    let a = &*(this as *mut HostAttributeList);
    if let Some(vec) = a.inner.lock().strings.get(&attr_key(id)) {
        let max_chars = (sz as usize / 2).saturating_sub(1);
        let copy_len = vec.len().min(max_chars);
        ptr::copy_nonoverlapping(vec.as_ptr(), s, copy_len);
        *s.add(copy_len) = 0;
        K_RESULT_OK
    } else {
        K_RESULT_FALSE
    }
}
unsafe extern "system" fn attr_set_binary(
    this: *mut c_void,
    id: *const c_char,
    data: *const c_void,
    size: u32,
) -> TResult {
    let a = &*(this as *mut HostAttributeList);
    let mut vec = vec![0u8; size as usize];
    if !data.is_null() && size > 0 {
        ptr::copy_nonoverlapping(data as *const u8, vec.as_mut_ptr(), size as usize);
    }
    a.inner.lock().binaries.insert(attr_key(id), vec.into_boxed_slice());
    K_RESULT_OK
}
unsafe extern "system" fn attr_get_binary(
    this: *mut c_void,
    id: *const c_char,
    data: *mut *const c_void,
    size: *mut u32,
) -> TResult {
    if data.is_null() || size.is_null() {
        return K_INVALID_ARGUMENT;
    }
    let a = &*(this as *mut HostAttributeList);
    let guard = a.inner.lock();
    if let Some(boxed) = guard.binaries.get(&attr_key(id)) {
        *data = boxed.as_ptr() as *const c_void;
        *size = boxed.len() as u32;
        K_RESULT_OK
    } else {
        *data = ptr::null();
        *size = 0;
        K_RESULT_FALSE
    }
}

static ATTR_LIST_VTBL: IAttributeListVtbl = IAttributeListVtbl {
    unknown: FUnknownVtbl {
        query_interface: attr_qi,
        add_ref: attr_add_ref,
        release: attr_release,
    },
    set_int: attr_set_int,
    get_int: attr_get_int,
    set_float: attr_set_float,
    get_float: attr_get_float,
    set_string: attr_set_string,
    get_string: attr_get_string,
    set_binary: attr_set_binary,
    get_binary: attr_get_binary,
};

impl HostAttributeList {
    fn new_boxed() -> *mut HostAttributeList {
        Box::into_raw(Box::new(Self {
            vtbl: &ATTR_LIST_VTBL,
            ref_count: AtomicU32::new(1),
            inner: Mutex::new(AttrInner::default()),
        }))
    }
}

#[repr(C)]
struct IMessageVtbl {
    unknown: FUnknownVtbl,
    get_message_id: unsafe extern "system" fn(this: *mut c_void) -> *const c_char,
    set_message_id: unsafe extern "system" fn(this: *mut c_void, id: *const c_char),
    get_attributes: unsafe extern "system" fn(this: *mut c_void) -> *mut c_void,
}

#[repr(C)]
struct HostMessage {
    vtbl: *const IMessageVtbl,
    ref_count: AtomicU32,
    msg_lock: Mutex<()>,
    msg_id: [u8; 256],
    attrs: *mut HostAttributeList,
}

unsafe extern "system" fn msg_qi(this: *mut c_void, iid: *const TUID, obj: *mut *mut c_void) -> TResult {
    if iid.is_null() || obj.is_null() {
        return K_NO_INTERFACE;
    }
    let id = &*iid;
    if IID_FUNKNOWN.matches(id) || IID_IMESSAGE.matches(id) {
        msg_add_ref(this);
        *obj = this;
        return K_RESULT_OK;
    }
    *obj = ptr::null_mut();
    K_NO_INTERFACE
}
unsafe extern "system" fn msg_add_ref(this: *mut c_void) -> u32 {
    let m = &*(this as *mut HostMessage);
    m.ref_count.fetch_add(1, Ordering::Relaxed) + 1
}
unsafe extern "system" fn msg_release(this: *mut c_void) -> u32 {
    let m = &*(this as *mut HostMessage);
    let prev = m.ref_count.fetch_sub(1, Ordering::Release);
    if prev == 1 {
        std::sync::atomic::fence(Ordering::Acquire);
        let boxed = Box::from_raw(this as *mut HostMessage);
        if !boxed.attrs.is_null() {
            attr_release(boxed.attrs as *mut c_void);
        }
        0
    } else {
        prev - 1
    }
}
unsafe extern "system" fn msg_get_id(this: *mut c_void) -> *const c_char {
    let m = &*(this as *const HostMessage);
    m.msg_id.as_ptr() as *const c_char
}
unsafe extern "system" fn msg_set_id(this: *mut c_void, id: *const c_char) {
    let m = &mut *(this as *mut HostMessage);
    let _g = m.msg_lock.lock();
    m.msg_id.fill(0);
    if !id.is_null() {
        let bytes = CStr::from_ptr(id).to_bytes();
        let n = bytes.len().min(254);
        m.msg_id[..n].copy_from_slice(&bytes[..n]);
        m.msg_id[n] = 0;
    }
}
unsafe extern "system" fn msg_get_attrs(this: *mut c_void) -> *mut c_void {
    let m = &*(this as *mut HostMessage);
    m.attrs as *mut c_void
}

static MESSAGE_VTBL: IMessageVtbl = IMessageVtbl {
    unknown: FUnknownVtbl {
        query_interface: msg_qi,
        add_ref: msg_add_ref,
        release: msg_release,
    },
    get_message_id: msg_get_id,
    set_message_id: msg_set_id,
    get_attributes: msg_get_attrs,
};

impl HostMessage {
    fn new_boxed() -> *mut HostMessage {
        Box::into_raw(Box::new(Self {
            vtbl: &MESSAGE_VTBL,
            ref_count: AtomicU32::new(1),
            msg_lock: Mutex::new(()),
            msg_id: [0u8; 256],
            attrs: HostAttributeList::new_boxed(),
        }))
    }
}

// ==============================================================================
// 5. IHOSTAPPLICATION, ICOMPONENTHANDLER & IPARAMETERCHANGES
// ==============================================================================

#[repr(C)]
struct HostAppVtbl {
    unknown: FUnknownVtbl,
    get_name: unsafe extern "system" fn(this: *mut c_void, name: *mut u16) -> TResult,
    create_instance: unsafe extern "system" fn(this: *mut c_void, cid: *const TUID, iid: *const TUID, obj: *mut *mut c_void) -> TResult,
}

#[repr(C)]
struct HostAppCom {
    vtbl: *const HostAppVtbl,
}

unsafe extern "system" fn host_qi(this: *mut c_void, iid: *const TUID, obj: *mut *mut c_void) -> TResult {
    if iid.is_null() || obj.is_null() {
        return K_NO_INTERFACE;
    }
    let id = &*iid;
    if IID_FUNKNOWN.matches(id) || IID_IHOSTAPPLICATION.matches(id) {
        *obj = this;
        return K_RESULT_OK;
    }
    *obj = ptr::null_mut();
    K_NO_INTERFACE
}
unsafe extern "system" fn host_get_name(_this: *mut c_void, name: *mut u16) -> TResult {
    if !name.is_null() {
        let s = "Rinka SoundBridge v2.0";
        for (i, ch) in s.encode_utf16().chain(std::iter::once(0)).enumerate() {
            if i < 127 {
                *name.add(i) = ch;
            }
        }
    }
    K_RESULT_OK
}
unsafe extern "system" fn host_create_instance(
    _this: *mut c_void,
    cid: *const TUID,
    iid: *const TUID,
    obj: *mut *mut c_void,
) -> TResult {
    if obj.is_null() || cid.is_null() || iid.is_null() {
        return K_INVALID_ARGUMENT;
    }
    let c = &*cid;
    let i = &*iid;
    if IID_IMESSAGE.matches(c) || IID_IMESSAGE.matches(i) {
        *obj = HostMessage::new_boxed() as *mut c_void;
        return K_RESULT_OK;
    }
    if IID_IATTRIBUTELIST.matches(c) || IID_IATTRIBUTELIST.matches(i) {
        *obj = HostAttributeList::new_boxed() as *mut c_void;
        return K_RESULT_OK;
    }
    *obj = ptr::null_mut();
    K_NO_INTERFACE
}

static HOST_APP_VTBL: HostAppVtbl = HostAppVtbl {
    unknown: FUnknownVtbl {
        query_interface: host_qi,
        add_ref: noop_add_ref,
        release: noop_release,
    },
    get_name: host_get_name,
    create_instance: host_create_instance,
};

static mut GLOBAL_HOST_APP: HostAppCom = HostAppCom {
    vtbl: &HOST_APP_VTBL,
};

/// Antrean perubahan parameter per-plugin dari GUI (`IEditController`) ke DSP (`IAudioProcessor`).
pub type SharedParamQueue = Arc<Mutex<Vec<(u32, f64)>>>;
/// Tabel nilai parameter terkini yang diperbarui secara real-time saat user memutar knob di GUI VST,
/// sehingga penyimpanan config tidak perlu memanggil fungsi COM `getParamNormalized` lintas thread!
pub type SharedLiveParams = Arc<Mutex<HashMap<u32, f64>>>;

#[repr(C)]
struct ComponentHandlerVtbl {
    unknown: FUnknownVtbl,
    begin_edit: unsafe extern "system" fn(this: *mut c_void, id: u32) -> TResult,
    perform_edit: unsafe extern "system" fn(this: *mut c_void, id: u32, value_normalized: f64) -> TResult,
    end_edit: unsafe extern "system" fn(this: *mut c_void, id: u32) -> TResult,
    restart_component: unsafe extern "system" fn(this: *mut c_void, flags: i32) -> TResult,
}

#[repr(C)]
pub struct PluginComponentHandler {
    vtbl: *const ComponentHandlerVtbl,
    pub closed: AtomicBool,
    pub queue: SharedParamQueue,
    pub live_params: SharedLiveParams,
    pub controller: *mut c_void,
}

unsafe extern "system" fn ch_qi(this: *mut c_void, iid: *const TUID, obj: *mut *mut c_void) -> TResult {
    if iid.is_null() || obj.is_null() {
        return K_NO_INTERFACE;
    }
    let id = &*iid;
    if IID_FUNKNOWN.matches(id) || IID_ICOMPONENTHANDLER.matches(id) {
        *obj = this;
        return K_RESULT_OK;
    }
    *obj = ptr::null_mut();
    K_NO_INTERFACE
}
unsafe extern "system" fn ch_begin(_this: *mut c_void, _id: u32) -> TResult {
    K_RESULT_OK
}
unsafe extern "system" fn ch_perform(this: *mut c_void, id: u32, value_normalized: f64) -> TResult {
    if this.is_null() {
        return K_RESULT_OK;
    }
    let ch = &*(this as *mut PluginComponentHandler);
    if ch.closed.load(Ordering::Relaxed) {
        return K_RESULT_OK;
    }
    if value_normalized.is_finite() {
        let clamped = value_normalized.clamp(0.0, 1.0);
        {
            let mut q = ch.queue.lock();
            if let Some(existing) = q.iter_mut().find(|(pid, _)| *pid == id) {
                existing.1 = clamped;
            } else if q.len() < 512 {
                q.push((id, clamped));
            }
        }
        if let Some(mut lp) = ch.live_params.try_lock() {
            lp.insert(id, clamped);
        }
    }
    K_RESULT_OK
}
unsafe extern "system" fn ch_end(_this: *mut c_void, _id: u32) -> TResult {
    K_RESULT_OK
}
unsafe extern "system" fn ch_restart(_this: *mut c_void, _flags: i32) -> TResult {
    K_RESULT_OK
}

static COMP_HANDLER_VTBL: ComponentHandlerVtbl = ComponentHandlerVtbl {
    unknown: FUnknownVtbl {
        query_interface: ch_qi,
        add_ref: noop_add_ref,
        release: noop_release,
    },
    begin_edit: ch_begin,
    perform_edit: ch_perform,
    end_edit: ch_end,
    restart_component: ch_restart,
};

// Implementasi COM IParamValueQueue & IParameterChanges bebas-realokasi untuk ProcessData
const MAX_PARAM_QUEUES: usize = 512;

#[repr(C)]
struct IParamValueQueueVtbl {
    unknown: FUnknownVtbl,
    get_parameter_id: unsafe extern "system" fn(this: *mut c_void) -> u32,
    get_point_count: unsafe extern "system" fn(this: *mut c_void) -> i32,
    get_point: unsafe extern "system" fn(this: *mut c_void, index: i32, sample_offset: *mut i32, value: *mut f64) -> TResult,
    add_point: unsafe extern "system" fn(this: *mut c_void, sample_offset: i32, value: f64, index: *mut i32) -> TResult,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct HostParamValueQueue {
    vtbl: *const IParamValueQueueVtbl,
    pub param_id: u32,
    pub value: f64,
}

unsafe extern "system" fn pvq_qi(this: *mut c_void, iid: *const TUID, obj: *mut *mut c_void) -> TResult {
    if iid.is_null() || obj.is_null() {
        return K_NO_INTERFACE;
    }
    let id = &*iid;
    if IID_FUNKNOWN.matches(id) || IID_IPARAMVALUEQUEUE.matches(id) {
        *obj = this;
        return K_RESULT_OK;
    }
    *obj = ptr::null_mut();
    K_NO_INTERFACE
}
unsafe extern "system" fn pvq_get_id(this: *mut c_void) -> u32 {
    (*(this as *const HostParamValueQueue)).param_id
}
unsafe extern "system" fn pvq_get_count(_this: *mut c_void) -> i32 {
    1
}
unsafe extern "system" fn pvq_get_point(
    this: *mut c_void,
    index: i32,
    sample_offset: *mut i32,
    value: *mut f64,
) -> TResult {
    if index != 0 {
        return K_INVALID_ARGUMENT;
    }
    let q = &*(this as *const HostParamValueQueue);
    if !sample_offset.is_null() {
        *sample_offset = 0;
    }
    if !value.is_null() {
        *value = q.value;
    }
    K_RESULT_OK
}
unsafe extern "system" fn pvq_add_point(
    this: *mut c_void,
    _sample_offset: i32,
    value: f64,
    index: *mut i32,
) -> TResult {
    let q = &mut *(this as *mut HostParamValueQueue);
    q.value = value;
    if !index.is_null() {
        *index = 0;
    }
    K_RESULT_OK
}

static PVQ_VTBL: IParamValueQueueVtbl = IParamValueQueueVtbl {
    unknown: FUnknownVtbl {
        query_interface: pvq_qi,
        add_ref: noop_add_ref,
        release: noop_release,
    },
    get_parameter_id: pvq_get_id,
    get_point_count: pvq_get_count,
    get_point: pvq_get_point,
    add_point: pvq_add_point,
};

#[repr(C)]
struct IParameterChangesVtbl {
    unknown: FUnknownVtbl,
    get_parameter_count: unsafe extern "system" fn(this: *mut c_void) -> i32,
    get_parameter_data: unsafe extern "system" fn(this: *mut c_void, index: i32) -> *mut c_void,
    add_parameter_data: unsafe extern "system" fn(this: *mut c_void, id: *const u32, index: *mut i32) -> *mut c_void,
}

/// Kapasitas `queues` dikunci pada `MAX_PARAM_QUEUES` (512) tanpa pernah realokasi di heap,
/// sehingga pointer `*mut HostParamValueQueue` yang dikembalikan ke plugin di dalam `process()`
/// dijamin 100% stabil dan tidak pernah memicu Use-After-Free!
#[repr(C)]
pub struct HostParameterChanges {
    vtbl: *const IParameterChangesVtbl,
    pub queues: Box<[HostParamValueQueue; MAX_PARAM_QUEUES]>,
    pub len: usize,
}

unsafe extern "system" fn pc_qi(this: *mut c_void, iid: *const TUID, obj: *mut *mut c_void) -> TResult {
    if iid.is_null() || obj.is_null() {
        return K_NO_INTERFACE;
    }
    let id = &*iid;
    if IID_FUNKNOWN.matches(id) || IID_IPARAMETERCHANGES.matches(id) {
        *obj = this;
        return K_RESULT_OK;
    }
    *obj = ptr::null_mut();
    K_NO_INTERFACE
}
unsafe extern "system" fn pc_get_count(this: *mut c_void) -> i32 {
    (*(this as *const HostParameterChanges)).len as i32
}
unsafe extern "system" fn pc_get_data(this: *mut c_void, index: i32) -> *mut c_void {
    let pc = &mut *(this as *mut HostParameterChanges);
    if index >= 0 && (index as usize) < pc.len {
        &mut pc.queues[index as usize] as *mut HostParamValueQueue as *mut c_void
    } else {
        ptr::null_mut()
    }
}
unsafe extern "system" fn pc_add_data(
    this: *mut c_void,
    id: *const u32,
    index: *mut i32,
) -> *mut c_void {
    if id.is_null() {
        return ptr::null_mut();
    }
    let pid = *id;
    let pc = &mut *(this as *mut HostParameterChanges);
    let idx = if let Some(pos) = pc.queues[..pc.len].iter().position(|q| q.param_id == pid) {
        pos
    } else if pc.len < MAX_PARAM_QUEUES {
        let pos = pc.len;
        pc.queues[pos] = HostParamValueQueue {
            vtbl: &PVQ_VTBL,
            param_id: pid,
            value: 0.0,
        };
        pc.len += 1;
        pos
    } else {
        let pos = MAX_PARAM_QUEUES - 1;
        pc.queues[pos].param_id = pid;
        pc.queues[pos].value = 0.0;
        pos
    };
    if !index.is_null() {
        *index = idx as i32;
    }
    &mut pc.queues[idx] as *mut HostParamValueQueue as *mut c_void
}

static PARAM_CHANGES_VTBL: IParameterChangesVtbl = IParameterChangesVtbl {
    unknown: FUnknownVtbl {
        query_interface: pc_qi,
        add_ref: noop_add_ref,
        release: noop_release,
    },
    get_parameter_count: pc_get_count,
    get_parameter_data: pc_get_data,
    add_parameter_data: pc_add_data,
};

impl HostParameterChanges {
    pub fn new() -> Self {
        Self {
            vtbl: &PARAM_CHANGES_VTBL,
            queues: Box::new([HostParamValueQueue {
                vtbl: &PVQ_VTBL,
                param_id: 0,
                value: 0.0,
            }; MAX_PARAM_QUEUES]),
            len: 0,
        }
    }
    #[inline]
    pub fn clear(&mut self) {
        self.len = 0;
    }
    #[inline]
    pub fn load_from_slice(&mut self, items: &[(u32, f64)]) {
        self.len = 0;
        for &(param_id, value) in items.iter().take(MAX_PARAM_QUEUES) {
            self.queues[self.len] = HostParamValueQueue {
                vtbl: &PVQ_VTBL,
                param_id,
                value,
            };
            self.len += 1;
        }
    }
}

// ==============================================================================
// 6. IMPLEMENTASI COM IPLUGFRAME (UNTUK RESIZE JENDELA GUI VST3)
// ==============================================================================

#[repr(C)]
struct IPlugFrameVtbl {
    unknown: FUnknownVtbl,
    resize_view: unsafe extern "system" fn(this: *mut c_void, view: *mut c_void, new_size: *mut ViewRect) -> TResult,
}

#[repr(C)]
pub struct HostPlugFrame {
    vtbl: *const IPlugFrameVtbl,
    ref_count: AtomicU32,
    in_resize: AtomicBool,
    pub hwnd: isize,
}

unsafe extern "system" fn pf_qi(this: *mut c_void, iid: *const TUID, obj: *mut *mut c_void) -> TResult {
    if iid.is_null() || obj.is_null() {
        return K_NO_INTERFACE;
    }
    let id = &*iid;
    if IID_FUNKNOWN.matches(id) || IID_IPLUGFRAME.matches(id) {
        pf_add_ref(this);
        *obj = this;
        return K_RESULT_OK;
    }
    *obj = ptr::null_mut();
    K_NO_INTERFACE
}

unsafe extern "system" fn pf_add_ref(this: *mut c_void) -> u32 {
    let pf = &*(this as *mut HostPlugFrame);
    pf.ref_count.fetch_add(1, Ordering::Relaxed) + 1
}

unsafe extern "system" fn pf_release(this: *mut c_void) -> u32 {
    let pf = &*(this as *mut HostPlugFrame);
    let prev = pf.ref_count.fetch_sub(1, Ordering::Release);
    if prev == 1 {
        std::sync::atomic::fence(Ordering::Acquire);
        let _ = Box::from_raw(this as *mut HostPlugFrame);
        0
    } else {
        prev - 1
    }
}

unsafe extern "system" fn pf_resize_view(
    this: *mut c_void,
    view: *mut c_void,
    new_size: *mut ViewRect,
) -> TResult {
    if view.is_null() || new_size.is_null() {
        return K_INVALID_ARGUMENT;
    }
    let pf = &*(this as *const HostPlugFrame);
    if pf.hwnd == 0 {
        return K_RESULT_FALSE;
    }
    // Cegah rekursi tak terbatas jika plugin memanggil resizeView lagi di dalam onSize()
    if pf.in_resize.swap(true, Ordering::SeqCst) {
        return K_RESULT_OK;
    }
    let r = *new_size;
    let client_w = (r.right - r.left).max(200);
    let client_h = (r.bottom - r.top).max(120);

    #[cfg(windows)]
    {
        use crate::win32::ffi;
        if pf.hwnd != 0 {
            let mut wr = ffi::RECT::default();
            ffi::GetWindowRect(pf.hwnd, &mut wr);
            ffi::SetWindowPos(
                pf.hwnd,
                0,
                wr.left,
                wr.top,
                client_w + 16,
                client_h + 39,
                0x0004 | 0x0020 | 0x0010, // SWP_NOZORDER | SWP_FRAMECHANGED | SWP_NOACTIVATE
            );
        }
    }

    let v_vtbl = &**(view as *mut *const IPlugViewVtbl);
    let mut nr = r;
    let _ = (v_vtbl.on_size)(view, &mut nr);
    pf.in_resize.store(false, Ordering::SeqCst);
    K_RESULT_OK
}

static PLUG_FRAME_VTBL: IPlugFrameVtbl = IPlugFrameVtbl {
    unknown: FUnknownVtbl {
        query_interface: pf_qi,
        add_ref: pf_add_ref,
        release: pf_release,
    },
    resize_view: pf_resize_view,
};

// ==============================================================================
// 7. RUNNER PEMROSES AUDIO REAL-TIME (TERPISAH DARI LOCK GUI EDITOR!)
// ==============================================================================

pub const VST_SCRATCH_CAPACITY: usize = 8192;
pub const VST_OPTIMAL_SUBBLOCK: usize = 2048;

pub struct VstAudioProcessor {
    pub _lib: Arc<Library>,
    pub component: *mut c_void,
    pub processor: *mut c_void,
    pub active_sample_rate: f64,
    pub max_block_size: usize,
    pub num_in_buses: usize,
    pub num_out_buses: usize,
    pub main_in_channels: usize,
    pub main_out_channels: usize,
    pub latency_samples: u32,
    pub is_active: bool,
    pub is_processing: bool,
    pub param_queue: SharedParamQueue,
    in_param_changes: HostParameterChanges,
    out_param_changes: HostParameterChanges,
    sample_counter: i64,
    // Pre-allocated scratch buffers (8192 sampel, 16-channel guard) bebas alokasi heap
    in_ch_bufs: Vec<Vec<f32>>,
    out_ch_bufs: Vec<Vec<f32>>,
    silent_buf: Vec<f32>,
    dummy_out_buf: Vec<f32>,
    in_buses_vec: Vec<AudioBusBuffers>,
    out_buses_vec: Vec<AudioBusBuffers>,
}

unsafe impl Send for VstAudioProcessor {}
unsafe impl Sync for VstAudioProcessor {}

impl VstAudioProcessor {
    /// Dipanggil HANYA pada main UI thread saat inisialisasi plugin (`load_vst3_pair`).
    pub fn setup_processing(&mut self, sample_rate: f64, max_block_size: usize) {
        self.active_sample_rate = sample_rate;
        self.max_block_size = max_block_size.max(1024).clamp(256, VST_OPTIMAL_SUBBLOCK);

        unsafe {
            let c_vtbl = &**(self.component as *mut *const IComponentVtbl);
            let p_vtbl = &**(self.processor as *mut *const IAudioProcessorVtbl);

            // Hanya matikan processing/active jika sebelumnya memang sudah aktif!
            // (Memanggil setActive(0) pada komponen yang baru di-initialize melanggar state machine VST3)
            if self.is_processing {
                let _ = (p_vtbl.set_processing)(self.processor, 0);
                self.is_processing = false;
            }
            if self.is_active {
                let _ = (c_vtbl.set_active)(self.component, 0);
                self.is_active = false;
            }

            let in_buses = (c_vtbl.get_bus_count)(self.component, 0, 0).max(0) as usize;
            let out_buses = (c_vtbl.get_bus_count)(self.component, 0, 1).max(0) as usize;
            self.num_in_buses = in_buses.max(1);
            self.num_out_buses = out_buses.max(1);

            // Negosiasi susunan bus bertahap seperti JUCE AudioPluginHost:
            // 1) Main Stereo (0x3) + Sidechain Kosong (0x0)
            // 2) Semua Bus Stereo (0x3)
            // 3) Hanya Bus 0 Stereo (num_ins = 1, num_outs = 1)
            if in_buses > 0 && out_buses > 0 {
                let mut in_arrs = vec![0u64; in_buses];
                let mut out_arrs = vec![0u64; out_buses];
                in_arrs[0] = 0x3;
                out_arrs[0] = 0x3;
                let r1 = (p_vtbl.set_bus_arrangements)(
                    self.processor,
                    in_arrs.as_mut_ptr(),
                    in_buses as i32,
                    out_arrs.as_mut_ptr(),
                    out_buses as i32,
                );
                if r1 != K_RESULT_OK {
                    let mut in_all = vec![0x3u64; in_buses];
                    let mut out_all = vec![0x3u64; out_buses];
                    let r2 = (p_vtbl.set_bus_arrangements)(
                        self.processor,
                        in_all.as_mut_ptr(),
                        in_buses as i32,
                        out_all.as_mut_ptr(),
                        out_buses as i32,
                    );
                    if r2 != K_RESULT_OK {
                        let mut in_one = [0x3u64];
                        let mut out_one = [0x3u64];
                        let _ = (p_vtbl.set_bus_arrangements)(
                            self.processor,
                            in_one.as_mut_ptr(),
                            1,
                            out_one.as_mut_ptr(),
                            1,
                        );
                    }
                }
            }

            // Periksa jumlah kanal aktual pada Bus 0 setelah negosiasi
            let mut in_binfo: BusInfo = std::mem::zeroed();
            if in_buses > 0 && (c_vtbl.get_bus_info)(self.component, 0, 0, 0, &mut in_binfo) == K_RESULT_OK {
                self.main_in_channels = (in_binfo.channel_count as usize).clamp(1, 2);
            } else {
                self.main_in_channels = 2;
            }

            let mut out_binfo: BusInfo = std::mem::zeroed();
            if out_buses > 0 && (c_vtbl.get_bus_info)(self.component, 0, 1, 0, &mut out_binfo) == K_RESULT_OK {
                self.main_out_channels = (out_binfo.channel_count as usize).clamp(1, 2);
            } else {
                self.main_out_channels = 2;
            }

            // Aktifkan Main Bus 0 dan nonaktifkan bus sidechain tambahan (b >= 1)
            for b in 0..in_buses {
                let _ = (c_vtbl.activate_bus)(self.component, 0, 0, b as i32, if b == 0 { 1 } else { 0 });
            }
            for b in 0..out_buses {
                let _ = (c_vtbl.activate_bus)(self.component, 0, 1, b as i32, if b == 0 { 1 } else { 0 });
            }

            let mut setup = ProcessSetup {
                process_mode: 0,         // kRealtime
                symbolic_sample_size: 0, // kSample32
                max_samples_per_block: self.max_block_size as i32,
                sample_rate,
            };
            let _ = (p_vtbl.setup_processing)(self.processor, &mut setup);
            if (c_vtbl.set_active)(self.component, 1) == K_RESULT_OK {
                self.is_active = true;
            } else {
                self.is_active = true; // Tetap tandai aktif agar teardown aman
            }
            let _ = (p_vtbl.set_processing)(self.processor, 1);
            self.is_processing = true;
            self.latency_samples = (p_vtbl.get_latency_samples)(self.processor);
        }

        self.in_ch_bufs = vec![vec![0.0f32; VST_SCRATCH_CAPACITY]; 2];
        self.out_ch_bufs = vec![vec![0.0f32; VST_SCRATCH_CAPACITY]; 2];
        self.silent_buf = vec![0.0f32; VST_SCRATCH_CAPACITY];
        self.dummy_out_buf = vec![0.0f32; VST_SCRATCH_CAPACITY];
        self.in_buses_vec = vec![
            AudioBusBuffers {
                num_channels: 0,
                silence_flags: 0x3,
                channel_buffers_32: ptr::null_mut(),
            };
            self.num_in_buses.max(1)
        ];
        self.out_buses_vec = vec![
            AudioBusBuffers {
                num_channels: 0,
                silence_flags: 0,
                channel_buffers_32: ptr::null_mut(),
            };
            self.num_out_buses.max(1)
        ];
    }

    /// Reset ringan tanpa memanggil `IComponent::setActive(0)` di thread audio!
    pub fn reset(&mut self) {
        for b in &mut self.in_ch_bufs {
            b.fill(0.0);
        }
        for b in &mut self.out_ch_bufs {
            b.fill(0.0);
        }
        self.silent_buf.fill(0.0);
        self.dummy_out_buf.fill(0.0);
    }

    pub fn process_stereo(&mut self, left: &mut [f32], right: &mut [f32], sample_rate: u32) -> Result<(), String> {
        let frames = left.len().min(right.len());
        if frames == 0 {
            return Ok(());
        }
        if frames > self.max_block_size {
            let step = self.max_block_size;
            let mut offset = 0;
            while offset < frames {
                let end = (offset + step).min(frames);
                self.process_stereo(&mut left[offset..end], &mut right[offset..end], sample_rate)?;
                offset = end;
            }
            return Ok(());
        }

        self.active_sample_rate = sample_rate as f64;

        // 1. Ambil antrean perubahan parameter dari GUI / preset tanpa menahan lock
        self.in_param_changes.clear();
        if let Some(mut q) = self.param_queue.try_lock() {
            if !q.is_empty() {
                self.in_param_changes.load_from_slice(&q);
                q.clear();
            }
        }
        self.out_param_changes.clear();

        // 2. Siapkan buffer input & output terpisah (dilengkapi guard 16-channel agar tidak pernah out-of-bounds)
        //    `silent_buf` sudah bernilai 0.0 permanen (read-only bagi plugin), sehingga tidak perlu di-zero ulang setiap blok!
        self.in_ch_bufs[0][..frames].copy_from_slice(&left[..frames]);
        self.in_ch_bufs[1][..frames].copy_from_slice(&right[..frames]);
        self.out_ch_bufs[0][..frames].fill(0.0);
        self.out_ch_bufs[1][..frames].fill(0.0);

        let sil_ptr = self.silent_buf.as_mut_ptr();
        let dum_ptr = self.dummy_out_buf.as_mut_ptr();

        let mut main_in_ptrs: [*mut f32; 16] = [sil_ptr; 16];
        main_in_ptrs[0] = self.in_ch_bufs[0].as_mut_ptr();
        main_in_ptrs[1] = self.in_ch_bufs[1].as_mut_ptr();

        let mut main_out_ptrs: [*mut f32; 16] = [dum_ptr; 16];
        main_out_ptrs[0] = self.out_ch_bufs[0].as_mut_ptr();
        main_out_ptrs[1] = self.out_ch_bufs[1].as_mut_ptr();

        let mut silent_in_ptrs: [*mut f32; 16] = [sil_ptr; 16];
        let mut dummy_out_ptrs: [*mut f32; 16] = [dum_ptr; 16];

        // 3. Gunakan pre-allocated AudioBusBuffers:
        //    - Bus 0 (Main): aktif dengan jumlah kanal utama (1 atau 2)
        //    - Bus >= 1 (Sidechain/Aux yang dinonaktifkan): num_channels = 0 namun channel_buffers_32
        //      tetap menunjuk ke silent_in_ptrs yang valid (mencegah crash null/unallocated pada FabFilter Pro-MB/Pro-Q 3!)
        let n_in = self.num_in_buses.max(1);
        let n_out = self.num_out_buses.max(1);
        for b in &mut self.in_buses_vec {
            b.num_channels = 0;
            b.silence_flags = u64::MAX;
            b.channel_buffers_32 = silent_in_ptrs.as_mut_ptr();
        }
        self.in_buses_vec[0] = AudioBusBuffers {
            num_channels: self.main_in_channels as i32,
            silence_flags: 0,
            channel_buffers_32: main_in_ptrs.as_mut_ptr(),
        };

        for b in &mut self.out_buses_vec {
            b.num_channels = 0;
            b.silence_flags = 0;
            b.channel_buffers_32 = dummy_out_ptrs.as_mut_ptr();
        }
        self.out_buses_vec[0] = AudioBusBuffers {
            num_channels: self.main_out_channels as i32,
            silence_flags: 0,
            channel_buffers_32: main_out_ptrs.as_mut_ptr(),
        };

        // kPlaying (1<<1) | kTempoValid (1<<10) | kTimeSigValid (1<<13) | kContTimeValid (1<<15)
        let mut ctx = ProcessContext {
            state: (1 << 1) | (1 << 10) | (1 << 13) | (1 << 15),
            sample_rate: sample_rate as f64,
            project_time_samples: self.sample_counter,
            continous_time_samples: self.sample_counter,
            tempo: 120.0,
            time_sig_numerator: 4,
            time_sig_denominator: 4,
            ..Default::default()
        };
        self.sample_counter = self.sample_counter.wrapping_add(frames as i64);

        let mut data = ProcessData {
            process_mode: 0,
            symbolic_sample_size: 0,
            num_samples: frames as i32,
            num_inputs: n_in as i32,
            num_outputs: n_out as i32,
            inputs: self.in_buses_vec.as_mut_ptr(),
            outputs: self.out_buses_vec.as_mut_ptr(),
            input_parameter_changes: &mut self.in_param_changes as *mut HostParameterChanges as *mut c_void,
            output_parameter_changes: &mut self.out_param_changes as *mut HostParameterChanges as *mut c_void,
            input_events: ptr::null_mut(),
            output_events: ptr::null_mut(),
            process_context: &mut ctx,
        };

        unsafe {
            let p_vtbl = &**(self.processor as *mut *const IAudioProcessorVtbl);
            let res = (p_vtbl.process)(self.processor, &mut data);
            if res != K_RESULT_OK && res != K_RESULT_FALSE {
                return Err(format!("IAudioProcessor::process failed (code {})", res));
            }
        }

        // 4. Validasi NaN/Inf & flush subnormal terlebih dahulu pada `out_ch_bufs` sebelum menyalin ke `left`/`right`.
        //    Dengan demikian, jika plugin menghasilkan NaN/Inf, buffer `left`/`right` asli tetap 100% utuh
        //    tanpa perlu menyalin `prev_l`/`prev_r` sebelum setiap slot VST!
        let out0 = &self.out_ch_bufs[0][..frames];
        if self.main_out_channels == 1 {
            for &s in out0 {
                if !s.is_finite() {
                    return Err("Plugin output contained NaN/Inf".into());
                }
            }
            for i in 0..frames {
                let s = out0[i];
                let clean = if s.abs() < 1.0e-30 { 0.0 } else { s };
                left[i] = clean;
                right[i] = clean;
            }
        } else {
            let out1 = &self.out_ch_bufs[1][..frames];
            for i in 0..frames {
                if !out0[i].is_finite() || !out1[i].is_finite() {
                    return Err("Plugin output contained NaN/Inf".into());
                }
            }
            for i in 0..frames {
                let l = out0[i];
                let r = out1[i];
                left[i] = if l.abs() < 1.0e-30 { 0.0 } else { l };
                right[i] = if r.abs() < 1.0e-30 { 0.0 } else { r };
            }
        }

        Ok(())
    }
}

// ==============================================================================
// 8. VST3 CONTROLLER & NON-BLOCKING WIN32 GUI EDITOR
// ==============================================================================

pub struct ActiveEditorWindow {
    pub view: *mut c_void,
    pub plug_frame: *mut HostPlugFrame,
    pub hwnd: isize,
    pub was_visible: bool,
}

unsafe impl Send for ActiveEditorWindow {}
unsafe impl Sync for ActiveEditorWindow {}

pub struct VstController {
    pub _lib: Arc<Library>,
    pub component: *mut c_void,
    pub controller: *mut c_void,
    pub comp_cp: *mut c_void,
    pub ctrl_cp: *mut c_void,
    pub separate_controller: bool,
    pub handler_box: *mut PluginComponentHandler,
    pub param_name_to_id: HashMap<String, u32>,
    pub param_queue: SharedParamQueue,
    pub live_params: SharedLiveParams,
    pub active_editor: Option<ActiveEditorWindow>,
}

unsafe impl Send for VstController {}
unsafe impl Sync for VstController {}

impl VstController {
    /// Ambil snapshot parameter langsung dari cache `live_params` yang diperbarui secara otomatis
    /// oleh `IComponentHandler::performEdit` (tanpa memanggil COM `getParamNormalized` saat DSP aktif!).
    pub fn capture_parameters(&self) -> HashMap<String, f64> {
        let mut map = HashMap::new();
        let lp = self.live_params.lock();
        for (&id, &val) in lp.iter() {
            if val.is_finite() {
                map.insert(format!("__id_{}", id), val);
            }
        }
        for (name, &id) in &self.param_name_to_id {
            if let Some(&val) = lp.get(&id) {
                if val.is_finite() {
                    map.insert(name.clone(), val);
                }
            }
        }
        map
    }

    pub fn restore_parameters(&self, params: &HashMap<String, f64>) {
        if self.controller.is_null() || params.is_empty() {
            return;
        }
        let mut queued = Vec::new();
        unsafe {
            let ctrl_vtbl = &**(self.controller as *mut *const IEditControllerVtbl);
            let mut lp = self.live_params.lock();
            for (key, &val) in params {
                let id_opt = if let Some(rest) = key.strip_prefix("__id_") {
                    rest.parse::<u32>().ok()
                } else {
                    self.param_name_to_id.get(key).copied()
                };
                if let Some(id) = id_opt {
                    let clamped = val.clamp(0.0, 1.0);
                    let _ = (ctrl_vtbl.set_param_normalized)(self.controller, id, clamped);
                    lp.insert(id, clamped);
                    queued.push((id, clamped));
                }
            }
        }
        if !queued.is_empty() {
            let mut q = self.param_queue.lock();
            for (id, val) in queued {
                if let Some(existing) = q.iter_mut().find(|(pid, _)| *pid == id) {
                    existing.1 = val;
                } else if q.len() < MAX_PARAM_QUEUES {
                    q.push((id, val));
                }
            }
        }
    }

    /// Tangkap binary chunk state (`IComponent::getState` & `IEditController::getState`) dalam format Hex
    /// agar kurva EQ dinamis (mis. FabFilter Pro-Q 3 / Pro-MB / TDR Nova) tersimpan utuh di `rinka_soundbridge_config.json`.
    pub fn capture_binary_states(&self) -> (Option<String>, Option<String>) {
        fn encode_hex(bytes: &[u8]) -> String {
            const HEX: &[u8; 16] = b"0123456789abcdef";
            let mut out = String::with_capacity(bytes.len() * 2);
            for &b in bytes {
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0x0F) as usize] as char);
            }
            out
        }
        let mut comp_hex = None;
        let mut ctrl_hex = None;
        unsafe {
            if !self.component.is_null() {
                let c_vtbl = &**(self.component as *mut *const IComponentVtbl);
                let mut stream = MemoryBStream::new();
                if (c_vtbl.get_state)(self.component, &mut stream as *mut MemoryBStream as *mut c_void) == K_RESULT_OK
                    && !stream.data.is_empty()
                    && stream.data.len() <= 512 * 1024
                {
                    comp_hex = Some(encode_hex(&stream.data));
                }
            }
            if !self.controller.is_null() {
                let ctrl_vtbl = &**(self.controller as *mut *const IEditControllerVtbl);
                let mut stream = MemoryBStream::new();
                if (ctrl_vtbl.get_state)(self.controller, &mut stream as *mut MemoryBStream as *mut c_void) == K_RESULT_OK
                    && !stream.data.is_empty()
                    && stream.data.len() <= 512 * 1024
                {
                    ctrl_hex = Some(encode_hex(&stream.data));
                }
            }
        }
        (comp_hex, ctrl_hex)
    }

    pub fn restore_binary_states(&self, comp_hex: Option<&str>, ctrl_hex: Option<&str>) {
        fn decode_hex(hex: &str) -> Option<Vec<u8>> {
            let s = hex.trim();
            if s.is_empty() || s.len() % 2 != 0 {
                return None;
            }
            let mut out = Vec::with_capacity(s.len() / 2);
            let bytes = s.as_bytes();
            for i in (0..bytes.len()).step_by(2) {
                let hi = (bytes[i] as char).to_digit(16)?;
                let lo = (bytes[i + 1] as char).to_digit(16)?;
                out.push(((hi << 4) | lo) as u8);
            }
            Some(out)
        }
        unsafe {
            if let Some(ch) = comp_hex {
                if let Some(raw) = decode_hex(ch) {
                    if !raw.is_empty() {
                        let mut stream = MemoryBStream::new();
                        stream.data = raw;
                        stream.cursor = 0;
                        if !self.component.is_null() {
                            let c_vtbl = &**(self.component as *mut *const IComponentVtbl);
                            let _ = (c_vtbl.set_state)(
                                self.component,
                                &mut stream as *mut MemoryBStream as *mut c_void,
                            );
                        }
                        if !self.controller.is_null() {
                            stream.cursor = 0;
                            let ctrl_vtbl = &**(self.controller as *mut *const IEditControllerVtbl);
                            let _ = (ctrl_vtbl.set_component_state)(
                                self.controller,
                                &mut stream as *mut MemoryBStream as *mut c_void,
                            );
                        }
                    }
                }
            }
            if let Some(ch) = ctrl_hex {
                if let Some(raw) = decode_hex(ch) {
                    if !raw.is_empty() && !self.controller.is_null() {
                        let mut stream = MemoryBStream::new();
                        stream.data = raw;
                        stream.cursor = 0;
                        let ctrl_vtbl = &**(self.controller as *mut *const IEditControllerVtbl);
                        let _ = (ctrl_vtbl.set_state)(
                            self.controller,
                            &mut stream as *mut MemoryBStream as *mut c_void,
                        );
                    }
                }
            }
            // Sinkronkan kembali live_params dari controller seusai binary chunk dimuat
            if (comp_hex.is_some() || ctrl_hex.is_some()) && !self.controller.is_null() {
                let ctrl_vtbl = &**(self.controller as *mut *const IEditControllerVtbl);
                let p_count = (ctrl_vtbl.get_parameter_count)(self.controller);
                let mut lp = self.live_params.lock();
                let mut q = self.param_queue.lock();
                for idx in 0..p_count {
                    let mut pinfo: ParameterInfo = std::mem::zeroed();
                    if (ctrl_vtbl.get_parameter_info)(self.controller, idx, &mut pinfo) == K_RESULT_OK {
                        let val = (ctrl_vtbl.get_param_normalized)(self.controller, pinfo.id);
                        if val.is_finite() {
                            let clamped = val.clamp(0.0, 1.0);
                            lp.insert(pinfo.id, clamped);
                            if q.len() < MAX_PARAM_QUEUES {
                                q.push((pinfo.id, clamped));
                            }
                        }
                    }
                }
            }
        }
    }

    /// Buka jendela GUI Editor VST3 secara NON-BLOCKING pada main thread!
    /// Audio thread & grafik spektrum (mis. FabFilter Pro-Q 3 / Pro-MB / Clear) tetap berjalan 60 FPS tanpa terhenti!
    pub fn open_editor_nonblocking(&mut self, plugin_name: &str) -> Result<(), String> {
        if let Some(ref mut ed) = self.active_editor {
            #[cfg(windows)]
            unsafe {
                let _gl_guard = OpenGlContextGuard::save();
                crate::win32::ffi::ShowWindow(ed.hwnd, crate::win32::ffi::SW_SHOW);
                crate::win32::ffi::ShowWindow(ed.hwnd, crate::win32::ffi::SW_RESTORE);
                crate::win32::ffi::SetForegroundWindow(ed.hwnd);
                ed.was_visible = true;
            }
            restore_host_gl_context();
            return Ok(());
        }

        if self.controller.is_null() {
            return Err(format!("Plugin '{}' does not provide an IEditController.", plugin_name));
        }

        #[cfg(windows)]
        unsafe {
            use crate::win32::{ffi, to_wide};
            let _gl_guard = OpenGlContextGuard::save();

            let ctrl_vtbl = &**(self.controller as *mut *const IEditControllerVtbl);

            let view = (ctrl_vtbl.create_view)(self.controller, b"editor\0".as_ptr() as *const c_char);
            if view.is_null() {
                return Err(format!("Plugin '{}' does not provide a custom GUI (IPlugView).", plugin_name));
            }
            let v_vtbl = &**(view as *mut *const IPlugViewVtbl);
            if (v_vtbl.is_platform_type_supported)(view, b"HWND\0".as_ptr() as *const c_char) != K_RESULT_OK {
                (v_vtbl.unknown.release)(view);
                return Err("Plugin does not support the Windows HWND platform".into());
            }

            let mut rect = ViewRect {
                left: 0,
                top: 0,
                right: 760,
                bottom: 500,
            };
            let _ = (v_vtbl.get_size)(view, &mut rect);
            let client_w = (rect.right - rect.left).max(400);
            let client_h = (rect.bottom - rect.top).max(300);
            let w = client_w + 16;
            let h = client_h + 39;

            let screen_w = ffi::GetSystemMetrics(0);
            let screen_h = ffi::GetSystemMetrics(1);
            let x = ((screen_w - w) / 2).max(50);
            let y = ((screen_h - h) / 2).max(50);

            let class_name = to_wide("RinkaVstEditorHostClass");
            let title = to_wide(&format!("VST3: {}", plugin_name));
            let hinst = ffi::GetModuleHandleW(ptr::null());

            let wc = ffi::WNDCLASSEXW {
                cbSize: std::mem::size_of::<ffi::WNDCLASSEXW>() as u32,
                style: 0,
                lpfnWndProc: Some(vst_editor_wndproc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: hinst,
                hIcon: ffi::LoadIconW(0, ffi::IDI_APPLICATION),
                hCursor: ffi::LoadCursorW(0, ffi::IDC_ARROW),
                hbrBackground: 6,
                lpszMenuName: ptr::null(),
                lpszClassName: class_name.as_ptr(),
                hIconSm: 0,
            };
            let _ = ffi::RegisterClassExW(&wc);

            // Buat HWND dalam kondisi belum tampil (tanpa WS_VISIBLE) agar WM_PAINT/WM_SIZE tidak masuk sebelum IPlugView::attached selesai!
            let hwnd = ffi::CreateWindowExW(
                0,
                class_name.as_ptr(),
                title.as_ptr(),
                ffi::WS_OVERLAPPEDWINDOW,
                x,
                y,
                w,
                h,
                0,
                0,
                hinst,
                ptr::null_mut(),
            );
            if hwnd == 0 {
                (v_vtbl.unknown.release)(view);
                return Err("Failed to create Win32 window for VST editor".into());
            }

            let plug_frame = Box::into_raw(Box::new(HostPlugFrame {
                vtbl: &PLUG_FRAME_VTBL,
                ref_count: AtomicU32::new(1),
                in_resize: AtomicBool::new(false),
                hwnd,
            }));
            let _ = (v_vtbl.set_frame)(view, plug_frame as *mut c_void);

            let attach_res = (v_vtbl.attached)(view, hwnd as *mut c_void, b"HWND\0".as_ptr() as *const c_char);
            if attach_res != K_RESULT_OK {
                let _ = (v_vtbl.set_frame)(view, ptr::null_mut());
                pf_release(plug_frame as *mut c_void);
                ffi::DestroyWindow(hwnd);
                (v_vtbl.unknown.release)(view);
                return Err(format!("IPlugView::attached failed (code {})", attach_res));
            }

            // Beberapa plugin memperbarui ukuran idealnya sesaat setelah attached()
            let mut post_rect = rect;
            if (v_vtbl.get_size)(view, &mut post_rect) == K_RESULT_OK {
                let pw = (post_rect.right - post_rect.left).max(320);
                let ph = (post_rect.bottom - post_rect.top).max(220);
                if pw != client_w || ph != client_h {
                    ffi::SetWindowPos(hwnd, 0, x, y, pw + 16, ph + 39, 0x0004 | 0x0020 | 0x0010);
                }
            }

            ffi::ShowWindow(hwnd, ffi::SW_SHOW);
            ffi::SetForegroundWindow(hwnd);
            reassert_crash_handlers();

            self.active_editor = Some(ActiveEditorWindow {
                view,
                plug_frame,
                hwnd,
                was_visible: true,
            });
            log_info(format!("Editor VST '{}' dibuka secara non-blocking.", plugin_name));
            Ok(())
        }
        #[cfg(not(windows))]
        Err("VST3 HWND GUI Editor is only available on Windows".into())
    }

    pub fn hide_editor_window(&mut self) {
        if let Some(ref mut ed) = self.active_editor {
            #[cfg(windows)]
            unsafe {
                let _gl_guard = OpenGlContextGuard::save();
                if ed.hwnd != 0 {
                    crate::win32::ffi::ShowWindow(ed.hwnd, crate::win32::ffi::SW_HIDE);
                }
                ed.was_visible = false;
            }
            restore_host_gl_context();
        }
    }

    pub fn poll_editor_closed(&mut self) -> bool {
        let Some(ref mut ed) = self.active_editor else {
            return false;
        };
        #[cfg(windows)]
        unsafe {
            let visible = crate::win32::ffi::IsWindowVisible(ed.hwnd) != 0;
            if !visible && ed.was_visible {
                ed.was_visible = false;
                record_breadcrumb("Jendela GUI Editor VST ditutup (SW_HIDE)");
                restore_host_gl_context();
                return true;
            }
        }
        false
    }
}

#[cfg(windows)]
unsafe extern "system" fn vst_editor_wndproc(
    hwnd: crate::win32::ffi::HWND,
    msg: crate::win32::ffi::UINT,
    wparam: crate::win32::ffi::WPARAM,
    lparam: crate::win32::ffi::LPARAM,
) -> crate::win32::ffi::LRESULT {
    use crate::win32::ffi;
    if msg == ffi::WM_CLOSE {
        ffi::ShowWindow(hwnd, ffi::SW_HIDE);
        return 0;
    }
    ffi::DefWindowProcW(hwnd, msg, wparam, lparam)
}

// ==============================================================================
// 9. PEMUAT UTAMA VST3 & VSTSLOT (DENGAN LOCK TERPISAH AUDIO VS GUI!)
// ==============================================================================

pub fn load_vst3_pair(
    path: &str,
    sample_rate: f64,
    max_block_size: usize,
) -> Result<(VstAudioProcessor, VstController), String> {
    let bin_path = resolve_vst3_binary(Path::new(path))?;
    let lib = get_or_load_vst3_library(&bin_path)?;

    unsafe {
        let get_factory = lib
            .get::<unsafe extern "system" fn() -> *mut c_void>(b"GetPluginFactory\0")
            .map_err(|e| format!("GetPluginFactory symbol not found: {}", e))?;

        let factory = get_factory();
        if factory.is_null() {
            return Err("GetPluginFactory returned null".into());
        }
        let f_vtbl = &**(factory as *mut *const IPluginFactoryVtbl);

        let num_classes = (f_vtbl.count_classes)(factory);
        let mut audio_cids = Vec::new();
        let mut controller_cids = Vec::new();

        for i in 0..num_classes {
            let mut info: PClassInfo = std::mem::zeroed();
            if (f_vtbl.get_class_info)(factory, i, &mut info) == K_RESULT_OK {
                let cat = CStr::from_ptr(info.category.as_ptr()).to_string_lossy();
                if cat.contains("Audio Module Class") {
                    audio_cids.push(info.cid);
                } else if cat.contains("Component Controller Class") {
                    controller_cids.push(info.cid);
                }
            }
        }

        if audio_cids.is_empty() && num_classes > 0 {
            let mut info: PClassInfo = std::mem::zeroed();
            if (f_vtbl.get_class_info)(factory, 0, &mut info) == K_RESULT_OK {
                audio_cids.push(info.cid);
            }
        }

        let cid = audio_cids
            .first()
            .copied()
            .ok_or_else(|| "No 'Audio Module Class' found in VST3".to_string())?;

        let component = factory_create_dual(factory, f_vtbl, &cid, &IID_ICOMPONENT);
        if component.is_null() {
            (f_vtbl.unknown.release)(factory);
            return Err("Failed to create IComponent instance from IPluginFactory".into());
        }

        let c_vtbl = &**(component as *mut *const IComponentVtbl);
        let host_ctx = std::ptr::addr_of_mut!(GLOBAL_HOST_APP) as *mut c_void;
        let init_res = (c_vtbl.base.initialize)(component, host_ctx);
        if init_res != K_RESULT_OK && init_res != K_RESULT_FALSE {
            (c_vtbl.base.unknown.release)(component);
            (f_vtbl.unknown.release)(factory);
            return Err(format!("IComponent::initialize failed (code {})", init_res));
        }

        let processor = query_interface_dual(component, &IID_IAUDIOPROCESSOR);
        if processor.is_null() {
            (c_vtbl.base.terminate)(component);
            (c_vtbl.base.unknown.release)(component);
            (f_vtbl.unknown.release)(factory);
            return Err("Plugin does not implement IAudioProcessor".into());
        }

        // 1. Cari IEditController: pertama query langsung dari IComponent, lalu lewat getControllerClassId, lalu lewat kelas Component Controller di factory
        let mut controller = query_interface_dual(component, &IID_IEDITCONTROLLER);
        let mut separate_controller = false;

        if controller.is_null() {
            let mut ctrl_cid: TUID = [0; 16];
            if (c_vtbl.get_controller_class_id)(component, &mut ctrl_cid) == K_RESULT_OK && ctrl_cid != [0; 16] {
                controller = factory_create_dual(factory, f_vtbl, &ctrl_cid, &IID_IEDITCONTROLLER);
            }
            if controller.is_null() {
                for fallback_cid in &controller_cids {
                    controller = factory_create_dual(factory, f_vtbl, fallback_cid, &IID_IEDITCONTROLLER);
                    if !controller.is_null() {
                        break;
                    }
                }
            }
            if !controller.is_null() {
                let ctrl_vtbl = &**(controller as *mut *const IEditControllerVtbl);
                let _ = ctrl_vtbl.set_io_mode_or_init(controller, host_ctx);
                separate_controller = true;
            }
        }

        (f_vtbl.unknown.release)(factory);

        let param_queue: SharedParamQueue = Arc::new(Mutex::new(Vec::with_capacity(64)));
        let live_params: SharedLiveParams = Arc::new(Mutex::new(HashMap::with_capacity(64)));
        let handler_box = Box::into_raw(Box::new(PluginComponentHandler {
            vtbl: &COMP_HANDLER_VTBL,
            closed: AtomicBool::new(false),
            queue: Arc::clone(&param_queue),
            live_params: Arc::clone(&live_params),
            controller,
        }));

        let mut comp_cp: *mut c_void = ptr::null_mut();
        let mut ctrl_cp: *mut c_void = ptr::null_mut();
        let mut param_name_to_id = HashMap::new();

        if !controller.is_null() {
            let ctrl_vtbl = &**(controller as *mut *const IEditControllerVtbl);
            let _ = (ctrl_vtbl.set_component_handler)(controller, handler_box as *mut c_void);

            // 2. Hubungkan IComponent <-> IEditController lewat IConnectionPoint
            comp_cp = query_interface_dual(component, &IID_ICONNECTIONPOINT);
            ctrl_cp = query_interface_dual(controller, &IID_ICONNECTIONPOINT);
            if !comp_cp.is_null() && !ctrl_cp.is_null() && comp_cp != ctrl_cp {
                let comp_cp_vtbl = &**(comp_cp as *mut *const IConnectionPointVtbl);
                let ctrl_cp_vtbl = &**(ctrl_cp as *mut *const IConnectionPointVtbl);
                let _ = (comp_cp_vtbl.connect)(comp_cp, ctrl_cp);
                let _ = (ctrl_cp_vtbl.connect)(ctrl_cp, comp_cp);
            }

            // 3. Sinkronkan state awal dari IComponent::getState -> IEditController::setComponentState
            //    (Khususnya jika separate_controller == true)
            if separate_controller {
                let mut stream = MemoryBStream::new();
                if (c_vtbl.get_state)(component, &mut stream as *mut MemoryBStream as *mut c_void) == K_RESULT_OK
                    && !stream.data.is_empty()
                {
                    stream.cursor = 0;
                    let _ = (ctrl_vtbl.set_component_state)(
                        controller,
                        &mut stream as *mut MemoryBStream as *mut c_void,
                    );
                }
            }

            // 4. Baca daftar parameter satu kali saat inisialisasi (sebelum audio berjalan)
            let p_count = (ctrl_vtbl.get_parameter_count)(controller);
            let mut init_params = Vec::new();
            let mut init_live = HashMap::new();
            for idx in 0..p_count {
                let mut pinfo: ParameterInfo = std::mem::zeroed();
                if (ctrl_vtbl.get_parameter_info)(controller, idx, &mut pinfo) == K_RESULT_OK {
                    let len = pinfo.title.iter().position(|&c| c == 0).unwrap_or(128);
                    let name = String::from_utf16_lossy(&pinfo.title[..len]);
                    if !name.is_empty() {
                        param_name_to_id.insert(name, pinfo.id);
                    }
                    let val = (ctrl_vtbl.get_param_normalized)(controller, pinfo.id);
                    if val.is_finite() {
                        init_live.insert(pinfo.id, val);
                        if init_params.len() < MAX_PARAM_QUEUES {
                            init_params.push((pinfo.id, val));
                        }
                    }
                }
            }
            *live_params.lock() = init_live;
            if !init_params.is_empty() {
                param_queue.lock().extend(init_params);
            }
        }

        let mut audio_proc = VstAudioProcessor {
            _lib: Arc::clone(&lib),
            component,
            processor,
            active_sample_rate: sample_rate,
            max_block_size: max_block_size.clamp(64, VST_OPTIMAL_SUBBLOCK),
            num_in_buses: 1,
            num_out_buses: 1,
            main_in_channels: 2,
            main_out_channels: 2,
            latency_samples: 0,
            is_active: false,
            is_processing: false,
            param_queue: Arc::clone(&param_queue),
            in_param_changes: HostParameterChanges::new(),
            out_param_changes: HostParameterChanges::new(),
            sample_counter: 0,
            in_ch_bufs: Vec::new(),
            out_ch_bufs: Vec::new(),
            silent_buf: Vec::new(),
            dummy_out_buf: Vec::new(),
            in_buses_vec: Vec::new(),
            out_buses_vec: Vec::new(),
        };
        audio_proc.setup_processing(sample_rate, max_block_size.clamp(64, VST_OPTIMAL_SUBBLOCK));

        let vst_ctrl = VstController {
            _lib: lib,
            component,
            controller,
            comp_cp,
            ctrl_cp,
            separate_controller,
            handler_box,
            param_name_to_id,
            param_queue,
            live_params,
            active_editor: None,
        };

        reassert_crash_handlers();
        trim_process_working_set();
        Ok((audio_proc, vst_ctrl))
    }
}

trait ControllerInitExt {
    unsafe fn set_io_mode_or_init(&self, ctrl: *mut c_void, host_ctx: *mut c_void) -> TResult;
}
impl ControllerInitExt for IEditControllerVtbl {
    unsafe fn set_io_mode_or_init(&self, ctrl: *mut c_void, host_ctx: *mut c_void) -> TResult {
        (self.base.initialize)(ctrl, host_ctx)
    }
}

pub struct VSTSlot {
    pub file_path: String,
    pub name: String,
    pub audio_proc: Mutex<Option<VstAudioProcessor>>,
    pub gui_ctrl: Mutex<Option<VstController>>,
    pub bypassed: AtomicBool,
    pub reset_pending: AtomicBool,
    pub flush_pending: AtomicBool,
    pub error_count: AtomicU32,
    pub last_error_message: Mutex<Option<String>>,
    pub last_error_time_ms: AtomicU64,
    pub cached_params: Mutex<HashMap<String, f64>>,
    pub cached_comp_state: Mutex<Option<String>>,
    pub cached_ctrl_state: Mutex<Option<String>>,
    pub latency_samples: AtomicU32,
    pub tint_color: String,
    pub last_logged_time: Mutex<Option<Instant>>,
    pub suppressed_logs: AtomicU32,
    pub last_process_ns: AtomicU64,
}

impl VSTSlot {
    pub fn load(file_path: String, sample_rate: f64, max_block_size: usize) -> Result<Self, String> {
        let name = Path::new(&file_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("VST3 Plugin")
            .to_string();
        record_breadcrumb(format!("Memuat VST '{}' ({})", name, file_path));
        let (audio_proc, gui_ctrl) = load_vst3_pair(&file_path, sample_rate, max_block_size)?;
        let init_lat = audio_proc.latency_samples;
        let cached = gui_ctrl.capture_parameters();

        let mut rng = rand::thread_rng();
        let tint = DIM_VST_COLORS
            .choose(&mut rng)
            .copied()
            .unwrap_or("#22333B")
            .to_string();

        Ok(Self {
            file_path,
            name,
            audio_proc: Mutex::new(Some(audio_proc)),
            gui_ctrl: Mutex::new(Some(gui_ctrl)),
            bypassed: AtomicBool::new(false),
            reset_pending: AtomicBool::new(false),
            flush_pending: AtomicBool::new(false),
            error_count: AtomicU32::new(0),
            last_error_message: Mutex::new(None),
            last_error_time_ms: AtomicU64::new(0),
            cached_params: Mutex::new(cached),
            cached_comp_state: Mutex::new(None),
            cached_ctrl_state: Mutex::new(None),
            latency_samples: AtomicU32::new(init_lat),
            tint_color: tint,
            last_logged_time: Mutex::new(None),
            suppressed_logs: AtomicU32::new(0),
            last_process_ns: AtomicU64::new(0),
        })
    }

    pub fn is_bypassed(&self) -> bool {
        self.bypassed.load(Ordering::Relaxed)
    }

    pub fn set_bypassed(&self, val: bool) {
        record_breadcrumb(format!(
            "Mengubah status Bypass VST '{}' menjadi {}",
            self.name,
            if val { "BYPASS (Nonaktif)" } else { "ON (Aktif)" }
        ));
        self.bypassed.store(val, Ordering::Relaxed);
        if !val {
            self.flush_pending.store(true, Ordering::Relaxed);
        }
    }

    pub fn record_error(&self, msg: String) {
        self.error_count.fetch_add(1, Ordering::Relaxed);
        *self.last_error_message.lock() = Some(msg);
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        self.last_error_time_ms.store(now_ms, Ordering::Relaxed);
    }

    pub fn has_recent_error(&self, timeout_sec: f64) -> Option<(u32, String)> {
        let count = self.error_count.load(Ordering::Relaxed);
        if count == 0 {
            return None;
        }
        let last_ms = self.last_error_time_ms.load(Ordering::Relaxed);
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        if now_ms.saturating_sub(last_ms) <= (timeout_sec * 1000.0) as u64 {
            let msg = self
                .last_error_message
                .lock()
                .clone()
                .unwrap_or_else(|| "Unknown error".into());
            Some((count, msg))
        } else {
            None
        }
    }

    pub fn sync_cached_params(&self) {
        if let Some(ref ctrl) = *self.gui_ctrl.lock() {
            *self.cached_params.lock() = ctrl.capture_parameters();
            let (comp_hex, ctrl_hex) = ctrl.capture_binary_states();
            if comp_hex.is_some() {
                *self.cached_comp_state.lock() = comp_hex;
            }
            if ctrl_hex.is_some() {
                *self.cached_ctrl_state.lock() = ctrl_hex;
            }
        }
    }

    pub fn restore_full_state(
        &self,
        params: &HashMap<String, f64>,
        comp_hex: Option<&str>,
        ctrl_hex: Option<&str>,
    ) {
        // JANGAN mengunci `self.audio_proc.lock()` dari thread UI agar callback real-time audio
        // tidak pernah gagal `try_lock()` dan melewatkan pemrosesan VST!
        if let Some(ref ctrl) = *self.gui_ctrl.lock() {
            ctrl.restore_binary_states(comp_hex, ctrl_hex);
            if !params.is_empty() {
                ctrl.restore_parameters(params);
            }
            *self.cached_params.lock() = ctrl.capture_parameters();
            if let Some(ch) = comp_hex {
                *self.cached_comp_state.lock() = Some(ch.to_string());
            }
            if let Some(ch) = ctrl_hex {
                *self.cached_ctrl_state.lock() = Some(ch.to_string());
            }
        }
        self.flush_pending.store(true, Ordering::Relaxed);
    }

    pub fn open_editor(&self) -> Result<(), String> {
        record_breadcrumb(format!("Membuka GUI Editor VST '{}'", self.name));
        let mut guard = self.gui_ctrl.lock();
        if let Some(ref mut ctrl) = *guard {
            ctrl.open_editor_nonblocking(&self.name)
        } else {
            Err("Plugin is already closed".into())
        }
    }

    pub fn hide_editor_window(&self) {
        let mut guard = self.gui_ctrl.lock();
        if let Some(ref mut ctrl) = *guard {
            ctrl.hide_editor_window();
        }
    }

    pub fn poll_editor_closed(&self) -> bool {
        let closed = {
            let mut guard = self.gui_ctrl.lock();
            if let Some(ref mut ctrl) = *guard {
                ctrl.poll_editor_closed()
            } else {
                false
            }
        };
        if closed {
            self.sync_cached_params();
        }
        closed
    }

    pub fn shutdown_and_release(&self) {
        // Kunci dengan urutan konsisten: audio_proc terlebih dahulu, baru gui_ctrl
        let proc_opt = self.audio_proc.lock().take();
        let mut ctrl_opt = self.gui_ctrl.lock().take();

        if proc_opt.is_none() && ctrl_opt.is_none() {
            return;
        }
        record_breadcrumb(format!("Melepas instance VST '{}'", self.name));
        let _gl_guard = OpenGlContextGuard::save();

        unsafe {
            // 1. Jika ada jendela editor GUI, sembunyikan terlebih dahulu & matikan callback resize
            let mut had_active_editor = false;
            if let Some(ref mut ctrl) = ctrl_opt {
                if let Some(ed) = ctrl.active_editor.take() {
                    had_active_editor = true;
                    if !ed.plug_frame.is_null() {
                        (*ed.plug_frame).hwnd = 0;
                    }
                    #[cfg(windows)]
                    if ed.hwnd != 0 {
                        crate::win32::ffi::ShowWindow(ed.hwnd, crate::win32::ffi::SW_HIDE);
                    }
                    // Parkir pointer view & hwnd yang sudah tersembunyi agar tidak memicu crash OpenGL/baseview di tengah sesi
                    let retired = RETIRED_VST_EDITORS.get_or_init(|| Mutex::new(Vec::new()));
                    retired.lock().push((ed.view as usize, ed.plug_frame as usize, ed.hwnd as usize));
                }
            }

            // 2. Hentikan pemrosesan audio (`setProcessing(0)` lalu `setActive(0)`) SELAGI `_lib` & `ctrl` MASIH HIDUP!
            if let Some(ref proc) = proc_opt {
                if !proc.processor.is_null() && proc.is_processing {
                    let p_vtbl = &**(proc.processor as *mut *const IAudioProcessorVtbl);
                    let _ = (p_vtbl.set_processing)(proc.processor, 0);
                }
                if !proc.component.is_null() && proc.is_active {
                    let c_vtbl = &**(proc.component as *mut *const IComponentVtbl);
                    let _ = (c_vtbl.set_active)(proc.component, 0);
                }
            }

            // 3. Putuskan hubungan IComponentHandler & IConnectionPoint
            if let Some(ref mut ctrl) = ctrl_opt {
                if !ctrl.handler_box.is_null() {
                    (*ctrl.handler_box).closed.store(true, Ordering::SeqCst);
                }
                if !ctrl.controller.is_null() {
                    let ctrl_vtbl = &**(ctrl.controller as *mut *const IEditControllerVtbl);
                    let _ = (ctrl_vtbl.set_component_handler)(ctrl.controller, ptr::null_mut());
                }
                if !ctrl.comp_cp.is_null() && !ctrl.ctrl_cp.is_null() && ctrl.comp_cp != ctrl.ctrl_cp {
                    let comp_cp_vtbl = &**(ctrl.comp_cp as *mut *const IConnectionPointVtbl);
                    let ctrl_cp_vtbl = &**(ctrl.ctrl_cp as *mut *const IConnectionPointVtbl);
                    let _ = (comp_cp_vtbl.disconnect)(ctrl.comp_cp, ctrl.ctrl_cp);
                    let _ = (ctrl_cp_vtbl.disconnect)(ctrl.ctrl_cp, ctrl.comp_cp);
                }
                if !ctrl.comp_cp.is_null() {
                    let v = &**(ctrl.comp_cp as *mut *const FUnknownVtbl);
                    (v.release)(ctrl.comp_cp);
                    ctrl.comp_cp = ptr::null_mut();
                }
                if !ctrl.ctrl_cp.is_null() {
                    let v = &**(ctrl.ctrl_cp as *mut *const FUnknownVtbl);
                    (v.release)(ctrl.ctrl_cp);
                    ctrl.ctrl_cp = ptr::null_mut();
                }
            }

            // 4. Jika tidak ada jendela OpenGL aktif yang diparkir, lepaskan interface COM secara berurutan
            if !had_active_editor {
                if let Some(ref proc) = proc_opt {
                    if !proc.processor.is_null() {
                        let p_vtbl = &**(proc.processor as *mut *const IAudioProcessorVtbl);
                        (p_vtbl.unknown.release)(proc.processor);
                    }
                }
                if let Some(ref mut ctrl) = ctrl_opt {
                    if !ctrl.controller.is_null() {
                        let ctrl_vtbl = &**(ctrl.controller as *mut *const IEditControllerVtbl);
                        if ctrl.separate_controller {
                            let _ = (ctrl_vtbl.base.terminate)(ctrl.controller);
                        }
                        (ctrl_vtbl.base.unknown.release)(ctrl.controller);
                        ctrl.controller = ptr::null_mut();
                    }
                }
                if let Some(ref proc) = proc_opt {
                    if !proc.component.is_null() {
                        let c_vtbl = &**(proc.component as *mut *const IComponentVtbl);
                        let _ = (c_vtbl.base.terminate)(proc.component);
                        (c_vtbl.base.unknown.release)(proc.component);
                    }
                }
                if let Some(ref mut ctrl) = ctrl_opt {
                    if !ctrl.handler_box.is_null() {
                        let _ = Box::from_raw(ctrl.handler_box);
                        ctrl.handler_box = ptr::null_mut();
                    }
                }
            }
        }
        restore_host_gl_context();
        trim_process_working_set();
    }
}

impl Drop for VSTSlot {
    fn drop(&mut self) {
        self.shutdown_and_release();
    }
}
