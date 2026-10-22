//! Win32 Native Interop (64-bit Windows)
//! Mencakup:
//! - MMCSS "Pro Audio" & `ABOVE_NORMAL_PRIORITY_CLASS`
//! - Windows 11 EcoQoS / Efficiency Mode Disabler (`PROCESS_POWER_THROTTLING_EXECUTION_SPEED`)
//! - Hardware CPU Denormal/Subnormal Flush (`FTZ` / `DAZ` pada register SSE/AVX `MXCSR`)
//! - Single-Instance Mutex (`Local\RinkaSoundBridge_SingleInstance`)
//! - System Tray Win32 (`Shell_NotifyIconW`)
//! - Registry & COM Helpers untuk ASIO Native & WASAPI Exclusive

use parking_lot::Mutex;
use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

#[cfg(windows)]
#[allow(non_snake_case, non_camel_case_types, non_upper_case_globals, dead_code)]
pub mod ffi {
    use std::ffi::c_void;

    pub type HANDLE = *mut c_void;
    pub type HWND = isize;
    pub type HINSTANCE = isize;
    pub type HMODULE = isize;
    pub type HICON = isize;
    pub type HMENU = isize;
    pub type HBRUSH = isize;
    pub type HCURSOR = isize;
    pub type HKEY = isize;
    pub type DWORD = u32;
    pub type UINT = u32;
    pub type ULONG = u32;
    pub type BOOL = i32;
    pub type LONG = i32;
    pub type HRESULT = i32;
    pub type WPARAM = usize;
    pub type LPARAM = isize;
    pub type LRESULT = isize;
    pub type LPCWSTR = *const u16;
    pub type LPWSTR = *mut u16;

    pub const INVALID_HANDLE_VALUE: HANDLE = -1isize as HANDLE;
    pub const ERROR_ALREADY_EXISTS: DWORD = 183;
    pub const ERROR_SUCCESS: LONG = 0;
    pub const ABOVE_NORMAL_PRIORITY_CLASS: DWORD = 0x00008000;
    pub const THREAD_PRIORITY_HIGHEST: i32 = 2;
    pub const THREAD_PRIORITY_TIME_CRITICAL: i32 = 15;

    pub const PROCESS_POWER_THROTTLING_CURRENT_VERSION: ULONG = 1;
    pub const PROCESS_POWER_THROTTLING_EXECUTION_SPEED: ULONG = 0x1;
    pub const ProcessPowerThrottling: i32 = 4;
    pub const ThreadPowerThrottling: i32 = 11;

    pub const HKEY_CURRENT_USER: HKEY = -2147483647isize; // 0x80000001
    pub const HKEY_LOCAL_MACHINE: HKEY = -2147483646isize; // 0x80000002
    pub const KEY_READ: DWORD = 0x20019;
    pub const KEY_WRITE: DWORD = 0x20006;
    pub const KEY_ALL_ACCESS: DWORD = 0xF003F;
    pub const REG_SZ: DWORD = 1;
    pub const PAGE_READWRITE: DWORD = 0x04;
    pub const FILE_MAP_ALL_ACCESS: DWORD = 0xF001F;

    pub const COINIT_APARTMENTTHREADED: DWORD = 0x2;
    pub const COINIT_MULTITHREADED: DWORD = 0x0;
    pub const CLSCTX_INPROC_SERVER: DWORD = 0x1;
    pub const CLSCTX_ALL: DWORD = 0x17;

    pub const WAIT_OBJECT_0: DWORD = 0x00000000;
    pub const WAIT_TIMEOUT: DWORD = 0x00000102;

    pub const WS_VISIBLE: DWORD = 0x10000000;
    pub const WS_OVERLAPPEDWINDOW: DWORD = 0x00CF0000;

    pub const SW_HIDE: i32 = 0;
    pub const SW_SHOWNORMAL: i32 = 1;
    pub const SW_SHOW: i32 = 5;
    pub const SW_SHOWNA: i32 = 8;
    pub const SW_RESTORE: i32 = 9;

    pub const WM_NULL: UINT = 0x0000;
    pub const WM_CLOSE: UINT = 0x0010;
    pub const WM_CONTEXTMENU: UINT = 0x007B;
    pub const WM_USER: UINT = 0x0400;
    pub const WM_TRAYICON: UINT = WM_USER + 144;
    pub const WM_LBUTTONUP: UINT = 0x0202;
    pub const WM_LBUTTONDBLCLK: UINT = 0x0203;
    pub const WM_RBUTTONUP: UINT = 0x0205;

    pub const IMAGE_ICON: UINT = 1;
    pub const LR_LOADFROMFILE: UINT = 0x00000010;
    pub const LR_DEFAULTSIZE: UINT = 0x00000040;
    pub const TPM_RIGHTBUTTON: UINT = 0x0002;
    pub const TPM_NONOTIFY: UINT = 0x0080;

    pub const NIM_ADD: DWORD = 0x00000000;
    pub const NIM_MODIFY: DWORD = 0x00000001;
    pub const NIM_DELETE: DWORD = 0x00000002;
    pub const NIF_MESSAGE: UINT = 0x00000001;
    pub const NIF_ICON: UINT = 0x00000002;
    pub const NIF_TIP: UINT = 0x00000004;
    pub const NIF_INFO: UINT = 0x00000010;
    pub const NIIF_INFO: DWORD = 0x00000001;

