#![allow(non_snake_case)]
use rinka_soundbridge_core::vst3_host::VSTSlot;
use std::path::Path;
use std::sync::atomic::Ordering;

#[test]
fn test_vst3_missing_plugin_returns_err_cleanly() {
    let res = VSTSlot::load("C:\\__nonexistent_rinka_vst3_plugin__.vst3".to_string(), 48_000.0, 2048);
    assert!(res.is_err());
}

#[cfg(windows)]
mod win32_gl_host {
    use std::ffi::c_void;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct PIXELFORMATDESCRIPTOR {
        pub nSize: u16,
        pub nVersion: u16,
        pub dwFlags: u32,
        pub iPixelType: u8,
        pub cColorBits: u8,
        pub cRedBits: u8,
        pub cRedShift: u8,
        pub cGreenBits: u8,
        pub cGreenShift: u8,
        pub cBlueBits: u8,
        pub cBlueShift: u8,
        pub cAlphaBits: u8,
        pub cAlphaShift: u8,
        pub cAccumBits: u8,
        pub cAccumRedBits: u8,
        pub cAccumGreenBits: u8,
        pub cAccumBlueBits: u8,
        pub cAccumAlphaBits: u8,
        pub cDepthBits: u8,
        pub cStencilBits: u8,
        pub cAuxBuffers: u8,
        pub iLayerType: u8,
        pub bReserved: u8,
        pub dwLayerMask: u32,
        pub dwVisibleMask: u32,
        pub dwDamageMask: u32,
    }

    #[link(name = "user32")]
    extern "system" {
        pub fn GetDC(hWnd: isize) -> *mut c_void;
        pub fn ReleaseDC(hWnd: isize, hDC: *mut c_void) -> i32;
    }

    #[link(name = "gdi32")]
    extern "system" {
        pub fn ChoosePixelFormat(hdc: *mut c_void, ppfd: *const PIXELFORMATDESCRIPTOR) -> i32;
        pub fn SetPixelFormat(hdc: *mut c_void, format: i32, ppfd: *const PIXELFORMATDESCRIPTOR) -> i32;
    }

    #[link(name = "opengl32")]
    extern "system" {
        pub fn wglCreateContext(hdc: *mut c_void) -> *mut c_void;
        pub fn wglDeleteContext(hglrc: *mut c_void) -> i32;
        pub fn wglGetCurrentContext() -> *mut c_void;
        pub fn wglMakeCurrent(hdc: *mut c_void, hglrc: *mut c_void) -> i32;
    }
}

