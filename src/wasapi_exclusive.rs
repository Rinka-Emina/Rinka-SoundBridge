#![allow(non_snake_case)]
//! Native Windows WASAPI Exclusive & Flex-ASIO Hybrid COM Engine (`src/wasapi_exclusive.rs`)
//! Mengakses antarmuka COM `IMMDeviceEnumerator` -> `IMMDevice` -> `IAudioClient`
//! untuk:
//! 1. `WasapiExclusivePair`: Mode Exclusive murni (`AUDCLNT_SHAREMODE_EXCLUSIVE | AUDCLNT_STREAMFLAGS_EVENTCALLBACK`)
//! 2. `WasapiFlexAsioPair`: Mode `ASIO Experimental (Flex-Bridge)` Event-Driven berlatensi rendah yang
//!    kompatibel penuh dengan perangkat Multi-Channel / Loopback (seperti SSL Output 1/2 & Loopback 1/2)
//!    serta bebas underrun/patah-patah di semua ukuran sampel (`32..1024 smp`).

use crate::logger::log_info;
use crate::win32::{boost_audio_thread, ffi};
use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

pub const CLSID_MMDEVICE_ENUMERATOR: ffi::GUID = ffi::GUID {
    data1: 0xBCDE0395,
    data2: 0xE52F,
    data3: 0x467C,
    data4: [0x8E, 0x3D, 0xC4, 0x57, 0x92, 0x91, 0x69, 0x2E],
};

pub const IID_IMMDEVICE_ENUMERATOR: ffi::GUID = ffi::GUID {
    data1: 0xA95664D2,
    data2: 0x9614,
    data3: 0x4F35,
    data4: [0xA7, 0x46, 0xDE, 0x8D, 0xB6, 0x36, 0x17, 0xE6],
};

pub const IID_IAUDIOCLIENT: ffi::GUID = ffi::GUID {
    data1: 0x1CB9AD4C,
    data2: 0xDBFA,
    data4: [0xB1, 0x78, 0xC2, 0xF5, 0x68, 0xA7, 0x03, 0xB2],
    data3: 0x4C32,
};

pub const IID_IAUDIOCAPTURECLIENT: ffi::GUID = ffi::GUID {
    data1: 0xC8ADBD64,
    data2: 0xE71E,
    data3: 0x48A0,
    data4: [0xA4, 0xDE, 0x18, 0x5C, 0x39, 0x5C, 0xD3, 0x17],
};

pub const IID_IAUDIORENDERCLIENT: ffi::GUID = ffi::GUID {
    data1: 0xF294ACFC,
    data2: 0x3146,
    data3: 0x4483,
    data4: [0xA7, 0xBF, 0xAD, 0xDC, 0xA7, 0xC2, 0x60, 0xE2],
};

pub const AUDCLNT_SHAREMODE_SHARED: u32 = 0;
pub const AUDCLNT_SHAREMODE_EXCLUSIVE: u32 = 1;
pub const AUDCLNT_STREAMFLAGS_EVENTCALLBACK: u32 = 0x00040000;
pub const AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM: u32 = 0x80000000;
pub const AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY: u32 = 0x08000000;
pub const AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED: i32 = 0x8889_0019u32 as i32;

const STGM_READ: u32 = 0;
const VT_LPWSTR: u16 = 31;

#[repr(C)]
#[derive(Clone, Copy)]
struct PROPERTYKEY {
    fmtid: ffi::GUID,
    pid: u32,
}

const PKEY_DEVICE_FRIENDLY_NAME: PROPERTYKEY = PROPERTYKEY {
    fmtid: ffi::GUID {
        data1: 0xA45C254E,
        data2: 0xDF1C,
        data3: 0x4EFD,
        data4: [0x80, 0x20, 0x67, 0xD1, 0x46, 0xA8, 0x50, 0xE0],
    },
    pid: 14,
};

#[repr(C)]
struct PROPVARIANT {
    vt: u16,
    reserved1: u16,
    reserved2: u16,
    reserved3: u16,
    ptr_val: *mut c_void,
    pad: usize,
}

#[cfg(windows)]
#[link(name = "ole32")]
extern "system" {
    fn PropVariantClear(pvar: *mut PROPVARIANT) -> i32;
}