    pub const IDI_APPLICATION: LPCWSTR = 32512 as LPCWSTR;
    pub const IDC_ARROW: LPCWSTR = 32512 as LPCWSTR;
    pub const TPM_BOTTOMALIGN: UINT = 0x0020;
    pub const TPM_LEFTALIGN: UINT = 0x0000;
    pub const TPM_RETURNCMD: UINT = 0x0100;
    pub const MF_STRING: UINT = 0x0000;
    pub const MF_SEPARATOR: UINT = 0x0800;
    pub const PM_REMOVE: UINT = 0x0001;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct RECT {
        pub left: LONG,
        pub top: LONG,
        pub right: LONG,
        pub bottom: LONG,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct POINT {
        pub x: LONG,
        pub y: LONG,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
    pub struct GUID {
        pub data1: u32,
        pub data2: u16,
        pub data3: u16,
        pub data4: [u8; 8],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct MSG {
        pub hwnd: HWND,
        pub message: UINT,
        pub wParam: WPARAM,
        pub lParam: LPARAM,
        pub time: DWORD,
        pub pt: POINT,
    }

    #[repr(C)]
    pub struct PROCESS_POWER_THROTTLING_STATE {
        pub Version: ULONG,
        pub ControlMask: ULONG,
        pub StateMask: ULONG,
    }

    pub type WNDPROC = Option<unsafe extern "system" fn(HWND, UINT, WPARAM, LPARAM) -> LRESULT>;

    #[repr(C)]
    pub struct WNDCLASSEXW {
        pub cbSize: UINT,
        pub style: UINT,
        pub lpfnWndProc: WNDPROC,
        pub cbClsExtra: i32,
        pub cbWndExtra: i32,
        pub hInstance: HINSTANCE,
        pub hIcon: HICON,
        pub hCursor: HCURSOR,
        pub hbrBackground: HBRUSH,
        pub lpszMenuName: LPCWSTR,
        pub lpszClassName: LPCWSTR,
        pub hIconSm: HICON,
    }

    #[repr(C)]
    pub struct NOTIFYICONDATAW {
        pub cbSize: DWORD,
        pub hWnd: HWND,
        pub uID: UINT,
        pub uFlags: UINT,
        pub uCallbackMessage: UINT,
        pub hIcon: HICON,
        pub szTip: [u16; 128],
        pub dwState: DWORD,
        pub dwStateMask: DWORD,
        pub szInfo: [u16; 256],
        pub uTimeoutOrVersion: UINT,
        pub szInfoTitle: [u16; 64],
        pub dwInfoFlags: DWORD,
        pub guidItem: GUID,
        pub hBalloonIcon: HICON,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct FILETIME {
        pub dwLowDateTime: DWORD,
        pub dwHighDateTime: DWORD,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct PROCESS_MEMORY_COUNTERS {
        pub cb: DWORD,
        pub PageFaultCount: DWORD,
        pub PeakWorkingSetSize: usize,
        pub WorkingSetSize: usize,
        pub QuotaPeakPagedPoolUsage: usize,
        pub QuotaPagedPoolUsage: usize,
        pub QuotaPeakNonPagedPoolUsage: usize,
        pub QuotaNonPagedPoolUsage: usize,
        pub PagefileUsage: usize,
        pub PeakPagefileUsage: usize,
    }


    pub const GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS: u32 = 0x00000004;
    pub const GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT: u32 = 0x00000002;
    pub const EXCEPTION_EXECUTE_HANDLER: i32 = 1;

    #[repr(C)]
    pub struct EXCEPTION_RECORD {
        pub ExceptionCode: u32,
        pub ExceptionFlags: u32,
        pub ExceptionRecord: *mut EXCEPTION_RECORD,
        pub ExceptionAddress: *mut c_void,
        pub NumberParameters: u32,
        pub ExceptionInformation: [usize; 15],
    }

    #[repr(C, align(16))]
    pub struct CONTEXT_X64 {
        pub P1Home: u64,
        pub P2Home: u64,
        pub P3Home: u64,
        pub P4Home: u64,
        pub P5Home: u64,
        pub P6Home: u64,
        pub ContextFlags: u32,
        pub MxCsr: u32,
        pub SegCs: u16,
        pub SegDs: u16,
        pub SegEs: u16,
        pub SegFs: u16,
        pub SegGs: u16,
        pub SegSs: u16,
        pub EFlags: u32,
        pub Dr0: u64,
        pub Dr1: u64,
        pub Dr2: u64,
        pub Dr3: u64,
        pub Dr6: u64,
        pub Dr7: u64,
        pub Rax: u64,
        pub Rcx: u64,
        pub Rdx: u64,
        pub Rbx: u64,
        pub Rsp: u64,
        pub Rbp: u64,
        pub Rsi: u64,
        pub Rdi: u64,
        pub R8: u64,
        pub R9: u64,
        pub R10: u64,
        pub R11: u64,
        pub R12: u64,
        pub R13: u64,
        pub R14: u64,
        pub R15: u64,
        pub Rip: u64,
        pub _rest: [u8; 976],
    }

    #[repr(C)]
    pub struct EXCEPTION_POINTERS {
        pub ExceptionRecord: *mut EXCEPTION_RECORD,
        pub ContextRecord: *mut CONTEXT_X64,
    }

    pub type LPTOP_LEVEL_EXCEPTION_FILTER =
        Option<unsafe extern "system" fn(exception_info: *mut EXCEPTION_POINTERS) -> i32>;

    #[link(name = "kernel32")]
    extern "system" {
        pub fn GetCurrentProcess() -> HANDLE;
        pub fn GetCurrentThreadId() -> DWORD;
        pub fn AddVectoredExceptionHandler(
            First: u32,
            Handler: LPTOP_LEVEL_EXCEPTION_FILTER,
        ) -> HANDLE;
        pub fn SetUnhandledExceptionFilter(
            lpTopLevelExceptionFilter: LPTOP_LEVEL_EXCEPTION_FILTER,
        ) -> LPTOP_LEVEL_EXCEPTION_FILTER;
        pub fn GetModuleHandleExW(
            dwFlags: DWORD,
            lpModuleName: *const u16,
            phModule: *mut HINSTANCE,
        ) -> BOOL;
        pub fn GetModuleFileNameW(
            hModule: HINSTANCE,
            lpFilename: *mut u16,
            nSize: DWORD,
        ) -> DWORD;
        pub fn GetCurrentThread() -> HANDLE;
        pub fn GetLastError() -> DWORD;
        pub fn SetPriorityClass(hProcess: HANDLE, dwPriorityClass: DWORD) -> BOOL;
        pub fn SetThreadPriority(hThread: HANDLE, nPriority: i32) -> BOOL;
        pub fn GetProcessAffinityMask(
            hProcess: HANDLE,
            lpProcessAffinityMask: *mut usize,
            lpSystemAffinityMask: *mut usize,
        ) -> BOOL;
        pub fn SetThreadAffinityMask(hThread: HANDLE, dwThreadAffinityMask: usize) -> usize;
        pub fn SetProcessInformation(
            hProcess: HANDLE,
            ProcessInformationClass: i32,
            ProcessInformation: *const c_void,
            ProcessInformationSize: DWORD,
        ) -> BOOL;
        pub fn SetThreadInformation(
            hThread: HANDLE,
            ThreadInformationClass: i32,
            ThreadInformation: *const c_void,
            ThreadInformationSize: DWORD,
        ) -> BOOL;
        pub fn CreateMutexW(lpMutexAttributes: *mut c_void, bInitialOwner: BOOL, lpName: LPCWSTR) -> HANDLE;
        pub fn CreateEventW(
            lpEventAttributes: *mut c_void,
            bManualReset: BOOL,
            bInitialState: BOOL,
            lpName: LPCWSTR,
        ) -> HANDLE;
        pub fn CreateWaitableTimerExW(
            lpTimerAttributes: *mut c_void,
            lpTimerName: LPCWSTR,
            dwFlags: DWORD,
            dwDesiredAccess: DWORD,
        ) -> HANDLE;
        pub fn SetWaitableTimer(
            hTimer: HANDLE,
            lpDueTime: *const i64,
            lPeriod: i32,
            pfnCompletionRoutine: *mut c_void,
            lpArgToCompletionRoutine: *mut c_void,
            fResume: BOOL,
        ) -> BOOL;
        pub fn SetEvent(hEvent: HANDLE) -> BOOL;
        pub fn WaitForSingleObject(hHandle: HANDLE, dwMilliseconds: DWORD) -> DWORD;
        pub fn CloseHandle(hObject: HANDLE) -> BOOL;
        pub fn GetModuleHandleW(lpModuleName: LPCWSTR) -> HINSTANCE;
        pub fn LoadLibraryW(lpLibFileName: LPCWSTR) -> HINSTANCE;
        pub fn FreeLibrary(hLibModule: HINSTANCE) -> BOOL;
        pub fn GetProcAddress(hModule: HINSTANCE, lpProcName: *const u8) -> *mut c_void;
        pub fn Sleep(dwMilliseconds: DWORD);
        pub fn CreateFileMappingW(
            hFile: HANDLE,
            lpFileMappingAttributes: *mut c_void,
            flProtect: DWORD,
            dwMaximumSizeHigh: DWORD,
            dwMaximumSizeLow: DWORD,
            lpName: LPCWSTR,
        ) -> HANDLE;
        pub fn MapViewOfFile(
            hFileMappingObject: HANDLE,
            dwDesiredAccess: DWORD,
            dwFileOffsetHigh: DWORD,
            dwFileOffsetLow: DWORD,
            dwNumberOfBytesToMap: usize,
        ) -> *mut c_void;
        pub fn UnmapViewOfFile(lpBaseAddress: *const c_void) -> BOOL;
        pub fn GetProcessTimes(
            hProcess: HANDLE,
            lpCreationTime: *mut FILETIME,
            lpExitTime: *mut FILETIME,
            lpKernelTime: *mut FILETIME,
            lpUserTime: *mut FILETIME,
        ) -> BOOL;
        pub fn GetSystemTimeAsFileTime(lpSystemTimeAsFileTime: *mut FILETIME);
        pub fn K32EmptyWorkingSet(hProcess: HANDLE) -> BOOL;
        pub fn K32GetProcessMemoryInfo(
            Process: HANDLE,
            ppsmemCounters: *mut PROCESS_MEMORY_COUNTERS,
            cb: DWORD,
        ) -> BOOL;
        pub fn OpenProcess(
            dwDesiredAccess: DWORD,
            bInheritHandle: BOOL,
            dwProcessId: DWORD,
        ) -> HANDLE;
        pub fn VirtualAllocEx(
            hProcess: HANDLE,
            lpAddress: *mut c_void,
            dwSize: usize,
            flAllocationType: DWORD,
            flProtect: DWORD,
        ) -> *mut c_void;
        pub fn VirtualFreeEx(
            hProcess: HANDLE,
            lpAddress: *mut c_void,
            dwSize: usize,
            dwFreeType: DWORD,
        ) -> BOOL;
        pub fn WriteProcessMemory(
            hProcess: HANDLE,
            lpBaseAddress: *mut c_void,
            lpBuffer: *const c_void,
            nSize: usize,
            lpNumberOfBytesWritten: *mut usize,
        ) -> BOOL;
        pub fn CreateRemoteThread(
            hProcess: HANDLE,
            lpThreadAttributes: *mut c_void,
            dwStackSize: usize,
            lpStartAddress: *const c_void,
            lpParameter: *mut c_void,
            dwCreationFlags: DWORD,
            lpThreadId: *mut DWORD,
        ) -> HANDLE;
    }

    #[link(name = "user32")]
    extern "system" {
        pub fn IsWindowVisible(hWnd: HWND) -> BOOL;
        pub fn IsIconic(hWnd: HWND) -> BOOL;
        pub fn GetWindowRect(hWnd: HWND, lpRect: *mut RECT) -> BOOL;
        pub fn SetWindowPos(
            hWnd: HWND,
            hWndInsertAfter: HWND,
            X: i32,
            Y: i32,
            cx: i32,
            cy: i32,
            uFlags: UINT,
        ) -> BOOL;
        pub fn GetSystemMetrics(nIndex: i32) -> i32;
        pub fn ShowWindow(hWnd: HWND, nCmdShow: i32) -> BOOL;
        pub fn SetForegroundWindow(hWnd: HWND) -> BOOL;
        pub fn FindWindowW(lpClassName: LPCWSTR, lpWindowName: LPCWSTR) -> HWND;
        pub fn GetForegroundWindow() -> HWND;
        pub fn RegisterClassExW(lpWndClass: *const WNDCLASSEXW) -> u16;
        pub fn CreateWindowExW(
            dwExStyle: DWORD,
            lpClassName: LPCWSTR,
            lpWindowName: LPCWSTR,
            dwStyle: DWORD,
            X: i32,
            Y: i32,
            nWidth: i32,
            nHeight: i32,
            hWndParent: HWND,
            hMenu: HMENU,
            hInstance: HINSTANCE,
            lpParam: *mut c_void,
        ) -> HWND;
        pub fn DestroyWindow(hWnd: HWND) -> BOOL;
        pub fn DefWindowProcW(hWnd: HWND, Msg: UINT, wParam: WPARAM, lParam: LPARAM) -> LRESULT;
        pub fn PeekMessageW(lpMsg: *mut MSG, hWnd: HWND, wMsgFilterMin: UINT, wMsgFilterMax: UINT, wRemoveMsg: UINT) -> BOOL;
        pub fn TranslateMessage(lpMsg: *const MSG) -> BOOL;
        pub fn DispatchMessageW(lpMsg: *const MSG) -> LRESULT;
        pub fn LoadIconW(hInstance: HINSTANCE, lpIconName: LPCWSTR) -> HICON;
        pub fn LoadImageW(
            hInst: HINSTANCE,
            name: LPCWSTR,
            type_: UINT,
            cx: i32,
            cy: i32,
            fuLoad: UINT,
        ) -> HICON;
        pub fn CreateIcon(
            hInstance: HINSTANCE,
            nWidth: i32,
            nHeight: i32,
            cPlanes: u8,
            cBitsPixel: u8,
            lpbANDbits: *const u8,
            lpbXORbits: *const u8,
        ) -> HICON;
        pub fn PostMessageW(hWnd: HWND, Msg: UINT, wParam: WPARAM, lParam: LPARAM) -> BOOL;
        pub fn SendMessageW(hWnd: HWND, Msg: UINT, wParam: WPARAM, lParam: LPARAM) -> LRESULT;
        pub fn BringWindowToTop(hWnd: HWND) -> BOOL;
        pub fn InvalidateRect(hWnd: HWND, lpRect: *const RECT, bErase: BOOL) -> BOOL;
        pub fn GetWindowThreadProcessId(hWnd: HWND, lpdwProcessId: *mut DWORD) -> DWORD;
        pub fn LoadCursorW(hInstance: HINSTANCE, lpCursorName: LPCWSTR) -> HCURSOR;
        pub fn CreatePopupMenu() -> HMENU;
        pub fn AppendMenuW(hMenu: HMENU, uFlags: UINT, uIDNewItem: usize, lpNewItem: LPCWSTR) -> BOOL;
        pub fn TrackPopupMenu(
            hMenu: HMENU,
            uFlags: UINT,
            x: i32,
            y: i32,
            nReserved: i32,
            hWnd: HWND,
            prcRect: *const RECT,
        ) -> BOOL;
        pub fn DestroyMenu(hMenu: HMENU) -> BOOL;
        pub fn GetCursorPos(lpPoint: *mut POINT) -> BOOL;
        pub fn MessageBoxW(hWnd: HWND, lpText: LPCWSTR, lpCaption: LPCWSTR, uType: UINT) -> i32;
    }

    #[link(name = "shell32")]
    extern "system" {
        pub fn Shell_NotifyIconW(dwMessage: DWORD, lpData: *mut NOTIFYICONDATAW) -> BOOL;
        pub fn ShellExecuteW(
            hwnd: HWND,
            lpOperation: LPCWSTR,
            lpFile: LPCWSTR,
            lpParameters: LPCWSTR,
            lpDirectory: LPCWSTR,
            nShowCmd: i32,
        ) -> HINSTANCE;
    }

    #[link(name = "opengl32")]
    extern "system" {
        pub fn wglGetCurrentDC() -> HANDLE;
        pub fn wglGetCurrentContext() -> HANDLE;
        pub fn wglMakeCurrent(hdc: HANDLE, hglrc: HANDLE) -> BOOL;
    }

    #[link(name = "avrt")]
    extern "system" {
        pub fn AvSetMmThreadCharacteristicsW(TaskName: LPCWSTR, TaskIndex: *mut DWORD) -> HANDLE;
        pub fn AvSetMmThreadPriority(AvrtHandle: HANDLE, Priority: i32) -> BOOL;
    }

    #[link(name = "winmm")]
    extern "system" {
        pub fn timeBeginPeriod(uPeriod: UINT) -> UINT;
    }

    #[link(name = "advapi32")]
    extern "system" {
        pub fn RegOpenKeyExW(
            hKey: HKEY,
            lpSubKey: LPCWSTR,
            ulOptions: DWORD,
            samDesired: DWORD,
            phkResult: *mut HKEY,
        ) -> LONG;
        pub fn RegEnumKeyExW(
            hKey: HKEY,
            dwIndex: DWORD,
            lpName: LPWSTR,
            lpcchName: *mut DWORD,
            lpReserved: *mut DWORD,
            lpClass: LPWSTR,
            lpcchClass: *mut DWORD,
            lpftLastWriteTime: *mut FILETIME,
        ) -> LONG;
        pub fn RegQueryValueExW(
            hKey: HKEY,
            lpValueName: LPCWSTR,
            lpReserved: *mut DWORD,
            lpType: *mut DWORD,
            lpData: *mut u8,
            lpcbData: *mut DWORD,
        ) -> LONG;
        pub fn RegCreateKeyExW(
            hKey: HKEY,
            lpSubKey: LPCWSTR,
            Reserved: DWORD,
            lpClass: LPWSTR,
            dwOptions: DWORD,
            samDesired: DWORD,
            lpSecurityAttributes: *mut c_void,
            phkResult: *mut HKEY,
            lpdwDisposition: *mut DWORD,
        ) -> LONG;
        pub fn RegSetValueExW(
            hKey: HKEY,
            lpValueName: LPCWSTR,
            Reserved: DWORD,
            dwType: DWORD,
            lpData: *const u8,
            cbData: DWORD,
        ) -> LONG;
        pub fn RegDeleteKeyW(hKey: HKEY, lpSubKey: LPCWSTR) -> LONG;
        pub fn RegCloseKey(hKey: HKEY) -> LONG;
    }

    #[link(name = "ole32")]
    extern "system" {
        pub fn CoInitializeEx(pvReserved: *mut c_void, dwCoInit: DWORD) -> HRESULT;
        pub fn CoUninitialize();
        pub fn CLSIDFromString(lpsz: LPCWSTR, pclsid: *mut GUID) -> HRESULT;
        pub fn CoCreateInstance(
            rclsid: *const GUID,
            pUnkOuter: *mut c_void,
            dwClsContext: DWORD,
            riid: *const GUID,
            ppv: *mut *mut c_void,
        ) -> HRESULT;
        pub fn CoTaskMemFree(pv: *const c_void);
    }
}

pub static AUDIO_PRIORITY_BOOST: AtomicBool = AtomicBool::new(true);
static ASIO_CPU_CORES: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(2);

pub fn set_asio_cpu_cores(cores: u16) {
    ASIO_CPU_CORES.store(cores.min(64), Ordering::Relaxed);
}

pub fn get_asio_cpu_cores() -> u16 {
    ASIO_CPU_CORES.load(Ordering::Relaxed)
}

pub fn get_logical_cpu_count() -> u16 {
    std::thread::available_parallelism()
        .map(|n| n.get().clamp(1, 64) as u16)
        .unwrap_or(8)
}

/// Mengatur CPU Core Affinity pada thread audio internal Rinka SoundBridge:
/// - Di dalam proses host Voice Changer eksternal (`is_vc_host_process == true`, mis. `Vonovox.exe`),
///   JANGAN PERNAH membatasi CPU core affinity (`proc_mask` penuh tetap dipakai) agar thread pool
///   PyTorch/DirectML/CUDA/RMVPE milik Vonovox tidak pernah mengalami CPU starvation atau tambahan stall.
/// - Di dalam `RinkaSoundBridge.exe` (`is_vc_host_process == false`):
///   - `num_cores == 0`: Memakai seluruh core CPU yang diizinkan OS (`proc_mask`).
///   - `num_cores > 0`: Memilih sejumlah `num_cores` core (melewati Core 0 jika jumlah core mencukupi).
pub fn apply_asio_cpu_affinity(num_cores: u16, is_vc_host_process: bool) {
    #[cfg(windows)]
    unsafe {
        let mut proc_mask: usize = 0;
        let mut sys_mask: usize = 0;
        if ffi::GetProcessAffinityMask(ffi::GetCurrentProcess(), &mut proc_mask, &mut sys_mask) == 0
            || proc_mask == 0
        {
            return;
        }
        if is_vc_host_process || num_cores == 0 {
            let _ = ffi::SetThreadAffinityMask(ffi::GetCurrentThread(), proc_mask);
            return;
        }
        let wanted = (num_cores.max(1)) as usize;
        let mut avail_bits: Vec<usize> = Vec::with_capacity(64);
        for bit in 0..(std::mem::size_of::<usize>() * 8) {
            if (proc_mask & (1usize << bit)) != 0 {
                avail_bits.push(bit);
            }
        }
        if avail_bits.len() <= wanted {
            let _ = ffi::SetThreadAffinityMask(ffi::GetCurrentThread(), proc_mask);
            return;
        }
        let start_idx = if avail_bits.len() > wanted { 1 } else { 0 };
        let mut target_mask: usize = 0;
        for &bit in avail_bits.iter().skip(start_idx).take(wanted) {
            target_mask |= 1usize << bit;
        }
        if target_mask != 0 {
            let _ = ffi::SetThreadAffinityMask(ffi::GetCurrentThread(), target_mask);
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (num_cores, is_vc_host_process);
    }
}
static SINGLE_INSTANCE_MUTEX: AtomicIsize = AtomicIsize::new(0);
static TIMER_1MS_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Kunci resolusi timer scheduler kernel Windows ke 1.0 ms (mencegah Sleep/Wait meleset hingga 15.6 ms).
#[inline]
pub fn ensure_1ms_timer_resolution() {
    #[cfg(windows)]
    unsafe {
        if !TIMER_1MS_INITIALIZED.swap(true, Ordering::Relaxed) {
            let _ = ffi::timeBeginPeriod(1);
        }
    }
}

pub fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn set_audio_priority_boost(flag: bool) {
    AUDIO_PRIORITY_BOOST.store(flag, Ordering::Relaxed);
}

pub fn get_audio_priority_boost() -> bool {
    AUDIO_PRIORITY_BOOST.load(Ordering::Relaxed)
}

/// Aktifkan Flush-To-Zero (FTZ) dan Denormals-Are-Zero (DAZ) pada register hardware CPU x86_64.
/// Mencegah lonjakan pemakaian CPU DSP (hingga 20x lipat) saat ekor filter IIR/Reverb/ONNX VST3 mendekati nol (hening).
#[inline]
#[allow(deprecated)]
pub fn enable_cpu_denormal_flush() {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        use core::arch::x86_64::{_mm_getcsr, _mm_setcsr};
        let csr = _mm_getcsr();
        // Bit 15 (0x8000) = FTZ (Flush-To-Zero), Bit 6 (0x0040) = DAZ (Denormals-Are-Zero)
        // Bits 7..12 (0x1F80) = Mask semua exception floating-point hardware (mencegah crash jika DLL VST mengubah MXCSR)
        // Clear bits 0..5 (!0x003F) = Bersihkan sticky exception flags
        let target = (csr | 0x8040 | 0x1F80) & !0x003F;
        if csr != target {
            _mm_setcsr(target);
        }
    }
}

/// Matikan Windows 11 Power Throttling (EcoQoS / Efficiency Mode) agar proses & thread audio
/// tidak dipindahkan ke E-Core lambat ketika jendela di-minimize ke System Tray saat bermain game.
pub fn disable_windows_power_throttling() {
    #[cfg(windows)]
    unsafe {
        let state = ffi::PROCESS_POWER_THROTTLING_STATE {
            Version: ffi::PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: ffi::PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
            StateMask: 0, // 0 = Matikan throttling
        };
        let sz = std::mem::size_of::<ffi::PROCESS_POWER_THROTTLING_STATE>() as u32;
        let _ = ffi::SetProcessInformation(
            ffi::GetCurrentProcess(),
            ffi::ProcessPowerThrottling,
            &state as *const _ as *const c_void,
            sz,
        );
        let _ = ffi::SetThreadInformation(
            ffi::GetCurrentThread(),
            ffi::ThreadPowerThrottling,
            &state as *const _ as *const c_void,
            sz,
        );
    }
}

/// Daftarkan thread callback audio saat ini ke MMCSS 'Pro Audio', matikan EcoQoS throttling,
/// dan aktifkan FTZ/DAZ denormal protection.
pub fn boost_audio_thread() -> Option<&'static str> {
    enable_cpu_denormal_flush();
    ensure_1ms_timer_resolution();
    disable_windows_power_throttling();
    apply_asio_cpu_affinity(get_asio_cpu_cores(), false);
    #[cfg(windows)]
    unsafe {
        let _ = ffi::CoInitializeEx(std::ptr::null_mut(), ffi::COINIT_MULTITHREADED);
    }

    if !AUDIO_PRIORITY_BOOST.load(Ordering::Relaxed) {
        return None;
    }
    #[cfg(windows)]
    unsafe {
        let task = to_wide("Pro Audio");
        let mut task_idx: ffi::DWORD = 0;
        let h = ffi::AvSetMmThreadCharacteristicsW(task.as_ptr(), &mut task_idx);
        if !h.is_null() {
            // AVRT_PRIORITY_HIGH = 1 (prioritas tinggi MMCSS Pro Audio yang kooperatif dengan Audiodg.exe & GPU driver)
            let _ = ffi::AvSetMmThreadPriority(h, 1);
            return Some("MMCSS Pro Audio (High) + HighRes Timer + FTZ/DAZ");
        }
        if ffi::SetThreadPriority(ffi::GetCurrentThread(), ffi::THREAD_PRIORITY_HIGHEST) != 0 {
            return Some("THREAD_PRIORITY_HIGHEST + HighRes Timer + FTZ/DAZ");
        }
    }
    Some("FTZ/DAZ Active")
}

/// Daftarkan thread pump Virtual ASIO ke MMCSS 'Pro Audio' dengan prioritas NORMAL (0)
/// agar presisi tinggi tetap terjaga tanpa pernah mengambil jatah CPU dari Audiodg.exe atau GPU RVC Vonovox.
pub fn boost_virtual_asio_pump_thread() {
    boost_virtual_asio_pump_thread_with_cores(get_asio_cpu_cores());
}

pub fn boost_virtual_asio_pump_thread_with_cores(cpu_cores: u16) {
    enable_cpu_denormal_flush();
    ensure_1ms_timer_resolution();
    disable_windows_power_throttling();
    apply_asio_cpu_affinity(cpu_cores, true);
    #[cfg(windows)]
    unsafe {
        let _ = ffi::CoInitializeEx(std::ptr::null_mut(), ffi::COINIT_MULTITHREADED);
        if AUDIO_PRIORITY_BOOST.load(Ordering::Relaxed) {
            let task = to_wide("Pro Audio");
            let mut task_idx: ffi::DWORD = 0;
            let h = ffi::AvSetMmThreadCharacteristicsW(task.as_ptr(), &mut task_idx);
            if !h.is_null() {
                // AVRT_PRIORITY_NORMAL = 0 di dalam kelas Pro Audio
                let _ = ffi::AvSetMmThreadPriority(h, 0);
            }
        }
    }
}

#[cfg(windows)]
struct ThreadHighResTimer(ffi::HANDLE);

#[cfg(windows)]
impl ThreadHighResTimer {
    fn new() -> Self {
        unsafe {
            // CREATE_WAITABLE_TIMER_HIGH_RESOLUTION = 0x00000002, TIMER_ALL_ACCESS = 0x1F0003
            let mut h = ffi::CreateWaitableTimerExW(
                std::ptr::null_mut(),
                std::ptr::null(),
                0x0000_0002,
                0x001F_0003,
            );
            if h.is_null() {
                h = ffi::CreateWaitableTimerExW(
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    0,
                    0x001F_0003,
                );
            }
            Self(h)
        }
    }
}

#[cfg(windows)]
impl Drop for ThreadHighResTimer {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = ffi::CloseHandle(self.0);
            }
        }
    }
}

#[cfg(windows)]
thread_local! {
    static HIGH_RES_TIMER: ThreadHighResTimer = ThreadHighResTimer::new();
}

/// Tidur presisi sub-milidetik menggunakan Windows Kernel High-Resolution Waitable Timer
/// (`CREATE_WAITABLE_TIMER_HIGH_RESOLUTION`) dengan 0.0% penggunaan CPU (tanpa busy-spin loop!).
#[inline]
pub fn sleep_high_res(dur: std::time::Duration) {
    if dur.is_zero() {
        return;
    }
    #[cfg(windows)]
    {
        let ns = dur.as_nanos();
        if ns < 150_000 {
            return;
        }
        let ticks_100ns = (ns / 100).max(1) as i64;
        let due_time: i64 = -ticks_100ns;
        let timeout_ms = ((ns / 1_000_000) as u32).saturating_add(5).max(2);
        let slept = HIGH_RES_TIMER.with(|t| {
            if !t.0.is_null() {
                unsafe {
                    if ffi::SetWaitableTimer(
                        t.0,
                        &due_time,
                        0,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        0,
                    ) != 0
                    {
                        let _ = ffi::WaitForSingleObject(t.0, timeout_ms);
                        return true;
                    }
                }
            }
            false
        });
        if !slept {
            std::thread::sleep(dur);
        }
    }
    #[cfg(not(windows))]
    {
        std::thread::sleep(dur);
    }
}

/// Naikkan kelas prioritas proses ke ABOVE_NORMAL, kunci timer 1ms, dan matikan EcoQoS Power Throttling (Windows).
pub fn boost_process_priority() -> bool {
    ensure_1ms_timer_resolution();
    disable_windows_power_throttling();
    if !AUDIO_PRIORITY_BOOST.load(Ordering::Relaxed) {
        return false;
    }
    #[cfg(windows)]
    unsafe {
        return ffi::SetPriorityClass(ffi::GetCurrentProcess(), ffi::ABOVE_NORMAL_PRIORITY_CLASS) != 0;
    }
    #[cfg(not(windows))]
    false
}

/// Mutex bernama per sesi Windows untuk mencegah 2 instance berjalan bersamaan.
pub fn acquire_single_instance(wait_seconds: f64) -> bool {
    #[cfg(windows)]
    unsafe {
        let name = to_wide("Local\\RinkaSoundBridge_SingleInstance");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(wait_seconds);
        loop {
            let handle = ffi::CreateMutexW(std::ptr::null_mut(), 0, name.as_ptr());
            let err = ffi::GetLastError();
            if handle.is_null() {
                return true;
            }
            if err != ffi::ERROR_ALREADY_EXISTS {
                SINGLE_INSTANCE_MUTEX.store(handle as isize, Ordering::SeqCst);
                return true;
            }
            ffi::CloseHandle(handle);
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    }
    #[cfg(not(windows))]
    {
        let _ = wait_seconds;
        true
    }
}

/// Lepaskan mutex single-instance secara eksplisit saat aplikasi melakukan restart atau keluar.
pub fn release_single_instance() {
    #[cfg(windows)]
    unsafe {
        let h = SINGLE_INSTANCE_MUTEX.swap(0, Ordering::SeqCst);
        if h != 0 {
            let _ = ffi::CloseHandle(h as ffi::HANDLE);
        }
    }
}

pub fn show_info_message_box(title: &str, message: &str) {
    #[cfg(windows)]
    unsafe {
        let t = to_wide(title);
        let m = to_wide(message);
        ffi::MessageBoxW(0, m.as_ptr(), t.as_ptr(), 0x00000040);
    }
    #[cfg(not(windows))]
    {
        println!("[{}] {}", title, message);
    }
}

// ==============================================================================
// MONITOR PERFORMA PROSES NATIVE (CPU % & RAM RSS MB TANPA OVERHEAD)
// ==============================================================================

#[cfg(windows)]
fn ft_to_u64(ft: ffi::FILETIME) -> u64 {
    ((ft.dwHighDateTime as u64) << 32) | (ft.dwLowDateTime as u64)
}

pub struct ProcessPerfMonitor {
    last_sys_time: u64,
    last_proc_time: u64,
    num_cpus: u64,
}

impl ProcessPerfMonitor {
    pub fn new() -> Self {
        let num_cpus = std::thread::available_parallelism()
            .map(|n| n.get() as u64)
            .unwrap_or(1)
            .max(1);
        let mut mon = Self {
            last_sys_time: 0,
            last_proc_time: 0,
            num_cpus,
        };
        let _ = mon.sample();
        mon
    }

    pub fn sample(&mut self) -> (f32, u64) {
        #[cfg(windows)]
        unsafe {
            let hproc = ffi::GetCurrentProcess();
            let mut creat = ffi::FILETIME::default();
            let mut exit = ffi::FILETIME::default();
            let mut kern = ffi::FILETIME::default();
            let mut user = ffi::FILETIME::default();
            let mut sys_ft = ffi::FILETIME::default();

            ffi::GetSystemTimeAsFileTime(&mut sys_ft);
            let mut cpu_pct = 0.0f32;
            if ffi::GetProcessTimes(hproc, &mut creat, &mut exit, &mut kern, &mut user) != 0 {
                let sys_now = ft_to_u64(sys_ft);
                let proc_now = ft_to_u64(kern) + ft_to_u64(user);
                if self.last_sys_time > 0 && sys_now > self.last_sys_time {
                    let sys_diff = sys_now - self.last_sys_time;
                    let proc_diff = proc_now.saturating_sub(self.last_proc_time);
                    cpu_pct = ((proc_diff as f64 / sys_diff as f64) / (self.num_cpus as f64) * 100.0) as f32;
                }
                self.last_sys_time = sys_now;
                self.last_proc_time = proc_now;
            }

            let mut pmc = ffi::PROCESS_MEMORY_COUNTERS {
                cb: std::mem::size_of::<ffi::PROCESS_MEMORY_COUNTERS>() as u32,
                ..Default::default()
            };
            let ram_mb = if ffi::K32GetProcessMemoryInfo(hproc, &mut pmc, pmc.cb) != 0 {
                (pmc.WorkingSetSize / (1024 * 1024)) as u64
            } else {
                0
            };

            (cpu_pct.clamp(0.0, 100.0), ram_mb)
        }
        #[cfg(not(windows))]
        {
            (0.0, 0)
        }
    }
}

// ==============================================================================
// SYSTEM TRAY MANAGER WIN32 (`Shell_NotifyIconW`)
// ==============================================================================

static TRAY_RESTORE_REQUESTED: AtomicBool = AtomicBool::new(false);
static TRAY_QUIT_REQUESTED: AtomicBool = AtomicBool::new(false);
static TRAY_IS_ACTIVE: AtomicBool = AtomicBool::new(false);
static MAIN_WINDOW_HWND: AtomicIsize = AtomicIsize::new(0);
static TRAY_MSG_HWND: AtomicIsize = AtomicIsize::new(0);
static TRAY_HICON: AtomicIsize = AtomicIsize::new(0);

pub fn is_tray_quit_requested() -> bool {
    TRAY_QUIT_REQUESTED.load(Ordering::SeqCst)
}

/// Buat atau muat `HICON` aplikasi Rinka SoundBridge dengan urutan prioritas:
/// 1) Dari array piksel 32x32 RGBA (`APP_ICON_RGBA`) langsung di memori menggunakan `CreateIcon` (100% selalu berhasil!)
/// 2) Dari file `icon.ico` di folder proyek / `.exe` menggunakan `LoadImageW`
/// 3) Dari resource `.exe` (`MAKEINTRESOURCEW(1)`) yang ditanam oleh `winres`
/// 4) Fallback ke `IDI_APPLICATION`
#[cfg(windows)]
unsafe fn get_or_create_app_hicon(rgba_opt: Option<&[u8]>) -> ffi::HICON {
    let cached = TRAY_HICON.load(Ordering::SeqCst);
    if cached != 0 {
        return cached;
    }

    let hinst = ffi::GetModuleHandleW(std::ptr::null());
    let mut icon: ffi::HICON = 0;

    // 0. Coba ambil HICON yang sudah dipasang winit/eframe pada jendela utama (WM_GETICON = 0x007F)
    let main_hwnd = MAIN_WINDOW_HWND.load(Ordering::SeqCst);
    if main_hwnd != 0 {
        let h_small = ffi::SendMessageW(main_hwnd, 0x007F, 0, 0) as ffi::HICON;
        if h_small != 0 {
            icon = h_small;
        } else {
            let h_big = ffi::SendMessageW(main_hwnd, 0x007F, 1, 0) as ffi::HICON;
            if h_big != 0 {
                icon = h_big;
            }
        }
    }

    // 0b. Coba resource icon #1 yang ditanam oleh winres di dalam RinkaSoundBridge.exe
    if icon == 0 {
        let h = ffi::LoadIconW(hinst, 1 as ffi::LPCWSTR);
        if h != 0 {
            icon = h;
        }
    }

    // 1. Coba muat dari Modules\icon.ico atau folder utama proyek
    let base_dir = crate::logger::get_base_dir();
    let mod_ico = crate::logger::get_modules_dir().join("icon.ico");
    let root_ico = base_dir.join("icon.ico");
    if root_ico.exists() && !mod_ico.exists() && !base_dir.join("Cargo.toml").exists() {
        let _ = std::fs::rename(&root_ico, &mod_ico);
    }
    let ico_candidates = [
        mod_ico,
        root_ico,
        base_dir.join("..").join("icon.ico"),
    ];
    for p in &ico_candidates {
        if p.exists() {
            let wpath = to_wide(&p.to_string_lossy());
            let h = ffi::LoadImageW(
                0,
                wpath.as_ptr(),
                ffi::IMAGE_ICON,
                32,
                32,
                ffi::LR_LOADFROMFILE,
            );
            if h != 0 {
                icon = h;
                break;
            }
        }
    }

    // 2. Jika belum dapat, buat HICON 32x32 ARGB langsung dari piksel `APP_ICON_RGBA` di memori!
    if icon == 0 {
        if let Some(rgba) = rgba_opt {
            if rgba.len() == 32 * 32 * 4 {
                let mut bgra = [0u8; 32 * 32 * 4];
                let mut and_mask = [0u8; (32 * 32) / 8];
                for i in 0..(32 * 32) {
                    let r = rgba[i * 4];
                    let g = rgba[i * 4 + 1];
                    let b = rgba[i * 4 + 2];
                    let a = rgba[i * 4 + 3];
                    bgra[i * 4] = b;
                    bgra[i * 4 + 1] = g;
                    bgra[i * 4 + 2] = r;
                    bgra[i * 4 + 3] = a;
                    if a == 0 {
                        and_mask[i / 8] |= 0x80 >> (i % 8);
                    }
                }
                let h = ffi::CreateIcon(hinst, 32, 32, 1, 32, and_mask.as_ptr(), bgra.as_ptr());
                if h != 0 {
                    icon = h;
                }
            }
        }
    }

    // 3. Coba resource icon #1 yang ditanam oleh winres di dalam RinkaSoundBridge.exe
    if icon == 0 {
        let h = ffi::LoadIconW(hinst, 1 as ffi::LPCWSTR);
        if h != 0 {
            icon = h;
        }
    }

    // 4. Fallback terakhir
    if icon == 0 {
        icon = ffi::LoadIconW(0, ffi::IDI_APPLICATION);
    }

    if icon != 0 {
        TRAY_HICON.store(icon, Ordering::SeqCst);
    }
    icon
}

#[cfg(windows)]
unsafe fn remove_tray_icon_internal() {
    let msg_hwnd = TRAY_MSG_HWND.load(Ordering::SeqCst);
    if TRAY_IS_ACTIVE.swap(false, Ordering::SeqCst) && msg_hwnd != 0 {
        let mut nid: ffi::NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<ffi::NOTIFYICONDATAW>() as u32;
        nid.hWnd = msg_hwnd;
        nid.uID = 1001;
        let _ = ffi::Shell_NotifyIconW(ffi::NIM_DELETE, &mut nid);
    }
}

/// Pulihkan jendela utama secara LANGSUNG dari dalam `tray_wndproc`!
/// Penting: Saat jendela utama disembunyikan (`SW_HIDE`), Windows menghentikan pengiriman `WM_PAINT`
/// sehingga `eframe::App::update()` tidak berjalan. Karena itu pemulihan jendela WAJIB dieksekusi
/// langsung saat event klik tray diterima di `tray_wndproc`!
#[cfg(windows)]
unsafe fn restore_main_window_immediate() {
    remove_tray_icon_internal();
    let main_hwnd = MAIN_WINDOW_HWND.load(Ordering::SeqCst);
    if main_hwnd != 0 {
        ffi::ShowWindow(main_hwnd, ffi::SW_SHOW);
        ffi::ShowWindow(main_hwnd, ffi::SW_SHOWNORMAL);
        ffi::ShowWindow(main_hwnd, ffi::SW_RESTORE);
        ffi::SetForegroundWindow(main_hwnd);
        ffi::BringWindowToTop(main_hwnd);
        ffi::InvalidateRect(main_hwnd, std::ptr::null(), 1);
        let _ = ffi::PostMessageW(main_hwnd, ffi::WM_NULL, 0, 0);
    }
    TRAY_RESTORE_REQUESTED.store(true, Ordering::SeqCst);
}

/// Tutup aplikasi secara bersih langsung dari menu System Tray ("Quit Completely").
#[cfg(windows)]
unsafe fn quit_from_tray_immediate() {
    remove_tray_icon_internal();
    TRAY_QUIT_REQUESTED.store(true, Ordering::SeqCst);
    let main_hwnd = MAIN_WINDOW_HWND.load(Ordering::SeqCst);
    if main_hwnd != 0 {
        // Bangunkan message loop & kirim WM_CLOSE langsung ke HWND utama agar eframe menjalankan on_exit() -> clean_shutdown()
        ffi::ShowWindow(main_hwnd, ffi::SW_SHOWNA);
        ffi::InvalidateRect(main_hwnd, std::ptr::null(), 1);
        let _ = ffi::PostMessageW(main_hwnd, ffi::WM_CLOSE, 0, 0);
    }
    // Failsafe thread: jika dalam 800 ms proses belum tertutup oleh winit, tutup proses secara paksa
    // agar tidak pernah nyangkut di Task Manager!
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_millis(800));
        crate::logger::shutdown_logger();
        std::process::exit(0);
    });
}