#[test]
#[ignore = "Live Windows VST3 & OpenGL hardware test against installed plugins"]
fn test_real_vst3_plugins_detach_reopen_bypass_and_clean_release() {
    #[cfg(windows)]
    unsafe {
        use rinka_soundbridge_core::win32::{capture_host_gl_context, ffi, to_wide};
        use std::ptr;
        use std::time::{Duration, Instant};

        unsafe extern "system" fn dummy_wndproc(
            hwnd: ffi::HWND,
            msg: ffi::UINT,
            wparam: ffi::WPARAM,
            lparam: ffi::LPARAM,
        ) -> ffi::LRESULT {
            ffi::DefWindowProcW(hwnd, msg, wparam, lparam)
        }

        let class_name = to_wide("RinkaTestHostGlDummy");
        let title = to_wide("RinkaTestHostGlDummyWindow");
        let hinst = ffi::GetModuleHandleW(ptr::null());
        let wc = ffi::WNDCLASSEXW {
            cbSize: std::mem::size_of::<ffi::WNDCLASSEXW>() as u32,
            style: 0x0020, // CS_OWNDC
            lpfnWndProc: Some(dummy_wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinst,
            hIcon: 0,
            hCursor: 0,
            hbrBackground: 0,
            lpszMenuName: ptr::null(),
            lpszClassName: class_name.as_ptr(),
            hIconSm: 0,
        };
        let _ = ffi::RegisterClassExW(&wc);
        let host_hwnd = ffi::CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            ffi::WS_OVERLAPPEDWINDOW,
            0,
            0,
            320,
            240,
            0,
            0,
            hinst,
            ptr::null_mut(),
        );
        assert_ne!(host_hwnd, 0, "Failed to create dummy host HWND");

        let host_dc = win32_gl_host::GetDC(host_hwnd);
        assert!(!host_dc.is_null(), "GetDC failed");
        let mut pfd: win32_gl_host::PIXELFORMATDESCRIPTOR = std::mem::zeroed();
        pfd.nSize = std::mem::size_of::<win32_gl_host::PIXELFORMATDESCRIPTOR>() as u16;
        pfd.nVersion = 1;
        pfd.dwFlags = 0x0000_0025; // PFD_DRAW_TO_WINDOW | PFD_SUPPORT_OPENGL | PFD_DOUBLEBUFFER
        pfd.iPixelType = 0; // PFD_TYPE_RGBA
        pfd.cColorBits = 32;
        let fmt = win32_gl_host::ChoosePixelFormat(host_dc, &pfd);
        assert_ne!(fmt, 0, "ChoosePixelFormat failed");
        assert_ne!(win32_gl_host::SetPixelFormat(host_dc, fmt, &pfd), 0, "SetPixelFormat failed");
        let host_gl = win32_gl_host::wglCreateContext(host_dc);
        assert!(!host_gl.is_null(), "wglCreateContext failed");
        assert_ne!(win32_gl_host::wglMakeCurrent(host_dc, host_gl), 0, "wglMakeCurrent failed");

        capture_host_gl_context();

        let pump_ms = |ms: u64| {
            let until = Instant::now() + Duration::from_millis(ms);
            let mut msg: ffi::MSG = std::mem::zeroed();
            while Instant::now() < until {
                while ffi::PeekMessageW(&mut msg, 0, 0, 0, ffi::PM_REMOVE) != 0 {
                    ffi::TranslateMessage(&msg);
                    ffi::DispatchMessageW(&msg);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        };

        let candidates = [
            "C:\\Program Files\\Common Files\\VST3\\Clear.vst3",
            "C:\\Program Files\\Common Files\\VST3\\rinka-microroom-vst.vst3",
            "C:\\Program Files\\Common Files\\VST3\\FabFilter\\FabFilter Pro-R.vst3",
            "C:\\Program Files\\Common Files\\VST3\\Rinka Denoiser pack.vst3",
            "C:\\Program Files\\Common Files\\VST3\\rinka-radio-comms-vst.vst3",
            "C:\\Program Files\\Common Files\\VST3\\rinka-dns64-lite-v2-vst.vst3",
        ];

        let mut tested = 0usize;
        for path in candidates {
            if !Path::new(path).exists() {
                println!("[SKIP] Missing {}", path);
                continue;
            }
            tested += 1;
            println!("[TEST] Loading {} ...", path);
            let slot = VSTSlot::load(path.to_string(), 48_000.0, 2048)
                .unwrap_or_else(|e| panic!("VSTSlot::load failed for {}: {}", path, e));

            let mut left = vec![0.0f32; 256];
            let mut right = vec![0.0f32; 256];
            for i in 0..256 {
                let s = (2.0 * std::f32::consts::PI * 440.0 * (i as f32 / 48_000.0)).sin() * 0.2;
                left[i] = s;
                right[i] = s;
            }

            for _ in 0..5 {
                let mut l = left.clone();
                let mut r = right.clone();
                slot.audio_proc
                    .lock()
                    .as_mut()
                    .unwrap()
                    .process_stereo(&mut l, &mut r, 48_000)
                    .unwrap_or_else(|e| panic!("process_stereo 1 failed for {}: {}", path, e));
                assert!(l.iter().all(|v| v.is_finite()) && r.iter().all(|v| v.is_finite()));
            }

            // Verify bypass suspends IAudioProcessor::is_processing and unbypass restores it cleanly
            slot.set_bypassed(true);
            assert!(slot.is_bypassed());
            assert!(!slot.audio_proc.lock().as_ref().unwrap().is_processing);
            slot.set_bypassed(false);
            assert!(!slot.is_bypassed());
            assert!(slot.audio_proc.lock().as_ref().unwrap().is_processing);
            assert!(slot.flush_pending.load(Ordering::Acquire));

            for _ in 0..5 {
                let mut l = left.clone();
                let mut r = right.clone();
                slot.audio_proc
                    .lock()
                    .as_mut()
                    .unwrap()
                    .process_stereo(&mut l, &mut r, 48_000)
                    .unwrap_or_else(|e| panic!("process_stereo 2 failed for {}: {}", path, e));
                assert!(l.iter().all(|v| v.is_finite()) && r.iter().all(|v| v.is_finite()));
            }

            // Open #1 -> close via hide_editor_window() -> verify active_editor.is_none() & WGL intact
            if let Ok(()) = slot.open_editor() {
                pump_ms(300);
                slot.hide_editor_window();
                pump_ms(40);
                assert!(
                    slot.gui_ctrl.lock().as_ref().unwrap().active_editor.is_none(),
                    "active_editor must be None after hide_editor_window for {}",
                    path
                );
                assert_eq!(
                    win32_gl_host::wglGetCurrentContext(),
                    host_gl,
                    "Host WGL context lost after hide_editor_window for {}",
                    path
                );

                // Open #2 -> WM_CLOSE -> poll_editor_closed() -> verify full detach & WGL intact
                slot.open_editor()
                    .unwrap_or_else(|e| panic!("2nd open_editor failed for {}: {}", path, e));
                pump_ms(200);
                let ed_hwnd = slot
                    .gui_ctrl
                    .lock()
                    .as_ref()
                    .and_then(|c| c.active_editor.as_ref().map(|e| e.hwnd))
                    .unwrap_or(0);
                assert_ne!(ed_hwnd, 0);
                let _ = ffi::PostMessageW(ed_hwnd, ffi::WM_CLOSE, 0, 0);
                pump_ms(60);
                assert!(
                    slot.poll_editor_closed(),
                    "poll_editor_closed should detect WM_CLOSE for {}",
                    path
                );
                assert!(
                    slot.gui_ctrl.lock().as_ref().unwrap().active_editor.is_none(),
                    "active_editor must be None after poll_editor_closed for {}",
                    path
                );
                assert_eq!(
                    win32_gl_host::wglGetCurrentContext(),
                    host_gl,
                    "Host WGL context lost after poll_editor_closed for {}",
                    path
                );
            } else {
                println!("[INFO] {} has no editor view", path);
            }

            slot.shutdown_and_release();
            assert_eq!(
                win32_gl_host::wglGetCurrentContext(),
                host_gl,
                "Host WGL context lost after shutdown_and_release for {}",
                path
            );
            println!("[PASS] {}", path);
        }

        assert!(tested > 0, "Expected at least one VST3 plugin in C:\\Program Files\\Common Files\\VST3");
        let _ = win32_gl_host::wglMakeCurrent(ptr::null_mut(), ptr::null_mut());
        let _ = win32_gl_host::wglDeleteContext(host_gl);
        let _ = win32_gl_host::ReleaseDC(host_hwnd, host_dc);
        let _ = ffi::DestroyWindow(host_hwnd);
    }
}