#[repr(C)]
struct IPropertyStoreVtbl {
    query_interface: unsafe extern "system" fn(*mut c_void, *const ffi::GUID, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    get_count: unsafe extern "system" fn(*mut c_void, *mut u32) -> i32,
    get_at: unsafe extern "system" fn(*mut c_void, u32, *mut PROPERTYKEY) -> i32,
    get_value: unsafe extern "system" fn(*mut c_void, *const PROPERTYKEY, *mut PROPVARIANT) -> i32,
}

#[derive(Clone, Debug)]
pub enum WasapiEndpointTarget {
    ByName(String),
    ByIndex(usize),
}

impl From<String> for WasapiEndpointTarget {
    fn from(s: String) -> Self {
        Self::ByName(s)
    }
}

impl From<&str> for WasapiEndpointTarget {
    fn from(s: &str) -> Self {
        Self::ByName(s.to_string())
    }
}

impl From<usize> for WasapiEndpointTarget {
    fn from(idx: usize) -> Self {
        Self::ByIndex(idx)
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct WAVEFORMATEX {
    pub wFormatTag: u16,
    pub nChannels: u16,
    pub nSamplesPerSec: u32,
    pub nAvgBytesPerSec: u32,
    pub nBlockAlign: u16,
    pub wBitsPerSample: u16,
    pub cbSize: u16,
}

#[repr(C)]
pub struct IMMDeviceEnumeratorVtbl {
    pub query_interface: unsafe extern "system" fn(*mut c_void, *const ffi::GUID, *mut *mut c_void) -> i32,
    pub add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    pub release: unsafe extern "system" fn(*mut c_void) -> u32,
    pub enum_audio_endpoints: unsafe extern "system" fn(*mut c_void, i32, u32, *mut *mut c_void) -> i32,
    pub get_default_audio_endpoint: unsafe extern "system" fn(*mut c_void, i32, i32, *mut *mut c_void) -> i32,
}

#[repr(C)]
pub struct IMMDeviceCollectionVtbl {
    pub query_interface: unsafe extern "system" fn(*mut c_void, *const ffi::GUID, *mut *mut c_void) -> i32,
    pub add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    pub release: unsafe extern "system" fn(*mut c_void) -> u32,
    pub get_count: unsafe extern "system" fn(*mut c_void, *mut u32) -> i32,
    pub item: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> i32,
}

#[repr(C)]
pub struct IMMDeviceVtbl {
    pub query_interface: unsafe extern "system" fn(*mut c_void, *const ffi::GUID, *mut *mut c_void) -> i32,
    pub add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    pub release: unsafe extern "system" fn(*mut c_void) -> u32,
    pub activate: unsafe extern "system" fn(
        *mut c_void,
        *const ffi::GUID,
        u32,
        *mut c_void,
        *mut *mut c_void,
    ) -> i32,
    pub open_property_store: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> i32,
    pub get_id: unsafe extern "system" fn(*mut c_void, *mut *mut u16) -> i32,
}

#[cfg(windows)]
unsafe fn read_mmdevice_friendly_name(device: *mut c_void) -> Option<String> {
    let dev_vtbl = &**(device as *mut *const IMMDeviceVtbl);
    let mut store: *mut c_void = ptr::null_mut();
    if (dev_vtbl.open_property_store)(device, STGM_READ, &mut store) < 0 || store.is_null() {
        return None;
    }
    let store_vtbl = &**(store as *mut *const IPropertyStoreVtbl);
    let mut pv: PROPVARIANT = std::mem::zeroed();
    let hr = (store_vtbl.get_value)(store, &PKEY_DEVICE_FRIENDLY_NAME, &mut pv);
    let mut out = None;
    if hr >= 0 && pv.vt == VT_LPWSTR && !pv.ptr_val.is_null() {
        let wptr = pv.ptr_val as *const u16;
        let mut len = 0usize;
        while len < 1024 && *wptr.add(len) != 0 {
            len += 1;
        }
        let slice = std::slice::from_raw_parts(wptr, len);
        out = Some(String::from_utf16_lossy(slice));
    }
    let _ = PropVariantClear(&mut pv);
    (store_vtbl.release)(store);
    out
}

/// Resolves an active Windows MMDevice endpoint by exact/case-insensitive friendly name
/// (avoiding cpal `eAll` filter-shift & hot-plug index mismatches) or legacy numeric index.
#[cfg(windows)]
unsafe fn open_mmdevice(target: &WasapiEndpointTarget, is_capture: bool) -> Result<*mut c_void, String> {
    let mut enumerator: *mut c_void = ptr::null_mut();
    let hr = ffi::CoCreateInstance(
        &CLSID_MMDEVICE_ENUMERATOR,
        ptr::null_mut(),
        ffi::CLSCTX_ALL,
        &IID_IMMDEVICE_ENUMERATOR,
        &mut enumerator,
    );
    if hr < 0 || enumerator.is_null() {
        return Err(format!("IMMDeviceEnumerator gagal (0x{:08X})", hr));
    }
    let enum_vtbl = &**(enumerator as *mut *const IMMDeviceEnumeratorVtbl);
    // data_flow: 0 = eRender (Output), 1 = eCapture (Input); state_mask: 1 = DEVICE_STATE_ACTIVE
    let data_flow = if is_capture { 1 } else { 0 };
    let mut collection: *mut c_void = ptr::null_mut();
    if (enum_vtbl.enum_audio_endpoints)(enumerator, data_flow, 1, &mut collection) < 0 || collection.is_null() {
        (enum_vtbl.release)(enumerator);
        return Err("EnumAudioEndpoints gagal".into());
    }
    let col_vtbl = &**(collection as *mut *const IMMDeviceCollectionVtbl);

    let result = match target {
        WasapiEndpointTarget::ByIndex(idx) => {
            let mut device: *mut c_void = ptr::null_mut();
            if (col_vtbl.item)(collection, *idx as u32, &mut device) < 0 || device.is_null() {
                Err("Device index not found in IMMDeviceCollection".into())
            } else {
                Ok(device)
            }
        }
        WasapiEndpointTarget::ByName(want_name) => {
            let mut count: u32 = 0;
            if (col_vtbl.get_count)(collection, &mut count) < 0 {
                Err("IMMDeviceCollection::GetCount failed".into())
            } else {
                let mut exact_dev: *mut c_void = ptr::null_mut();
                let mut ci_dev: *mut c_void = ptr::null_mut();
                for i in 0..count {
                    let mut dev: *mut c_void = ptr::null_mut();
                    if (col_vtbl.item)(collection, i, &mut dev) < 0 || dev.is_null() {
                        continue;
                    }
                    let fname = read_mmdevice_friendly_name(dev);
                    if let Some(ref f) = fname {
                        if f == want_name {
                            exact_dev = dev;
                            break;
                        }
                        if ci_dev.is_null() && f.eq_ignore_ascii_case(want_name) {
                            ci_dev = dev;
                            continue;
                        }
                    }
                    let dev_vtbl = &**(dev as *mut *const IMMDeviceVtbl);
                    (dev_vtbl.release)(dev);
                }
                if !exact_dev.is_null() {
                    if !ci_dev.is_null() {
                        let v = &**(ci_dev as *mut *const IMMDeviceVtbl);
                        (v.release)(ci_dev);
                    }
                    Ok(exact_dev)
                } else if !ci_dev.is_null() {
                    Ok(ci_dev)
                } else {
                    Err(format!("WASAPI endpoint '{}' not found in active MMDeviceCollection", want_name))
                }
            }
        }
    };

    (col_vtbl.release)(collection);
    (enum_vtbl.release)(enumerator);
    result
}

#[cfg(windows)]
unsafe fn activate_audio_client_from_device(device: *mut c_void) -> Result<*mut c_void, String> {
    let dev_vtbl = &**(device as *mut *const IMMDeviceVtbl);
    let mut client: *mut c_void = ptr::null_mut();
    let act_hr = (dev_vtbl.activate)(
        device,
        &IID_IAUDIOCLIENT,
        ffi::CLSCTX_ALL,
        ptr::null_mut(),
        &mut client,
    );
    if act_hr < 0 || client.is_null() {
        return Err(format!("IAudioClient::Activate failed (0x{:08X})", act_hr));
    }
    Ok(client)
}

/// Attempts Exclusive Event-Driven `IAudioClient::Initialize` handling `AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED`
/// by querying the hardware-aligned buffer size and re-activating a fresh `IAudioClient`.
#[cfg(windows)]
unsafe fn try_init_exclusive_aligned(
    device: *mut c_void,
    client: &mut *mut c_void,
    wfx: &WAVEFORMATEX,
    period_hns: i64,
) -> i32 {
    if client.is_null() {
        match activate_audio_client_from_device(device) {
            Ok(c) => *client = c,
            Err(_) => return -1,
        }
    }
    let cli_vtbl = &**(*client as *mut *const IAudioClientVtbl);
    let hr = (cli_vtbl.initialize)(
        *client,
        AUDCLNT_SHAREMODE_EXCLUSIVE,
        AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
        period_hns,
        period_hns,
        wfx,
        ptr::null(),
    );
    if hr != AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED {
        return hr;
    }
    // Hardware requested a specific aligned buffer size (Microsoft WASAPI Exclusive specification)
    let mut aligned_frames: u32 = 0;
    let gbs_hr = (cli_vtbl.get_buffer_size)(*client, &mut aligned_frames);
    (cli_vtbl.release)(*client);
    *client = ptr::null_mut();
    if gbs_hr < 0 || aligned_frames == 0 || wfx.nSamplesPerSec == 0 {
        return hr;
    }
    let aligned_period =
        ((10_000_000.0 * (aligned_frames as f64) / (wfx.nSamplesPerSec as f64)) + 0.5) as i64;
    match activate_audio_client_from_device(device) {
        Ok(fresh) => {
            *client = fresh;
            let v2 = &**(fresh as *mut *const IAudioClientVtbl);
            (v2.initialize)(
                fresh,
                AUDCLNT_SHAREMODE_EXCLUSIVE,
                AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                aligned_period,
                aligned_period,
                wfx,
                ptr::null(),
            )
        }
        Err(_) => -1,
    }
}

#[repr(C)]
pub struct IAudioClientVtbl {
    pub query_interface: unsafe extern "system" fn(*mut c_void, *const ffi::GUID, *mut *mut c_void) -> i32,
    pub add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    pub release: unsafe extern "system" fn(*mut c_void) -> u32,
    pub initialize: unsafe extern "system" fn(
        *mut c_void,
        u32,
        u32,
        i64,
        i64,
        *const WAVEFORMATEX,
        *const ffi::GUID,
    ) -> i32,
    pub get_buffer_size: unsafe extern "system" fn(*mut c_void, *mut u32) -> i32,
    pub get_stream_latency: unsafe extern "system" fn(*mut c_void, *mut i64) -> i32,
    pub get_current_padding: unsafe extern "system" fn(*mut c_void, *mut u32) -> i32,
    pub is_format_supported: unsafe extern "system" fn(
        *mut c_void,
        u32,
        *const WAVEFORMATEX,
        *mut *mut WAVEFORMATEX,
    ) -> i32,
    pub get_mix_format: unsafe extern "system" fn(*mut c_void, *mut *mut WAVEFORMATEX) -> i32,
    pub get_device_period: unsafe extern "system" fn(*mut c_void, *mut i64, *mut i64) -> i32,
    pub start: unsafe extern "system" fn(*mut c_void) -> i32,
    pub stop: unsafe extern "system" fn(*mut c_void) -> i32,
    pub reset: unsafe extern "system" fn(*mut c_void) -> i32,
    pub set_event_handle: unsafe extern "system" fn(*mut c_void, ffi::HANDLE) -> i32,
    pub get_service: unsafe extern "system" fn(*mut c_void, *const ffi::GUID, *mut *mut c_void) -> i32,
}

#[repr(C)]
pub struct IAudioCaptureClientVtbl {
    pub query_interface: unsafe extern "system" fn(*mut c_void, *const ffi::GUID, *mut *mut c_void) -> i32,
    pub add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    pub release: unsafe extern "system" fn(*mut c_void) -> u32,
    pub get_buffer: unsafe extern "system" fn(*mut c_void, *mut *mut u8, *mut u32, *mut u32, *mut u64, *mut u64) -> i32,
    pub release_buffer: unsafe extern "system" fn(*mut c_void, u32) -> i32,
    pub get_next_packet_size: unsafe extern "system" fn(*mut c_void, *mut u32) -> i32,
}

#[repr(C)]
pub struct IAudioRenderClientVtbl {
    pub query_interface: unsafe extern "system" fn(*mut c_void, *const ffi::GUID, *mut *mut c_void) -> i32,
    pub add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    pub release: unsafe extern "system" fn(*mut c_void) -> u32,
    pub get_buffer: unsafe extern "system" fn(*mut c_void, u32, *mut *mut u8) -> i32,
    pub release_buffer: unsafe extern "system" fn(*mut c_void, u32, u32) -> i32,
}

pub struct WasapiExclusivePair {
    stop_flag: Arc<AtomicBool>,
    in_thread: Option<JoinHandle<()>>,
    out_thread: Option<JoinHandle<()>>,
    pub active_sample_rate: u32,
    pub active_in_buf_frames: u32,
    pub active_out_buf_frames: u32,
}

unsafe impl Send for WasapiExclusivePair {}
unsafe impl Sync for WasapiExclusivePair {}

impl WasapiExclusivePair {
    /// Coba buka perangkat Input & Output dalam mode WASAPI Exclusive (Event-Driven).
    pub fn try_start<FIn, FOut>(
        in_target: impl Into<WasapiEndpointTarget>,
        out_target: impl Into<WasapiEndpointTarget>,
        sample_rate: u32,
        buffer_size: u32,
        mut in_cb: FIn,
        mut out_cb: FOut,
    ) -> Result<Self, String>
    where
        FIn: FnMut(&[f32], usize, usize) + Send + 'static,
        FOut: FnMut(&mut [f32], usize, usize) + Send + 'static,
    {
        let in_target = in_target.into();
        let out_target = out_target.into();
        #[cfg(windows)]
        {
            let stop_flag = Arc::new(AtomicBool::new(false));
            let (in_ready_tx, in_ready_rx) = std::sync::mpsc::sync_channel::<Result<(u32, u32), String>>(1);
            let (out_ready_tx, out_ready_rx) = std::sync::mpsc::sync_channel::<Result<(u32, u32), String>>(1);

            let stop_in = Arc::clone(&stop_flag);
            let in_thread = thread::Builder::new()
                .name("rinka-wasapi-excl-in".into())
                .spawn(move || {
                    run_exclusive_endpoint(
                        true,
                        in_target,
                        sample_rate,
                        buffer_size,
                        stop_in,
                        in_ready_tx,
                        move |buf, ch, frames| in_cb(buf, ch, frames),
                        |_, _, _| {},
                    );
                })
                .map_err(|e| e.to_string())?;

            let (in_sr, in_buf_frames) = match in_ready_rx.recv() {
                Ok(Ok(v)) => v,
                Ok(Err(e)) => {
                    stop_flag.store(true, Ordering::SeqCst);
                    let _ = in_thread.join();
                    return Err(format!("Exclusive Input rejected: {}", e));
                }
                Err(_) => {
                    stop_flag.store(true, Ordering::SeqCst);
                    let _ = in_thread.join();
                    return Err("Exclusive Input thread stopped".into());
                }
            };

            let stop_out = Arc::clone(&stop_flag);
            let out_thread = thread::Builder::new()
                .name("rinka-wasapi-excl-out".into())
                .spawn(move || {
                    run_exclusive_endpoint(
                        false,
                        out_target,
                        in_sr,
                        buffer_size,
                        stop_out,
                        out_ready_tx,
                        |_, _, _| {},
                        move |buf, ch, frames| out_cb(buf, ch, frames),
                    );
                })
                .map_err(|e| e.to_string())?;

            match out_ready_rx.recv() {
                Ok(Ok((_, out_buf_frames))) => Ok(Self {
                    stop_flag,
                    in_thread: Some(in_thread),
                    out_thread: Some(out_thread),
                    active_sample_rate: in_sr,
                    active_in_buf_frames: in_buf_frames,
                    active_out_buf_frames: out_buf_frames,
                }),
                Ok(Err(e)) => {
                    stop_flag.store(true, Ordering::SeqCst);
                    let _ = in_thread.join();
                    let _ = out_thread.join();
                    Err(format!("Exclusive Output rejected: {}", e))
                }
                Err(_) => {
                    stop_flag.store(true, Ordering::SeqCst);
                    let _ = in_thread.join();
                    let _ = out_thread.join();
                    Err("Exclusive Output thread stopped".into())
                }
            }
        }
        #[cfg(not(windows))]
        {
            let _ = (in_target, out_target, sample_rate, buffer_size, &mut in_cb, &mut out_cb);
            Err("WASAPI Exclusive is only available on Windows".into())
        }
    }
}

impl Drop for WasapiExclusivePair {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Some(t) = self.in_thread.take() {
            let _ = t.join();
        }
        if let Some(t) = self.out_thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
fn run_exclusive_endpoint<FIn, FOut>(
    is_capture: bool,
    target: WasapiEndpointTarget,
    requested_sr: u32,
    requested_frames: u32,
    stop_flag: Arc<AtomicBool>,
    ready_tx: std::sync::mpsc::SyncSender<Result<(u32, u32), String>>,
    mut in_cb: FIn,
    mut out_cb: FOut,
) where
    FIn: FnMut(&[f32], usize, usize),
    FOut: FnMut(&mut [f32], usize, usize),
{
    unsafe {
        let hr_com = ffi::CoInitializeEx(ptr::null_mut(), ffi::COINIT_MULTITHREADED);
        let com_ours = hr_com == 0 || hr_com == 1;
        let _ = boost_audio_thread();
        let cleanup_com = |ours: bool| {
            if ours {
                ffi::CoUninitialize();
            }
        };

        let device = match open_mmdevice(&target, is_capture) {
            Ok(d) => d,
            Err(e) => {
                let _ = ready_tx.send(Err(e));
                cleanup_com(com_ours);
                return;
            }
        };
        let dev_vtbl = &**(device as *mut *const IMMDeviceVtbl);

        let mut client = match activate_audio_client_from_device(device) {
            Ok(c) => c,
            Err(e) => {
                (dev_vtbl.release)(device);
                let _ = ready_tx.send(Err(e));
                cleanup_com(com_ours);
                return;
            }
        };
        let mut cli_vtbl = &**(client as *mut *const IAudioClientVtbl);

        let mut def_period: i64 = 0;
        let mut min_period: i64 = 0;
        let _ = (cli_vtbl.get_device_period)(client, &mut def_period, &mut min_period);
        let req_period = ((requested_frames as f64 / requested_sr.max(8000) as f64) * 10_000_000.0) as i64;
        let period = req_period.max(min_period.max(20_000)); // min 2ms

        // Coba inisialisasi Exclusive Mode: 32-bit Float Stereo -> 16-bit PCM Stereo
        // dengan re-aktivasi fresh IAudioClient & penanganan AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED
        let mut chosen_wfx = WAVEFORMATEX {
            wFormatTag: 3, // WAVE_FORMAT_IEEE_FLOAT
            nChannels: 2,
            nSamplesPerSec: requested_sr,
            nAvgBytesPerSec: requested_sr * 8,
            nBlockAlign: 8,
            wBitsPerSample: 32,
            cbSize: 0,
        };

        let mut init_hr = try_init_exclusive_aligned(device, &mut client, &chosen_wfx, period);

        if init_hr < 0 {
            if !client.is_null() {
                let v = &**(client as *mut *const IAudioClientVtbl);
                (v.release)(client);
                client = ptr::null_mut();
            }
            // Fallback ke 16-bit PCM Stereo di Exclusive Mode pada IAudioClient baru
            chosen_wfx = WAVEFORMATEX {
                wFormatTag: 1, // WAVE_FORMAT_PCM
                nChannels: 2,
                nSamplesPerSec: requested_sr,
                nAvgBytesPerSec: requested_sr * 4,
                nBlockAlign: 4,
                wBitsPerSample: 16,
                cbSize: 0,
            };
            init_hr = try_init_exclusive_aligned(device, &mut client, &chosen_wfx, period);
        }

        (dev_vtbl.release)(device);

        if init_hr < 0 || client.is_null() {
            if !client.is_null() {
                let v = &**(client as *mut *const IAudioClientVtbl);
                (v.release)(client);
            }
            let _ = ready_tx.send(Err(format!("Hardware rejected WASAPI Exclusive (0x{:08X})", init_hr)));
            cleanup_com(com_ours);
            return;
        }
        cli_vtbl = &**(client as *mut *const IAudioClientVtbl);

        let h_event = ffi::CreateEventW(ptr::null_mut(), 0, 0, ptr::null());
        if h_event.is_null() || (cli_vtbl.set_event_handle)(client, h_event) < 0 {
            if !h_event.is_null() {
                ffi::CloseHandle(h_event);
            }
            (cli_vtbl.release)(client);
            let _ = ready_tx.send(Err("SetEventHandle failed".into()));
            cleanup_com(com_ours);
            return;
        }

        let mut buf_frames: u32 = 0;
        let gbs_hr = (cli_vtbl.get_buffer_size)(client, &mut buf_frames);
        if gbs_hr < 0 || buf_frames == 0 {
            ffi::CloseHandle(h_event);
            (cli_vtbl.release)(client);
            let _ = ready_tx.send(Err(format!("GetBufferSize failed (0x{:08X}, frames={})", gbs_hr, buf_frames)));
            cleanup_com(com_ours);
            return;
        }
        let ch = chosen_wfx.nChannels as usize;
        let is_float = chosen_wfx.wFormatTag == 3;
        let mut scratch = vec![0.0f32; (buf_frames as usize).max(4096) * ch];

        if is_capture {
            let mut cap_client: *mut c_void = ptr::null_mut();
            if (cli_vtbl.get_service)(client, &IID_IAUDIOCAPTURECLIENT, &mut cap_client) < 0 || cap_client.is_null() {
                ffi::CloseHandle(h_event);
                (cli_vtbl.release)(client);
                let _ = ready_tx.send(Err("GetService IAudioCaptureClient failed".into()));
                cleanup_com(com_ours);
                return;
            }
            let cap_vtbl = &**(cap_client as *mut *const IAudioCaptureClientVtbl);
            if (cli_vtbl.start)(client) < 0 {
                (cap_vtbl.release)(cap_client);
                ffi::CloseHandle(h_event);
                (cli_vtbl.release)(client);
                let _ = ready_tx.send(Err("IAudioClient::Start failed".into()));
                cleanup_com(com_ours);
                return;
            }

            let _ = ready_tx.send(Ok((chosen_wfx.nSamplesPerSec, buf_frames)));
            log_info(format!(
                "[WASAPI Exclusive] Capture aktif @ {}Hz ({}-bit, buf={} smp)",
                chosen_wfx.nSamplesPerSec, chosen_wfx.wBitsPerSample, buf_frames
            ));

            while !stop_flag.load(Ordering::Relaxed) {
                let w = ffi::WaitForSingleObject(h_event, 200);
                if w == ffi::WAIT_OBJECT_0 {
                    let mut p_data: *mut u8 = ptr::null_mut();
                    let mut frames_avail: u32 = 0;
                    let mut flags: u32 = 0;
                    if (cap_vtbl.get_buffer)(
                        cap_client,
                        &mut p_data,
                        &mut frames_avail,
                        &mut flags,
                        ptr::null_mut(),
                        ptr::null_mut(),
                    ) >= 0
                        && frames_avail > 0
                    {
                        let n = frames_avail as usize;
                        let total_smp = n * ch;
                        if total_smp > scratch.len() {
                            scratch.resize(total_smp, 0.0);
                        }
                        if (flags & 2) != 0 || p_data.is_null() {
                            scratch[..total_smp].fill(0.0);
                        } else if is_float {
                            ptr::copy_nonoverlapping(
                                p_data,
                                scratch.as_mut_ptr() as *mut u8,
                                total_smp * std::mem::size_of::<f32>(),
                            );
                        } else {
                            let src_ptr = p_data as *const i16;
                            for (i, dst) in scratch[..total_smp].iter_mut().enumerate() {
                                let s = ptr::read_unaligned(src_ptr.add(i));
                                *dst = (s as f32) / 32768.0;
                            }
                        }
                        let _ = (cap_vtbl.release_buffer)(cap_client, frames_avail);
                        in_cb(&scratch[..total_smp], ch, n);
                    }
                }
            }

            let _ = (cli_vtbl.stop)(client);
            (cap_vtbl.release)(cap_client);
        } else {
            let mut ren_client: *mut c_void = ptr::null_mut();
            if (cli_vtbl.get_service)(client, &IID_IAUDIORENDERCLIENT, &mut ren_client) < 0 || ren_client.is_null() {
                ffi::CloseHandle(h_event);
                (cli_vtbl.release)(client);
                let _ = ready_tx.send(Err("GetService IAudioRenderClient failed".into()));
                cleanup_com(com_ours);
                return;
            }
            let ren_vtbl = &**(ren_client as *mut *const IAudioRenderClientVtbl);

            // Prime buffer awal dengan silence
            let mut p_init: *mut u8 = ptr::null_mut();
            if (ren_vtbl.get_buffer)(ren_client, buf_frames, &mut p_init) >= 0 {
                let _ = (ren_vtbl.release_buffer)(ren_client, buf_frames, 2); // AUDCLNT_BUFFERFLAGS_SILENT
            }

            if (cli_vtbl.start)(client) < 0 {
                (ren_vtbl.release)(ren_client);
                ffi::CloseHandle(h_event);
                (cli_vtbl.release)(client);
                let _ = ready_tx.send(Err("IAudioClient::Start Output failed".into()));
                cleanup_com(com_ours);
                return;
            }

            let _ = ready_tx.send(Ok((chosen_wfx.nSamplesPerSec, buf_frames)));
            log_info(format!(
                "[WASAPI Exclusive] Render aktif @ {}Hz ({}-bit, buf={} smp)",
                chosen_wfx.nSamplesPerSec, chosen_wfx.wBitsPerSample, buf_frames
            ));

            while !stop_flag.load(Ordering::Relaxed) {
                let w = ffi::WaitForSingleObject(h_event, 200);
                if w == ffi::WAIT_OBJECT_0 {
                    let mut p_data: *mut u8 = ptr::null_mut();
                    let n = buf_frames as usize;
                    let total_smp = n * ch;
                    if total_smp > scratch.len() {
                        scratch.resize(total_smp, 0.0);
                    }
                    if (ren_vtbl.get_buffer)(ren_client, buf_frames, &mut p_data) >= 0 && !p_data.is_null() {
                        out_cb(&mut scratch[..total_smp], ch, n);
                        if is_float {
                            ptr::copy_nonoverlapping(
                                scratch.as_ptr() as *const u8,
                                p_data,
                                total_smp * std::mem::size_of::<f32>(),
                            );
                        } else {
                            let dst_ptr = p_data as *mut i16;
                            for (i, &src) in scratch[..total_smp].iter().enumerate() {
                                let v = (src.clamp(-1.0, 1.0) * 32767.0) as i16;
                                ptr::write_unaligned(dst_ptr.add(i), v);
                            }
                        }
                        let _ = (ren_vtbl.release_buffer)(ren_client, buf_frames, 0);
                    }
                }
            }

            let _ = (cli_vtbl.stop)(client);
            (ren_vtbl.release)(ren_client);
        }

        ffi::CloseHandle(h_event);
        (cli_vtbl.release)(client);
        cleanup_com(com_ours);
    }
}

/// Mesin Hibrida `ASIO Experimental (Flex-Bridge)`:
/// Mengubah semua perangkat Input & Output WASAPI (Line A In -> Out, Line A Out -> Line B In, Line B Out)
/// menjadi aliran Event-Driven berlatensi rendah bergaya FlexASIO yang mendukung:
/// - Loopback Hardware/Software (seperti `SSL Output 1/2` -> `Loopback 1 (Output 1/2)`) tanpa mengunci
///   mixer shared Windows!
/// - Perangkat Mono, Stereo, maupun Multi-Channel (`1..16` kanal) melalui deteksi otomatis `GetMixFormat`.
/// - Pengisian buffer penuh bebas underrun di semua ukuran `smp` (`32..1024 smp`).
#[derive(Clone, Copy, Debug)]
pub struct FlexAsioEndpointOptions {
    pub set_latency_ms: Option<f32>,
    pub wasapi_set: bool,
    pub wasapi_exclusive: bool,
    pub wasapi_autoconvert: bool,
    pub channels: u16,
}

impl Default for FlexAsioEndpointOptions {
    fn default() -> Self {
        Self {
            set_latency_ms: Some(0.0),
            wasapi_set: true,
            wasapi_exclusive: false,
            wasapi_autoconvert: true,
            channels: 0,
        }
    }
}

pub struct WasapiFlexAsioPair {
    stop_flag: Arc<AtomicBool>,
    in_thread: Option<JoinHandle<()>>,
    out_thread: Option<JoinHandle<()>>,
    pub active_sample_rate: u32,
    pub in_mode_tag: String,
    pub out_mode_tag: String,
    pub actual_in_buf: u32,
    pub actual_out_buf: u32,
    pub actual_in_hw_frames: u32,
    pub actual_out_hw_frames: u32,
}

unsafe impl Send for WasapiFlexAsioPair {}
unsafe impl Sync for WasapiFlexAsioPair {}

impl WasapiFlexAsioPair {
    pub fn try_start<FIn, FOut>(
        in_target: Option<WasapiEndpointTarget>,
        out_target: Option<WasapiEndpointTarget>,
        sample_rate: u32,
        buffer_size: u32,
        set_in_latency_ms: Option<f32>,
        set_out_latency_ms: Option<f32>,
        in_cb: FIn,
        out_cb: FOut,
    ) -> Result<Self, String>
    where
        FIn: FnMut(&[f32], usize, usize) + Send + 'static,
        FOut: FnMut(&mut [f32], usize, usize) + Send + 'static,
    {
        let in_opts = FlexAsioEndpointOptions {
            set_latency_ms: set_in_latency_ms,
            ..Default::default()
        };
        let out_opts = FlexAsioEndpointOptions {
            set_latency_ms: set_out_latency_ms,
            ..Default::default()
        };
        Self::try_start_with_options(
            in_target,
            out_target,
            sample_rate,
            buffer_size,
            in_opts,
            out_opts,
            in_cb,
            out_cb,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn try_start_with_options<FIn, FOut>(
        in_target: Option<WasapiEndpointTarget>,
        out_target: Option<WasapiEndpointTarget>,
        sample_rate: u32,
        buffer_size: u32,
        in_opts: FlexAsioEndpointOptions,
        out_opts: FlexAsioEndpointOptions,
        mut in_cb: FIn,
        mut out_cb: FOut,
    ) -> Result<Self, String>
    where
        FIn: FnMut(&[f32], usize, usize) + Send + 'static,
        FOut: FnMut(&mut [f32], usize, usize) + Send + 'static,
    {
        #[cfg(windows)]
        {
            if in_target.is_none() && out_target.is_none() {
                return Err("At least one physical device must be selected".into());
            }
            let stop_flag = Arc::new(AtomicBool::new(false));

            // KASUS 1: Output adalah Virtual Bus (`ASIO Line A Out`), sehingga hanya Input fisik yang berjalan
            if let (Some(tgt_in), None) = (&in_target, &out_target) {
                let tgt_in = tgt_in.clone();
                let (in_ready_tx, in_ready_rx) =
                    std::sync::mpsc::sync_channel::<Result<(u32, u32, u32, String), String>>(1);
                let stop_in = Arc::clone(&stop_flag);
                let t = thread::Builder::new()
                    .name("rinka-flex-asio-in-to-vbus".into())
                    .spawn(move || {
                        run_flex_asio_endpoint(
                            true,
                            tgt_in,
                            sample_rate,
                            buffer_size,
                            in_opts,
                            stop_in,
                            in_ready_tx,
                            move |buf, ch, frames| {
                                in_cb(buf, ch, frames);
                            },
                            |_, _, _| {},
                        );
                    })
                    .map_err(|e| e.to_string())?;

                return match in_ready_rx.recv() {
                    Ok(Ok((sr, b_in, hw_in, tag))) => Ok(Self {
                        stop_flag,
                        in_thread: Some(t),
                        out_thread: None,
                        active_sample_rate: sr,
                        in_mode_tag: tag,
                        out_mode_tag: "ASIO-LineA-Out".to_string(),
                        actual_in_buf: b_in,
                        actual_out_buf: b_in,
                        actual_in_hw_frames: hw_in,
                        actual_out_hw_frames: 0,
                    }),
                    Ok(Err(e)) => {
                        stop_flag.store(true, Ordering::SeqCst);
                        let _ = t.join();
                        Err(format!("Flex-ASIO Input failed: {}", e))
                    }
                    Err(_) => {
                        stop_flag.store(true, Ordering::SeqCst);
                        let _ = t.join();
                        Err("Flex-ASIO Input thread stopped".into())
                    }
                };
            }

            // KASUS 2: Output adalah perangkat fisik (dengan Input fisik atau Input Virtual `ASIO Line B In`)
            let Some(tgt_out) = out_target else {
                return Err("Output device target not found".into());
            };
            let has_in = in_target.is_some();
            let (in_thread, in_sr, in_tag, act_in_buf, act_in_hw) = if let Some(tgt_in) = in_target {
                let (in_ready_tx, in_ready_rx) =
                    std::sync::mpsc::sync_channel::<Result<(u32, u32, u32, String), String>>(1);
                let stop_in = Arc::clone(&stop_flag);
                let t = thread::Builder::new()
                    .name("rinka-flex-asio-in".into())
                    .spawn(move || {
                        run_flex_asio_endpoint(
                            true,
                            tgt_in,
                            sample_rate,
                            buffer_size,
                            in_opts,
                            stop_in,
                            in_ready_tx,
                            move |buf, ch, frames| in_cb(buf, ch, frames),
                            |_, _, _| {},
                        );
                    })
                    .map_err(|e| e.to_string())?;

                match in_ready_rx.recv() {
                    Ok(Ok((sr, b_in, hw_in, tag))) => (Some(t), sr, tag, b_in, hw_in),
                    Ok(Err(e)) => {
                        stop_flag.store(true, Ordering::SeqCst);
                        let _ = t.join();
                        return Err(format!("Flex-ASIO Input failed: {}", e));
                    }
                    Err(_) => {
                        stop_flag.store(true, Ordering::SeqCst);
                        let _ = t.join();
                        return Err("Flex-ASIO Input thread stopped".into());
                    }
                }
            } else {
                (None, sample_rate, "ASIO-LineB-In".to_string(), buffer_size, 0)
            };

            let (out_ready_tx, out_ready_rx) =
                std::sync::mpsc::sync_channel::<Result<(u32, u32, u32, String), String>>(1);
            let stop_out = Arc::clone(&stop_flag);
            let out_thread = thread::Builder::new()
                .name("rinka-flex-asio-out".into())
                .spawn(move || {
                    run_flex_asio_endpoint(
                        false,
                        tgt_out,
                        in_sr,
                        buffer_size,
                        out_opts,
                        stop_out,
                        out_ready_tx,
                        |_, _, _| {},
                        move |buf, ch, frames| out_cb(buf, ch, frames),
                    );
                })
                .map_err(|e| e.to_string())?;

            match out_ready_rx.recv() {
                Ok(Ok((out_sr, act_out_buf, act_out_hw, out_tag))) => Ok(Self {
                    stop_flag,
                    in_thread,
                    out_thread: Some(out_thread),
                    active_sample_rate: if has_in { in_sr } else { out_sr },
                    in_mode_tag: in_tag,
                    out_mode_tag: out_tag,
                    actual_in_buf: if has_in { act_in_buf } else { act_out_buf },
                    actual_out_buf: act_out_buf,
                    actual_in_hw_frames: act_in_hw,
                    actual_out_hw_frames: act_out_hw,
                }),
                Ok(Err(e)) => {
                    stop_flag.store(true, Ordering::SeqCst);
                    if let Some(t) = in_thread {
                        let _ = t.join();
                    }
                    let _ = out_thread.join();
                    Err(format!("Flex-ASIO Output failed: {}", e))
                }
                Err(_) => {
                    stop_flag.store(true, Ordering::SeqCst);
                    if let Some(t) = in_thread {
                        let _ = t.join();
                    }
                    let _ = out_thread.join();
                    Err("Flex-ASIO Output thread stopped".into())
                }
            }
        }
        #[cfg(not(windows))]
        {
            let _ = (
                in_target,
                out_target,
                sample_rate,
                buffer_size,
                in_opts,
                out_opts,
                &mut in_cb,
                &mut out_cb,
            );
            Err("Flex-ASIO is only available on Windows".into())
        }
    }
}

impl Drop for WasapiFlexAsioPair {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Some(t) = self.in_thread.take() {
            let _ = t.join();
        }
        if let Some(t) = self.out_thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
fn run_flex_asio_endpoint<FIn, FOut>(
    is_capture: bool,
    target: WasapiEndpointTarget,
    requested_sr: u32,
    requested_frames: u32,
    opts: FlexAsioEndpointOptions,
    stop_flag: Arc<AtomicBool>,
    ready_tx: std::sync::mpsc::SyncSender<Result<(u32, u32, u32, String), String>>,
    mut in_cb: FIn,
    mut out_cb: FOut,
) where
    FIn: FnMut(&[f32], usize, usize),
    FOut: FnMut(&mut [f32], usize, usize),
{
    unsafe {
        let hr_com = ffi::CoInitializeEx(ptr::null_mut(), ffi::COINIT_MULTITHREADED);
        let com_ours = hr_com == 0 || hr_com == 1;
        let _ = boost_audio_thread();
        let cleanup_com = |ours: bool| {
            if ours {
                ffi::CoUninitialize();
            }
        };

        let device = match open_mmdevice(&target, is_capture) {
            Ok(d) => d,
            Err(e) => {
                let _ = ready_tx.send(Err(e));
                cleanup_com(com_ours);
                return;
            }
        };
        let dev_vtbl = &**(device as *mut *const IMMDeviceVtbl);

        let mut client = match activate_audio_client_from_device(device) {
            Ok(c) => c,
            Err(e) => {
                (dev_vtbl.release)(device);
                let _ = ready_tx.send(Err(e));
                cleanup_com(com_ours);
                return;
            }
        };
        let mut cli_vtbl = &**(client as *mut *const IAudioClientVtbl);

        let mut def_period: i64 = 0;
        let mut min_period: i64 = 0;
        let _ = (cli_vtbl.get_device_period)(client, &mut def_period, &mut min_period);
        if def_period <= 0 {
            def_period = 100_000; // 10 ms default
        }
        if min_period <= 0 {
            min_period = 30_000; // 3 ms default
        }

        // Periksa jumlah kanal & sample rate native perangkat dari GetMixFormat
        // agar jika SR & kanal sudah cocok, kita dapat memakai jalur Bit-Exact Direct (tanpa SRC APO Windows)!
        let mut native_channels: u16 = 2;
        let mut native_sr: u32 = requested_sr;
        let mut mix_fmt_ptr: *mut WAVEFORMATEX = ptr::null_mut();
        if (cli_vtbl.get_mix_format)(client, &mut mix_fmt_ptr) >= 0 && !mix_fmt_ptr.is_null() {
            native_channels = (*mix_fmt_ptr).nChannels.clamp(1, 16);
            native_sr = (*mix_fmt_ptr).nSamplesPerSec.clamp(8000, 384000);
            ffi::CoTaskMemFree(mix_fmt_ptr as *const c_void);
        }

        let smp_period =
            ((requested_frames.max(32) as f64 / requested_sr.max(44100) as f64) * 10_000_000.0) as i64;

        let set_latency_ms = opts.set_latency_ms;
        let extra_padding_frames = match set_latency_ms {
            Some(lat_ms) if lat_ms > 0.05 => {
                let eff_ms = if lat_ms <= 0.5 {
                    (lat_ms as f64 * 10.0).min(10.0)
                } else {
                    (lat_ms as f64).min(40.0)
                };
                ((eff_ms / 1000.0) * requested_sr as f64) as u32
            }
            _ => 0,
        };

        let shared_buf_duration = match set_latency_ms {
            Some(lat_ms) if lat_ms > 10.0 => {
                ((lat_ms.clamp(10.0, 250.0) as f64) * 10_000.0) as i64
            }
            _ => def_period.max(smp_period).clamp(100_000, 150_000),
        };
        let use_autoconvert = !opts.wasapi_set || opts.wasapi_autoconvert;
        let shared_flags = if use_autoconvert {
            AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY
        } else {
            AUDCLNT_STREAMFLAGS_EVENTCALLBACK
        };
        let auto_flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK
            | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
            | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;

        let make_float_wfx = |ch_count: u16, sr: u32| -> WAVEFORMATEX {
            let block_align = ch_count * 4;
            WAVEFORMATEX {
                wFormatTag: 3, // WAVE_FORMAT_IEEE_FLOAT
                nChannels: ch_count,
                nSamplesPerSec: sr,
                nAvgBytesPerSec: sr * (block_align as u32),
                nBlockAlign: block_align,
                wBitsPerSample: 32,
                cbSize: 0,
            }
        };

        let req_ch = if opts.channels > 0 {
            opts.channels.clamp(1, 16)
        } else if is_capture && native_channels == 1 {
            1
        } else {
            2
        };

        let mut chosen_wfx = make_float_wfx(req_ch, requested_sr);
        let mut init_hr: i32 = -1;
        let mut exclusive_active = false;

        let reopen_fresh_client = |cur_client: &mut *mut c_void| -> Result<&'static IAudioClientVtbl, String> {
            if !cur_client.is_null() {
                let v = &**(*cur_client as *mut *const IAudioClientVtbl);
                (v.release)(*cur_client);
                *cur_client = ptr::null_mut();
            }
            let c = activate_audio_client_from_device(device)?;
            *cur_client = c;
            Ok(&**(c as *mut *const IAudioClientVtbl))
        };

        // Jika user mencentang WASAPI Set + Exclusive di ASIO Setting, coba mode Exclusive terlebih dahulu
        if opts.wasapi_set && opts.wasapi_exclusive {
            let excl_period = smp_period.max(min_period.max(20_000));
            init_hr = try_init_exclusive_aligned(device, &mut client, &chosen_wfx, excl_period);
            if init_hr >= 0 {
                exclusive_active = true;
            } else {
                if let Ok(vt) = reopen_fresh_client(&mut client) {
                    cli_vtbl = vt;
                    let pcm16_wfx = WAVEFORMATEX {
                        wFormatTag: 1,
                        nChannels: req_ch,
                        nSamplesPerSec: requested_sr,
                        nAvgBytesPerSec: requested_sr * (req_ch as u32 * 2),
                        nBlockAlign: req_ch * 2,
                        wBitsPerSample: 16,
                        cbSize: 0,
                    };
                    init_hr = try_init_exclusive_aligned(device, &mut client, &pcm16_wfx, excl_period);
                    if init_hr >= 0 {
                        chosen_wfx = pcm16_wfx;
                        exclusive_active = true;
                    } else if let Ok(vt2) = reopen_fresh_client(&mut client) {
                        cli_vtbl = vt2;
                    }
                }
            }
            if !client.is_null() {
                cli_vtbl = &**(client as *mut *const IAudioClientVtbl);
            }
        }

        // Jika sample rate & jumlah kanal sama persis dengan MixFormat native Windows,
        // utamakan inisialisasi Direct Event-Driven murni (tanpa AUTOCONVERTPCM / tanpa filter SRC Windows)
        if init_hr < 0 && !client.is_null() && native_sr == requested_sr && req_ch == native_channels {
            chosen_wfx = make_float_wfx(req_ch, requested_sr);
            init_hr = (cli_vtbl.initialize)(
                client,
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                shared_buf_duration,
                0,
                &chosen_wfx,
                ptr::null(),
            );
            if init_hr < 0 {
                if let Ok(vt) = reopen_fresh_client(&mut client) {
                    cli_vtbl = vt;
                }
            }
        }

        if init_hr < 0 && !client.is_null() {
            chosen_wfx = make_float_wfx(req_ch, requested_sr);
            init_hr = (cli_vtbl.initialize)(
                client,
                AUDCLNT_SHAREMODE_SHARED,
                shared_flags,
                shared_buf_duration,
                0,
                &chosen_wfx,
                ptr::null(),
            );
        }

        if init_hr < 0 && !use_autoconvert {
            if let Ok(vt) = reopen_fresh_client(&mut client) {
                cli_vtbl = vt;
                init_hr = (cli_vtbl.initialize)(
                    client,
                    AUDCLNT_SHAREMODE_SHARED,
                    auto_flags,
                    shared_buf_duration,
                    0,
                    &chosen_wfx,
                    ptr::null(),
                );
            }
        }

        if init_hr < 0 && native_channels != 2 {
            if let Ok(vt) = reopen_fresh_client(&mut client) {
                cli_vtbl = vt;
                chosen_wfx = make_float_wfx(native_channels, requested_sr);
                init_hr = (cli_vtbl.initialize)(
                    client,
                    AUDCLNT_SHAREMODE_SHARED,
                    shared_flags,
                    shared_buf_duration,
                    0,
                    &chosen_wfx,
                    ptr::null(),
                );
            }
        }

        if init_hr < 0 {
            if let Ok(vt) = reopen_fresh_client(&mut client) {
                cli_vtbl = vt;
                chosen_wfx = make_float_wfx(native_channels, requested_sr);
                init_hr = (cli_vtbl.initialize)(
                    client,
                    AUDCLNT_SHAREMODE_SHARED,
                    shared_flags,
                    0,
                    0,
                    &chosen_wfx,
                    ptr::null(),
                );
            }
        }

        (dev_vtbl.release)(device);

        if init_hr < 0 || client.is_null() {
            if !client.is_null() {
                let v = &**(client as *mut *const IAudioClientVtbl);
                (v.release)(client);
            }
            let _ = ready_tx.send(Err(format!("Failed to initialize Flex-ASIO (0x{:08X})", init_hr)));
            cleanup_com(com_ours);
            return;
        }
        cli_vtbl = &**(client as *mut *const IAudioClientVtbl);

        let h_event = ffi::CreateEventW(ptr::null_mut(), 0, 0, ptr::null());
        if h_event.is_null() || (cli_vtbl.set_event_handle)(client, h_event) < 0 {
            if !h_event.is_null() {
                ffi::CloseHandle(h_event);
            }
            (cli_vtbl.release)(client);
            let _ = ready_tx.send(Err("Flex-ASIO SetEventHandle failed".into()));
            cleanup_com(com_ours);
            return;
        }

        let mut buf_frames: u32 = 0;
        let gbs_hr = (cli_vtbl.get_buffer_size)(client, &mut buf_frames);
        if gbs_hr < 0 || buf_frames == 0 {
            ffi::CloseHandle(h_event);
            (cli_vtbl.release)(client);
            let _ = ready_tx.send(Err(format!("Flex-ASIO GetBufferSize failed (0x{:08X}, frames={})", gbs_hr, buf_frames)));
            cleanup_com(com_ours);
            return;
        }
        let ch = chosen_wfx.nChannels.max(1) as usize;
        let is_float = chosen_wfx.wFormatTag == 3;
        let target_quantum = requested_frames.clamp(32, 1024);
        let def_period_frames = (((def_period as f64) / 10_000_000.0)
            * (chosen_wfx.nSamplesPerSec as f64))
            .round()
            .max(128.0) as u32;
        let eff_quantum = target_quantum.min(buf_frames.max(32));
        let mode_tag = if exclusive_active {
            format!("Flex-ASIO Excl {}smp", buf_frames)
        } else {
            format!("Flex-ASIO {}smp", eff_quantum)
        };
        let mut scratch = vec![0.0f32; (buf_frames as usize).max(4096) * ch];
        let mut stereo_scratch = vec![0.0f32; (buf_frames as usize).max(4096) * 2];

        if is_capture {
            let mut cap_client: *mut c_void = ptr::null_mut();
            if (cli_vtbl.get_service)(client, &IID_IAUDIOCAPTURECLIENT, &mut cap_client) < 0
                || cap_client.is_null()
            {
                ffi::CloseHandle(h_event);
                (cli_vtbl.release)(client);
                let _ = ready_tx.send(Err("GetService IAudioCaptureClient failed".into()));
                cleanup_com(com_ours);
                return;
            }
            let cap_vtbl = &**(cap_client as *mut *const IAudioCaptureClientVtbl);
            if (cli_vtbl.start)(client) < 0 {
                (cap_vtbl.release)(cap_client);
                ffi::CloseHandle(h_event);
                (cli_vtbl.release)(client);
                let _ = ready_tx.send(Err("IAudioClient::Start Capture failed".into()));
                cleanup_com(com_ours);
                return;
            }

            let hw_cap_frames = if exclusive_active { buf_frames } else { def_period_frames };
            let _ = ready_tx.send(Ok((chosen_wfx.nSamplesPerSec, eff_quantum, hw_cap_frames, mode_tag.clone())));
            log_info(format!(
                "[ASIO Experimental] Capture ({}) aktif @ {}Hz ({} ch, smp={}, hw_frames={}, set_in={:?})",
                mode_tag, chosen_wfx.nSamplesPerSec, ch, eff_quantum, hw_cap_frames, set_latency_ms
            ));

            while !stop_flag.load(Ordering::Relaxed) {
                let w = ffi::WaitForSingleObject(h_event, 50);
                if w == ffi::WAIT_OBJECT_0 {
                    loop {
                        let mut p_data: *mut u8 = ptr::null_mut();
                        let mut frames_avail: u32 = 0;
                        let mut flags: u32 = 0;
                        if (cap_vtbl.get_buffer)(
                            cap_client,
                            &mut p_data,
                            &mut frames_avail,
                            &mut flags,
                            ptr::null_mut(),
                            ptr::null_mut(),
                        ) < 0
                            || frames_avail == 0
                        {
                            break;
                        }
                        let n = frames_avail as usize;
                        let total_smp = n * ch;
                        if total_smp > scratch.len() {
                            scratch.resize(total_smp, 0.0);
                        }
                        if (flags & 2) != 0 || p_data.is_null() {
                            scratch[..total_smp].fill(0.0);
                        } else if is_float {
                            ptr::copy_nonoverlapping(
                                p_data,
                                scratch.as_mut_ptr() as *mut u8,
                                total_smp * std::mem::size_of::<f32>(),
                            );
                        } else {
                            let src_ptr = p_data as *const i16;
                            for (i, dst) in scratch[..total_smp].iter_mut().enumerate() {
                                let s = ptr::read_unaligned(src_ptr.add(i));
                                *dst = (s as f32) / 32768.0;
                            }
                        }
                        let _ = (cap_vtbl.release_buffer)(cap_client, frames_avail);

                        in_cb(&scratch[..total_smp], ch, n);
                    }
                }
            }

            let _ = (cli_vtbl.stop)(client);
            (cap_vtbl.release)(cap_client);
        } else {
            let mut ren_client: *mut c_void = ptr::null_mut();
            if (cli_vtbl.get_service)(client, &IID_IAUDIORENDERCLIENT, &mut ren_client) < 0
                || ren_client.is_null()
            {
                ffi::CloseHandle(h_event);
                (cli_vtbl.release)(client);
                let _ = ready_tx.send(Err("GetService IAudioRenderClient failed".into()));
                cleanup_com(com_ours);
                return;
            }
            let ren_vtbl = &**(ren_client as *mut *const IAudioRenderClientVtbl);

            let safety_headroom = (eff_quantum / 2).clamp(128, 256);
            let min_safe_fill = if exclusive_active {
                buf_frames
            } else {
                (def_period_frames + safety_headroom)
                    .max(eff_quantum + safety_headroom)
                    .min(buf_frames.max(256))
            };
            let desired_fill = if exclusive_active {
                buf_frames
            } else {
                min_safe_fill + extra_padding_frames.min(def_period_frames)
            };
            let max_fill_target = if exclusive_active {
                buf_frames
            } else {
                desired_fill.clamp(min_safe_fill, buf_frames.max(min_safe_fill))
            };

            // Exclusive Event-Driven mode REQUIRES priming the full `buf_frames` buffer before Start().
            let init_prime = if exclusive_active {
                buf_frames
            } else {
                max_fill_target.min(buf_frames)
            };
            if init_prime > 0 {
                let mut p_init: *mut u8 = ptr::null_mut();
                if (ren_vtbl.get_buffer)(ren_client, init_prime, &mut p_init) >= 0 {
                    let _ = (ren_vtbl.release_buffer)(ren_client, init_prime, 2); // AUDCLNT_BUFFERFLAGS_SILENT
                }
            }

            if (cli_vtbl.start)(client) < 0 {
                (ren_vtbl.release)(ren_client);
                ffi::CloseHandle(h_event);
                (cli_vtbl.release)(client);
                let _ = ready_tx.send(Err("IAudioClient::Start Render failed".into()));
                cleanup_com(com_ours);
                return;
            }

            let hw_ren_frames = if exclusive_active { buf_frames } else { max_fill_target.min(buf_frames) };
            let _ = ready_tx.send(Ok((chosen_wfx.nSamplesPerSec, eff_quantum, hw_ren_frames, mode_tag.clone())));
            log_info(format!(
                "[ASIO Experimental] Render ({}) aktif @ {}Hz ({} ch, smp={}, target_fill={}, set_out={:?})",
                mode_tag, chosen_wfx.nSamplesPerSec, ch, eff_quantum, hw_ren_frames, set_latency_ms
            ));

            while !stop_flag.load(Ordering::Relaxed) {
                let w = ffi::WaitForSingleObject(h_event, 50);
                if w == ffi::WAIT_OBJECT_0 {
                    // Exclusive Event-Driven mode always requires GetBuffer(buf_frames) (full buffer);
                    // Shared Event-Driven mode fills up to `max_fill_target`.
                    let frames_to_write = if exclusive_active {
                        buf_frames
                    } else {
                        let mut padding: u32 = 0;
                        let _ = (cli_vtbl.get_current_padding)(client, &mut padding);
                        let avail_space = buf_frames.saturating_sub(padding);
                        let needed = max_fill_target.saturating_sub(padding);
                        needed.min(avail_space)
                    };

                    if frames_to_write > 0 {
                        let mut p_data: *mut u8 = ptr::null_mut();
                        let n = frames_to_write as usize;
                        let total_smp = n * ch;
                        if total_smp > scratch.len() {
                            scratch.resize(total_smp, 0.0);
                        }
                        if (ren_vtbl.get_buffer)(ren_client, frames_to_write, &mut p_data) >= 0
                            && !p_data.is_null()
                        {
                            if ch == 2 {
                                out_cb(&mut scratch[..total_smp], 2, n);
                            } else {
                                let st_len = n * 2;
                                if st_len > stereo_scratch.len() {
                                    stereo_scratch.resize(st_len, 0.0);
                                }
                                out_cb(&mut stereo_scratch[..st_len], 2, n);
                                for i in 0..n {
                                    let l = stereo_scratch[i * 2];
                                    let r = stereo_scratch[i * 2 + 1];
                                    if ch == 1 {
                                        scratch[i] = 0.5 * (l + r);
                                    } else {
                                        scratch[i * ch] = l;
                                        scratch[i * ch + 1] = r;
                                        for c_idx in 2..ch {
                                            scratch[i * ch + c_idx] = 0.0;
                                        }
                                    }
                                }
                            }

                            if is_float {
                                ptr::copy_nonoverlapping(
                                    scratch.as_ptr() as *const u8,
                                    p_data,
                                    total_smp * std::mem::size_of::<f32>(),
                                );
                            } else {
                                let dst_ptr = p_data as *mut i16;
                                for (i, &src) in scratch[..total_smp].iter().enumerate() {
                                    let v = (src.clamp(-1.0, 1.0) * 32767.0) as i16;
                                    ptr::write_unaligned(dst_ptr.add(i), v);
                                }
                            }
                            let _ = (ren_vtbl.release_buffer)(ren_client, frames_to_write, 0);
                        }
                    }
                }
            }

            let _ = (cli_vtbl.stop)(client);
            (ren_vtbl.release)(ren_client);
        }

        ffi::CloseHandle(h_event);
        (cli_vtbl.release)(client);
        cleanup_com(com_ours);
    }
}