pub struct SystemTrayManager {
    #[cfg(windows)]
    msg_hwnd: ffi::HWND,
}

impl SystemTrayManager {
    pub fn new_with_icon(rgba_32x32: &[u8]) -> Self {
        #[cfg(windows)]
        unsafe {
            let app_icon = get_or_create_app_hicon(Some(rgba_32x32));
            let class_name = to_wide("RinkaSoundBridgeTrayMsgClass");
            let hinst = ffi::GetModuleHandleW(std::ptr::null());
            let wc = ffi::WNDCLASSEXW {
                cbSize: std::mem::size_of::<ffi::WNDCLASSEXW>() as u32,
                style: 0,
                lpfnWndProc: Some(tray_wndproc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: hinst,
                hIcon: app_icon,
                hCursor: ffi::LoadCursorW(0, ffi::IDC_ARROW),
                hbrBackground: 0,
                lpszMenuName: std::ptr::null(),
                lpszClassName: class_name.as_ptr(),
                hIconSm: app_icon,
            };
            let _ = ffi::RegisterClassExW(&wc);
            let msg_hwnd = ffi::CreateWindowExW(
                0,
                class_name.as_ptr(),
                to_wide("RinkaTrayHidden").as_ptr(),
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                hinst,
                std::ptr::null_mut(),
            );
            TRAY_MSG_HWND.store(msg_hwnd, Ordering::SeqCst);
            return Self { msg_hwnd };
        }
        #[cfg(not(windows))]
        {
            let _ = rgba_32x32;
            Self {}
        }
    }

    pub fn new() -> Self {
        Self::new_with_icon(&[])
    }

    pub fn minimize_to_tray(&mut self, window_title: &str) {
        #[cfg(windows)]
        unsafe {
            let wtitle = to_wide(window_title);
            let mut main_hwnd = ffi::FindWindowW(std::ptr::null(), wtitle.as_ptr());
            if main_hwnd == 0 {
                let fg = ffi::GetForegroundWindow();
                if fg != 0 {
                    main_hwnd = fg;
                }
            }
            if main_hwnd != 0 {
                MAIN_WINDOW_HWND.store(main_hwnd, Ordering::SeqCst);
            }

            let app_icon = get_or_create_app_hicon(None);

            let mut nid: ffi::NOTIFYICONDATAW = std::mem::zeroed();
            nid.cbSize = std::mem::size_of::<ffi::NOTIFYICONDATAW>() as u32;
            nid.hWnd = self.msg_hwnd;
            nid.uID = 1001;
            nid.uFlags = ffi::NIF_MESSAGE | ffi::NIF_ICON | ffi::NIF_TIP | ffi::NIF_INFO;
            nid.uCallbackMessage = ffi::WM_TRAYICON;
            nid.hIcon = app_icon;

            for (i, c) in "Rinka SoundBridge v2.0 (Click to open)".encode_utf16().take(127).enumerate() {
                nid.szTip[i] = c;
            }
            for (i, c) in "Rinka SoundBridge v2.0".encode_utf16().take(63).enumerate() {
                nid.szInfoTitle[i] = c;
            }
            for (i, c) in "Running in System Tray. Click or right-click the tray icon to restore."
                .encode_utf16()
                .take(255)
                .enumerate()
            {
                nid.szInfo[i] = c;
            }
            nid.dwInfoFlags = ffi::NIIF_INFO;

            if TRAY_IS_ACTIVE.load(Ordering::SeqCst) {
                let _ = ffi::Shell_NotifyIconW(ffi::NIM_MODIFY, &mut nid);
            } else {
                let _ = ffi::Shell_NotifyIconW(ffi::NIM_ADD, &mut nid);
                TRAY_IS_ACTIVE.store(true, Ordering::SeqCst);
            }

            if main_hwnd != 0 {
                ffi::ShowWindow(main_hwnd, ffi::SW_HIDE);
            }
        }
        #[cfg(not(windows))]
        let _ = window_title;
    }

    pub fn poll_events(&mut self) -> (bool, bool) {
        #[cfg(windows)]
        unsafe {
            if self.msg_hwnd != 0 {
                let mut msg: ffi::MSG = std::mem::zeroed();
                while ffi::PeekMessageW(&mut msg, self.msg_hwnd, 0, 0, ffi::PM_REMOVE) != 0 {
                    ffi::TranslateMessage(&msg);
                    ffi::DispatchMessageW(&msg);
                }
            }
        }
        let restore = TRAY_RESTORE_REQUESTED.swap(false, Ordering::SeqCst);
        let quit = TRAY_QUIT_REQUESTED.swap(false, Ordering::SeqCst);
        if restore {
            self.restore_from_tray();
        }
        (restore, quit)
    }

    pub fn restore_from_tray(&mut self) {
        #[cfg(windows)]
        unsafe {
            restore_main_window_immediate();
            TRAY_RESTORE_REQUESTED.store(false, Ordering::SeqCst);
        }
    }

    pub fn remove_tray_icon(&mut self) {
        #[cfg(windows)]
        unsafe {
            remove_tray_icon_internal();
        }
    }
}

impl Drop for SystemTrayManager {
    fn drop(&mut self) {
        self.remove_tray_icon();
        #[cfg(windows)]
        unsafe {
            if self.msg_hwnd != 0 {
                ffi::DestroyWindow(self.msg_hwnd);
            }
        }
    }
}

#[cfg(windows)]
unsafe extern "system" fn tray_wndproc(
    hwnd: ffi::HWND,
    msg: ffi::UINT,
    wparam: ffi::WPARAM,
    lparam: ffi::LPARAM,
) -> ffi::LRESULT {
    if msg == ffi::WM_TRAYICON {
        let ev = (lparam & 0xFFFF) as u32;
        // Baik klik kiri 1x (WM_LBUTTONUP) maupun klik kiri 2x (WM_LBUTTONDBLCLK) langsung membuka kembali aplikasi!
        if ev == ffi::WM_LBUTTONUP || ev == ffi::WM_LBUTTONDBLCLK {
            restore_main_window_immediate();
            return 0;
        } else if ev == ffi::WM_RBUTTONUP || ev == ffi::WM_CONTEXTMENU {
            let hmenu = ffi::CreatePopupMenu();
            if hmenu != 0 {
                let open_str = to_wide("Open Rinka SoundBridge");
                let quit_str = to_wide("Quit Completely");
                ffi::AppendMenuW(hmenu, ffi::MF_STRING, 1, open_str.as_ptr());
                ffi::AppendMenuW(hmenu, ffi::MF_SEPARATOR, 0, std::ptr::null());
                ffi::AppendMenuW(hmenu, ffi::MF_STRING, 2, quit_str.as_ptr());

                let mut pt = ffi::POINT::default();
                ffi::GetCursorPos(&mut pt);
                ffi::SetForegroundWindow(hwnd);
                let cmd = ffi::TrackPopupMenu(
                    hmenu,
                    ffi::TPM_RETURNCMD
                        | ffi::TPM_NONOTIFY
                        | ffi::TPM_RIGHTBUTTON
                        | ffi::TPM_BOTTOMALIGN
                        | ffi::TPM_LEFTALIGN,
                    pt.x,
                    pt.y,
                    0,
                    hwnd,
                    std::ptr::null(),
                );
                let _ = ffi::PostMessageW(hwnd, ffi::WM_NULL, 0, 0);
                ffi::DestroyMenu(hmenu);

                if cmd == 1 {
                    restore_main_window_immediate();
                } else if cmd == 2 {
                    quit_from_tray_immediate();
                }
            }
            return 0;
        }
    }
    ffi::DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// Pangkas memori Working Set yang sudah tidak terpakai (mis. setelah inisialisasi DLL VST3 atau font)
/// agar pemakaian RAM di Windows tetap serendah mungkin.
pub fn trim_process_working_set() {
    #[cfg(windows)]
    unsafe {
        let _ = ffi::K32EmptyWorkingSet(ffi::GetCurrentProcess());
    }
}

/// Penjaga konteks OpenGL (WGL) thread utama.
/// Saat editor VST3 (seperti nih-plug / baseview / JUCE OpenGL) dibuka atau ditutup (X),
/// plugin sering memanggil `wglMakeCurrent(null, null)` yang mencabut konteks OpenGL milik `eframe`
/// dan memicu crash Access Violation pada frame berikutnya. Guard ini menyimpan dan memulihkan
/// konteks OpenGL `eframe` secara otomatis!
static HOST_GL_HDC: AtomicIsize = AtomicIsize::new(0);
static HOST_GL_HGLRC: AtomicIsize = AtomicIsize::new(0);

/// Rekam konteks OpenGL (HDC & HGLRC) asli milik `eframe` saat startup (sebelum ada VST yang dibuka).
pub fn capture_host_gl_context() {
    #[cfg(windows)]
    unsafe {
        let dc = ffi::wglGetCurrentDC() as isize;
        let ctx = ffi::wglGetCurrentContext() as isize;
        if dc != 0 && ctx != 0 {
            HOST_GL_HDC.store(dc, Ordering::SeqCst);
            HOST_GL_HGLRC.store(ctx, Ordering::SeqCst);
        }
    }
}

/// Pastikan konteks OpenGL asli milik `eframe` kembali aktif di thread utama.
/// Mencegah blackscreen ketika GUI VST3 (nih-plug / baseview / JUCE OpenGL) mengambil alih
/// atau melepas `wglMakeCurrent` pada thread utama.
pub fn restore_host_gl_context() {
    #[cfg(windows)]
    unsafe {
        let target_dc = HOST_GL_HDC.load(Ordering::Relaxed) as ffi::HANDLE;
        let target_ctx = HOST_GL_HGLRC.load(Ordering::Relaxed) as ffi::HANDLE;
        if !target_dc.is_null() && !target_ctx.is_null() {
            if ffi::wglGetCurrentContext() != target_ctx || ffi::wglGetCurrentDC() != target_dc {
                let _ = ffi::wglMakeCurrent(target_dc, target_ctx);
            }
        } else {
            capture_host_gl_context();
        }
    }
}

pub struct OpenGlContextGuard {
    #[cfg(windows)]
    fallback_dc: ffi::HANDLE,
    #[cfg(windows)]
    fallback_ctx: ffi::HANDLE,
}

impl OpenGlContextGuard {
    pub fn save() -> Self {
        #[cfg(windows)]
        unsafe {
            let mut dc = HOST_GL_HDC.load(Ordering::Relaxed) as ffi::HANDLE;
            let mut ctx = HOST_GL_HGLRC.load(Ordering::Relaxed) as ffi::HANDLE;
            if dc.is_null() || ctx.is_null() {
                dc = ffi::wglGetCurrentDC();
                ctx = ffi::wglGetCurrentContext();
                if !dc.is_null() && !ctx.is_null() {
                    HOST_GL_HDC.store(dc as isize, Ordering::SeqCst);
                    HOST_GL_HGLRC.store(ctx as isize, Ordering::SeqCst);
                }
            }
            Self {
                fallback_dc: dc,
                fallback_ctx: ctx,
            }
        }
        #[cfg(not(windows))]
        Self {}
    }
}

impl Drop for OpenGlContextGuard {
    fn drop(&mut self) {
        #[cfg(windows)]
        unsafe {
            let dc = HOST_GL_HDC.load(Ordering::Relaxed) as ffi::HANDLE;
            let ctx = HOST_GL_HGLRC.load(Ordering::Relaxed) as ffi::HANDLE;
            let use_dc = if !dc.is_null() { dc } else { self.fallback_dc };
            let use_ctx = if !ctx.is_null() { ctx } else { self.fallback_ctx };
            if !use_ctx.is_null() {
                let _ = ffi::wglMakeCurrent(use_dc, use_ctx);
            }
        }
    }
}

/// Ambil posisi & ukuran jendela utama Win32 `(x, y, width, height)` jika sedang tampil normal (tidak minimize).
pub fn get_main_window_rect(window_title: &str) -> Option<(i32, i32, u32, u32)> {
    #[cfg(windows)]
    unsafe {
        let wtitle = to_wide(window_title);
        let mut hwnd = ffi::FindWindowW(std::ptr::null(), wtitle.as_ptr());
        if hwnd != 0 {
            MAIN_WINDOW_HWND.store(hwnd, Ordering::Relaxed);
        } else {
            hwnd = MAIN_WINDOW_HWND.load(Ordering::Relaxed);
        }
        if hwnd == 0 || ffi::IsWindowVisible(hwnd) == 0 || ffi::IsIconic(hwnd) != 0 {
            return None;
        }
        let mut r = ffi::RECT::default();
        if ffi::GetWindowRect(hwnd, &mut r) != 0 {
            let w = (r.right - r.left).max(400) as u32;
            let h = (r.bottom - r.top).max(300) as u32;
            // Abaikan koordinat off-screen ekstrem (-32000 saat baru minimize)
            if r.left > -10000 && r.top > -10000 {
                return Some((r.left, r.top, w, h));
            }
        }
        None
    }
    #[cfg(not(windows))]
    {
        let _ = window_title;
        None
    }
}

/// Pindahkan jendela utama Win32 ke koordinat `(x, y)` yang tersimpan di config.
pub fn restore_main_window_pos(window_title: &str, x: i32, y: i32, w: Option<u32>, h: Option<u32>) -> bool {
    #[cfg(windows)]
    unsafe {
        let wtitle = to_wide(window_title);
        let hwnd = ffi::FindWindowW(std::ptr::null(), wtitle.as_ptr());
        if hwnd == 0 {
            return false;
        }
        let mut r = ffi::RECT::default();
        let _ = ffi::GetWindowRect(hwnd, &mut r);
        let cur_w = w.map(|v| v as i32).unwrap_or((r.right - r.left).max(700));
        let cur_h = h.map(|v| v as i32).unwrap_or((r.bottom - r.top).max(580));
        // SWP_NOZORDER (0x0004) | SWP_NOACTIVATE (0x0010)
        ffi::SetWindowPos(hwnd, 0, x, y, cur_w, cur_h, 0x0004 | 0x0010) != 0
    }
    #[cfg(not(windows))]
    {
        let _ = (window_title, x, y, w, h);
        false
    }
}

/// Cari nama file DLL / VST3 / EXE dan offset heksadesimal dari alamat memori instruksi saat crash.
pub fn resolve_module_from_address(addr: usize) -> (String, usize, usize) {
    #[cfg(windows)]
    unsafe {
        if addr == 0 {
            return ("(Null Address / 0x0)".to_string(), 0, 0);
        }
        let mut hmod: ffi::HINSTANCE = 0;
        let flags = ffi::GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS
            | ffi::GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT;
        if ffi::GetModuleHandleExW(flags, addr as *const u16, &mut hmod) != 0 && hmod != 0 {
            let mut buf = [0u16; 512];
            let len = ffi::GetModuleFileNameW(hmod, buf.as_mut_ptr(), 512) as usize;
            let path = if len > 0 {
                String::from_utf16_lossy(&buf[..len])
            } else {
                "Unknown Module".to_string()
            };
            let base = hmod as usize;
            let offset = addr.saturating_sub(base);
            return (path, base, offset);
        }
    }
    ("Unregistered External / JIT Module".to_string(), 0, addr)
}


// ==============================================================================
// 7. SAFE GPU BOOST CLOCK LOCK & 1-SECOND LIVE GPU CLOCK TELEMETRY (NVML + SMI)
// ==============================================================================
// Persis seperti EVGA Precision X1 "Boost Lock":
// - TIDAK melakukan Overclocking (OC) dan TIDAK meminta input MHz manual.
// - Otomatis mendeteksi kapabilitas maksimum GPU Boost bawaan pabrik (`max_graphics_mhz`)
//   dari driver NVIDIA (`nvml.dll`) dan mengunci frekuensi P-State ke angka tersebut agar
//   clock GPU tetap stabil (mis. 1980 MHz) dan tidak turun-naik di sela-sela chunk 300 ms Vonovox.
// - Dilengkapi Auto Failsafe Watchdog (default ON): jika saat dikunci terjadi error driver,
//   suhu GPU mencapai >= 83°C, atau clock drop tidak wajar (< 500 MHz), sistem otomatis
//   membatalkan lock dan mengembalikan GPU ke mode dinamis standar (`-rgc`).

#[derive(Clone, Debug)]
pub struct GpuTelemetrySnapshot {
    pub available: bool,
    pub gpu_name: String,
    pub current_graphics_mhz: u32,
    pub max_graphics_mhz: u32,
    pub gpu_temp_c: u32,
    pub is_boost_locked: bool,
    pub locked_target_mhz: u32,
    pub low_clock_streak: u8,
    pub failsafe_triggered_msg: Option<String>,
    pub last_status_msg: String,
}

impl Default for GpuTelemetrySnapshot {
    fn default() -> Self {
        Self {
            available: false,
            gpu_name: "Detecting GPU...".to_string(),
            current_graphics_mhz: 0,
            max_graphics_mhz: 1980,
            gpu_temp_c: 0,
            is_boost_locked: false,
            locked_target_mhz: 1980,
            low_clock_streak: 0,
            failsafe_triggered_msg: None,
            last_status_msg: "Idle (Dynamic GPU Clock)".to_string(),
        }
    }
}

#[cfg(windows)]
struct NvmlApi {
    _hmod: ffi::HMODULE,
    dev: *mut c_void,
    gpu_name: String,
    auto_boost_mhz: u32,
    hw_ceiling_mhz: u32,
    fn_get_clock: unsafe extern "C" fn(*mut c_void, u32, *mut u32) -> i32,
    fn_get_temp: Option<unsafe extern "C" fn(*mut c_void, u32, *mut u32) -> i32>,
    fn_set_locked_clocks: Option<unsafe extern "C" fn(*mut c_void, u32, u32) -> i32>,
    fn_reset_locked_clocks: Option<unsafe extern "C" fn(*mut c_void) -> i32>,
}

#[cfg(windows)]
unsafe impl Send for NvmlApi {}
#[cfg(windows)]
unsafe impl Sync for NvmlApi {}

#[cfg(windows)]
static NVML_API: std::sync::OnceLock<Option<NvmlApi>> = std::sync::OnceLock::new();

static GPU_SNAPSHOT: Mutex<GpuTelemetrySnapshot> = Mutex::new(GpuTelemetrySnapshot {
    available: false,
    gpu_name: String::new(),
    current_graphics_mhz: 0,
    max_graphics_mhz: 1980,
    gpu_temp_c: 0,
    is_boost_locked: false,
    locked_target_mhz: 1980,
    low_clock_streak: 0,
    failsafe_triggered_msg: None,
    last_status_msg: String::new(),
});

#[cfg(windows)]
fn init_nvml_api() -> Option<NvmlApi> {
    unsafe {
        let candidates = [
            "nvml.dll",
            "C:\\Windows\\System32\\nvml.dll",
            "C:\\Program Files\\NVIDIA Corporation\\NVSMI\\nvml.dll",
        ];
        let mut hmod: ffi::HMODULE = 0;
        for c in candidates {
            let wc = to_wide(c);
            hmod = ffi::LoadLibraryW(wc.as_ptr());
            if hmod != 0 {
                break;
            }
        }
        if hmod == 0 {
            return None;
        }

        type FnInit = unsafe extern "C" fn() -> i32;
        type FnGetHandle = unsafe extern "C" fn(u32, *mut *mut c_void) -> i32;
        type FnGetName = unsafe extern "C" fn(*mut c_void, *mut i8, u32) -> i32;
        type FnGetClock = unsafe extern "C" fn(*mut c_void, u32, *mut u32) -> i32;
        type FnGetTemp = unsafe extern "C" fn(*mut c_void, u32, *mut u32) -> i32;
        type FnSetLocked = unsafe extern "C" fn(*mut c_void, u32, u32) -> i32;
        type FnResetLocked = unsafe extern "C" fn(*mut c_void) -> i32;

        let mut p_init = ffi::GetProcAddress(hmod, b"nvmlInit_v2\0".as_ptr());
        if p_init.is_null() {
            p_init = ffi::GetProcAddress(hmod, b"nvmlInit\0".as_ptr());
        }
        let mut p_handle = ffi::GetProcAddress(hmod, b"nvmlDeviceGetHandleByIndex_v2\0".as_ptr());
        if p_handle.is_null() {
            p_handle = ffi::GetProcAddress(hmod, b"nvmlDeviceGetHandleByIndex\0".as_ptr());
        }
        let p_name = ffi::GetProcAddress(hmod, b"nvmlDeviceGetName\0".as_ptr());
        let p_clock = ffi::GetProcAddress(hmod, b"nvmlDeviceGetClockInfo\0".as_ptr());
        let p_def_app_clock = ffi::GetProcAddress(hmod, b"nvmlDeviceGetDefaultApplicationsClock\0".as_ptr());
        let p_app_clock = ffi::GetProcAddress(hmod, b"nvmlDeviceGetApplicationsClock\0".as_ptr());
        let p_max_clock = ffi::GetProcAddress(hmod, b"nvmlDeviceGetMaxClockInfo\0".as_ptr());
        let p_temp = ffi::GetProcAddress(hmod, b"nvmlDeviceGetTemperature\0".as_ptr());
        let p_set_lock = ffi::GetProcAddress(hmod, b"nvmlDeviceSetGpuLockedClocks\0".as_ptr());
        let p_rst_lock = ffi::GetProcAddress(hmod, b"nvmlDeviceResetGpuLockedClocks\0".as_ptr());

        if p_handle.is_null() || p_clock.is_null() {
            return None;
        }
        if !p_init.is_null() {
            let f_init: FnInit = std::mem::transmute(p_init);
            if f_init() != 0 {
                return None;
            }
        }

        let f_handle: FnGetHandle = std::mem::transmute(p_handle);
        let mut dev: *mut c_void = ptr::null_mut();
        if f_handle(0, &mut dev) != 0 || dev.is_null() {
            return None;
        }

        let mut gpu_name = "NVIDIA GPU".to_string();
        if !p_name.is_null() {
            let f_name: FnGetName = std::mem::transmute(p_name);
            let mut nbuf = [0i8; 96];
            if f_name(dev, nbuf.as_mut_ptr(), 95) == 0 {
                let bytes: Vec<u8> = nbuf
                    .iter()
                    .take_while(|&&c| c != 0)
                    .map(|&c| c as u8)
                    .collect();
                if let Ok(s) = String::from_utf8(bytes) {
                    if !s.trim().is_empty() {
                        gpu_name = s.trim().to_string();
                    }
                }
            }
        }

        let f_clock: FnGetClock = std::mem::transmute(p_clock);

        // Deteksi otomatis Max GPU Boost Capability bawaan GPU (tanpa OC):
        // 1. Cek DefaultApplicationsClock / ApplicationsClock (Rated Boost Clock VBIOS, mis. 1860-1980 MHz)
        // 2. Cek MaxClockInfo (P0 Ceiling VBIOS)
        let mut def_boost_mhz = 0u32;
        if !p_def_app_clock.is_null() {
            let f_def: FnGetClock = std::mem::transmute(p_def_app_clock);
            let mut m = 0u32;
            if f_def(dev, 0, &mut m) == 0 && (1100..=3200).contains(&m) {
                def_boost_mhz = m;
            }
        }
        if def_boost_mhz == 0 && !p_app_clock.is_null() {
            let f_app: FnGetClock = std::mem::transmute(p_app_clock);
            let mut m = 0u32;
            if f_app(dev, 0, &mut m) == 0 && (1100..=3200).contains(&m) {
                def_boost_mhz = m;
            }
        }

        let mut hw_ceiling_mhz = 2100u32;
        if !p_max_clock.is_null() {
            let f_max: FnGetClock = std::mem::transmute(p_max_clock);
            let mut m = 0u32;
            if f_max(dev, 0, &mut m) == 0 && (1100..=3400).contains(&m) {
                hw_ceiling_mhz = m;
            }
        }

        let auto_boost_mhz = if def_boost_mhz >= 1200 {
            def_boost_mhz.min(hw_ceiling_mhz)
        } else {
            hw_ceiling_mhz.clamp(1350, 2400)
        };

        let fn_get_temp = if !p_temp.is_null() {
            Some(std::mem::transmute::<*mut c_void, FnGetTemp>(p_temp))
        } else {
            None
        };
        let fn_set_locked_clocks = if !p_set_lock.is_null() {
            Some(std::mem::transmute::<*mut c_void, FnSetLocked>(p_set_lock))
        } else {
            None
        };
        let fn_reset_locked_clocks = if !p_rst_lock.is_null() {
            Some(std::mem::transmute::<*mut c_void, FnResetLocked>(p_rst_lock))
        } else {
            None
        };

        Some(NvmlApi {
            _hmod: hmod,
            dev,
            gpu_name,
            auto_boost_mhz,
            hw_ceiling_mhz,
            fn_get_clock: f_clock,
            fn_get_temp,
            fn_set_locked_clocks,
            fn_reset_locked_clocks,
        })
    }
}

#[cfg(windows)]
unsafe fn force_unlock_gpu_clocks_quiet(api_opt: &Option<NvmlApi>) {
    let mut unlocked = false;
    if let Some(ref api) = api_opt {
        if let Some(f_rst) = api.fn_reset_locked_clocks {
            if f_rst(api.dev) == 0 {
                unlocked = true;
            }
        }
    }
    if !unlocked {
        let smi_candidates = [
            "C:\\Windows\\System32\\nvidia-smi.exe",
            "C:\\Program Files\\NVIDIA Corporation\\NVSMI\\nvidia-smi.exe",
            "nvidia-smi.exe",
        ];
        let smi_exe = smi_candidates
            .iter()
            .find(|p| std::path::Path::new(p).exists())
            .copied()
            .unwrap_or("nvidia-smi.exe");
        let w_op = to_wide("runas");
        let w_file = to_wide(smi_exe);
        let w_params = to_wide("-rgc");
        let _ = ffi::ShellExecuteW(
            0,
            w_op.as_ptr(),
            w_file.as_ptr(),
            w_params.as_ptr(),
            ptr::null(),
            ffi::SW_HIDE,
        );
    }
}

/// Dipanggil setiap 1 detik di background timer untuk:
/// 1) Memperbarui bacaan frekuensi GPU (`MHz`) & suhu (`°C`) secara real-time.
/// 2) Menjalankan **Auto Failsafe Watchdog** (jika `failsafe_enabled == true` dan Boost Lock sedang ON):
///    Jika terdeteksi error pembacaan driver, suhu GPU >= 83°C, atau clock drop tidak wajar (< 500 MHz
///    selama 3 detik berturut-turut saat terkunci), otomatis membatalkan lock & mengembalikan ke mode dinamis!
pub fn poll_gpu_clock_telemetry_with_failsafe(failsafe_enabled: bool) -> GpuTelemetrySnapshot {
    #[cfg(windows)]
    {
        let api_opt = NVML_API.get_or_init(init_nvml_api);
        let mut snap = GPU_SNAPSHOT.lock();
        if let Some(ref api) = api_opt {
            let mut cur_mhz = 0u32;
            let mut temp_c = 0u32;
            let clock_rc = unsafe { (api.fn_get_clock)(api.dev, 0, &mut cur_mhz) };
            if let Some(f_temp) = api.fn_get_temp {
                unsafe {
                    let _ = f_temp(api.dev, 0, &mut temp_c);
                }
            }

            snap.available = true;
            snap.gpu_name = api.gpu_name.clone();
            snap.current_graphics_mhz = cur_mhz;
            snap.gpu_temp_c = temp_c;

            // Saat inisialisasi atau saat GPU berakselerasi alami di dalam batas VBIOS (tanpa OC),
            // ikuti kemampuan GPU Boost alami tertinggi milik kartu grafis ini (seperti EVGA Precision X1).
            if snap.max_graphics_mhz < api.auto_boost_mhz {
                snap.max_graphics_mhz = api.auto_boost_mhz;
            }
            if !snap.is_boost_locked
                && cur_mhz > snap.max_graphics_mhz
                && cur_mhz <= api.hw_ceiling_mhz
            {
                snap.max_graphics_mhz = cur_mhz;
            }

            // AUTO FAILSAFE WATCHDOG (Berjalan setiap 1 detik saat GPU Boost Lock aktif)
            if snap.is_boost_locked && failsafe_enabled {
                let mut failsafe_reason: Option<String> = None;

                if clock_rc != 0 {
                    failsafe_reason = Some(format!(
                        "Failsafe Triggered: NVIDIA Driver / NVML returned status code {} while reading GPU clock. Boost Lock has been automatically canceled for GPU safety.",
                        clock_rc
                    ));
                } else if temp_c >= 83 {
                    failsafe_reason = Some(format!(
                        "Failsafe Triggered: GPU temperature reached {}°C (>= 83°C safety threshold). Boost Lock has been automatically canceled to cool down your GPU.",
                        temp_c
                    ));
                } else if cur_mhz > 0 && cur_mhz < 500 {
                    snap.low_clock_streak = snap.low_clock_streak.saturating_add(1);
                    if snap.low_clock_streak >= 3 {
                        failsafe_reason = Some(format!(
                            "Failsafe Triggered: GPU clock dropped to {} MHz under lock (possible power/driver fault). Boost Lock has been automatically canceled.",
                            cur_mhz
                        ));
                    }
                } else {
                    snap.low_clock_streak = 0;
                }

                if let Some(reason) = failsafe_reason {
                    unsafe {
                        force_unlock_gpu_clocks_quiet(api_opt);
                    }
                    snap.is_boost_locked = false;
                    snap.low_clock_streak = 0;
                    snap.last_status_msg = "Failsafe Auto-Unlocked (Dynamic Clock Restored)".to_string();
                    snap.failsafe_triggered_msg = Some(reason);
                }
            } else {
                snap.low_clock_streak = 0;
            }

            if snap.last_status_msg.is_empty() {
                snap.last_status_msg = "Dynamic Clock (Unlocked)".to_string();
            }
        } else {
            snap.available = false;
            if snap.gpu_name.is_empty() {
                snap.gpu_name = "NVIDIA NVML Not Detected (Use EVGA Precision X1 / MSI Afterburner)".to_string();
            }
            if snap.last_status_msg.is_empty() {
                snap.last_status_msg = "External GPU Utility Mode".to_string();
            }
        }
        return snap.clone();
    }
    #[cfg(not(windows))]
    {
        let _ = failsafe_enabled;
        GPU_SNAPSHOT.lock().clone()
    }
}

pub fn poll_gpu_clock_telemetry() -> GpuTelemetrySnapshot {
    poll_gpu_clock_telemetry_with_failsafe(true)
}

pub fn get_gpu_telemetry_snapshot() -> GpuTelemetrySnapshot {
    let snap = GPU_SNAPSHOT.lock();
    if snap.gpu_name.is_empty() {
        drop(snap);
        return poll_gpu_clock_telemetry_with_failsafe(true);
    }
    snap.clone()
}

pub fn take_gpu_failsafe_alert() -> Option<String> {
    let mut snap = GPU_SNAPSHOT.lock();
    snap.failsafe_triggered_msg.take()
}

/// Mengaktifkan atau mematikan penguncian GPU Boost Clock (`Lock GPU Boost`) secara OTOMATIS
/// mengikuti kemampuan maksimum GPU Boost kartu grafis (persis seperti EVGA Precision X1, tanpa OC manual).
pub fn set_gpu_boost_clock_lock(enable: bool, _unused_manual_mhz: u32) -> Result<String, String> {
    set_gpu_boost_clock_lock_with_failsafe(enable, true)
}

pub fn set_gpu_boost_clock_lock_with_failsafe(
    enable: bool,
    failsafe_enabled: bool,
) -> Result<String, String> {
    #[cfg(windows)]
    unsafe {
        let api_opt = NVML_API.get_or_init(init_nvml_api);

        // Jika Failsafe aktif dan NVML tidak terdeteksi pada saat hendak mengaktifkan Lock, batalkan dengan aman.
        if enable && failsafe_enabled && api_opt.is_none() {
            let mut snap = GPU_SNAPSHOT.lock();
            snap.is_boost_locked = false;
            snap.last_status_msg = "Failsafe Canceled: NVIDIA NVML Not Detected".to_string();
            return Err(
                "Failsafe Protection: NVIDIA GPU / NVML driver interface was not detected on this system. GPU Boost Lock has been safely canceled."
                    .to_string(),
            );
        }

        // Ambil kapabilitas Boost otomatis dari GPU (tanpa OC)
        let auto_target_mhz = {
            let snap = GPU_SNAPSHOT.lock();
            if let Some(ref api) = api_opt {
                snap.max_graphics_mhz
                    .max(api.auto_boost_mhz)
                    .min(api.hw_ceiling_mhz)
                    .clamp(1100, 3100)
            } else {
                snap.max_graphics_mhz.clamp(1200, 2400)
            }
        };

        // Pre-check suhu GPU jika Failsafe aktif
        if enable && failsafe_enabled {
            if let Some(ref api) = api_opt {
                if let Some(f_temp) = api.fn_get_temp {
                    let mut t = 0u32;
                    if f_temp(api.dev, 0, &mut t) == 0 && t >= 82 {
                        let mut snap = GPU_SNAPSHOT.lock();
                        snap.is_boost_locked = false;
                        snap.last_status_msg = format!("Failsafe Canceled: GPU Temp {}°C Too High", t);
                        return Err(format!(
                            "Failsafe Protection: Current GPU temperature is {}°C (>= 82°C limit). Locking GPU Boost Clock is canceled until GPU cools down.",
                            t
                        ));
                    }
                }
            }
        }

        let mut nvml_direct_ok = false;
        if let Some(ref api) = api_opt {
            if enable {
                if let Some(f_set) = api.fn_set_locked_clocks {
                    let r = f_set(api.dev, auto_target_mhz, auto_target_mhz);
                    if r == 0 {
                        nvml_direct_ok = true;
                    }
                }
            } else if let Some(f_rst) = api.fn_reset_locked_clocks {
                let r = f_rst(api.dev);
                if r == 0 {
                    nvml_direct_ok = true;
                }
            }
        }

        if !nvml_direct_ok {
            let smi_candidates = [
                "C:\\Windows\\System32\\nvidia-smi.exe",
                "C:\\Program Files\\NVIDIA Corporation\\NVSMI\\nvidia-smi.exe",
                "nvidia-smi.exe",
            ];
            let smi_exe = smi_candidates
                .iter()
                .find(|p| std::path::Path::new(p).exists())
                .copied()
                .unwrap_or("nvidia-smi.exe");

            let args = if enable {
                format!("-lgc {},{}", auto_target_mhz, auto_target_mhz)
            } else {
                "-rgc".to_string()
            };

            let w_op = to_wide("runas");
            let w_file = to_wide(smi_exe);
            let w_params = to_wide(&args);
            let hinst = ffi::ShellExecuteW(
                0,
                w_op.as_ptr(),
                w_file.as_ptr(),
                w_params.as_ptr(),
                ptr::null(),
                ffi::SW_HIDE,
            );
            if hinst <= 32 {
                let mut snap = GPU_SNAPSHOT.lock();
                snap.is_boost_locked = false;
                snap.last_status_msg = "Lock Canceled / Admin Permission Declined".to_string();
                return Err(
                    "GPU Boost Clock Lock requires Administrator permission (or an NVIDIA GPU with nvidia-smi / EVGA Precision X1). Operation safely canceled."
                        .to_string(),
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(280));
        }

        // Post-lock verification jika Failsafe aktif
        if enable && failsafe_enabled {
            if let Some(ref api) = api_opt {
                let mut test_mhz = 0u32;
                let rc = (api.fn_get_clock)(api.dev, 0, &mut test_mhz);
                if rc != 0 {
                    force_unlock_gpu_clocks_quiet(api_opt);
                    let mut snap = GPU_SNAPSHOT.lock();
                    snap.is_boost_locked = false;
                    snap.last_status_msg = "Failsafe Canceled: Post-Lock Check Failed".to_string();
                    return Err(
                        "Failsafe Protection: Driver reported an error after locking clock. Reverted to standard dynamic GPU clock."
                            .to_string(),
                    );
                }
            }
        }

        {
            let mut snap = GPU_SNAPSHOT.lock();
            snap.is_boost_locked = enable;
            snap.locked_target_mhz = auto_target_mhz;
            snap.low_clock_streak = 0;
            snap.failsafe_triggered_msg = None;
            snap.last_status_msg = if enable {
                format!(
                    "LOCKED @ {} MHz (Auto Max Boost | Thermal & TDP Limits Active)",
                    auto_target_mhz
                )
            } else {
                "Unlocked (Standard Dynamic GPU Boost)".to_string()
            };
        }

        let updated = poll_gpu_clock_telemetry_with_failsafe(failsafe_enabled);
        if enable {
            Ok(format!(
                "GPU Boost Clock safely locked to your GPU's auto boost capability ({} MHz | Live: {} MHz).",
                auto_target_mhz, updated.current_graphics_mhz
            ))
        } else {
            Ok(format!(
                "GPU Boost Clock unlocked and restored to dynamic power-saving mode (Live: {} MHz).",
                updated.current_graphics_mhz
            ))
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (enable, failsafe_enabled);
        Err("Only supported on Windows".to_string())
    }
}

/// Membuka file teks catatan ASIO (`Savedata\asio_performance_notes.txt`) langsung di Notepad Windows.
pub fn open_file_in_notepad(path: &std::path::Path) {
    #[cfg(windows)]
    unsafe {
        let w_op = to_wide("open");
        let w_file = to_wide("notepad.exe");
        let w_params = to_wide(&format!("\"{}\"", path.to_string_lossy()));
        let _ = ffi::ShellExecuteW(
            0,
            w_op.as_ptr(),
            w_file.as_ptr(),
            w_params.as_ptr(),
            ptr::null(),
            ffi::SW_RESTORE,
        );
    }
    #[cfg(not(windows))]
    {
        let _ = path;
    }
}


/// Otomatis dipanggil saat Rinka SoundBridge ditutup / dimatikan (atau mengalami crash):
/// Mengembalikan GPU Boost Clock ke mode dinamis standar (Auto-Reset & Auto-Off) agar GPU
/// tidak tetap terkunci di clock tinggi setelah aplikasi keluar.
pub fn reset_gpu_boost_clock_lock_on_exit() {
    #[cfg(windows)]
    unsafe {
        let was_locked = {
            if let Some(mut g) = GPU_SNAPSHOT.try_lock() {
                let locked = g.is_boost_locked;
                g.is_boost_locked = false;
                locked
            } else {
                true
            }
        };
        if was_locked {
            let api_opt = NVML_API.get_or_init(init_nvml_api);
            force_unlock_gpu_clocks_quiet(api_opt);
        }
    }
}
