//! Asynchronous Daily File Logger & Vectored SEH Crash Analyzer (`RinkaLogger`)
//! - Menulis log harian ke `logs/YYYY-MM-DD.txt` (baik di folder `.exe` maupun folder induk proyek jika dijalankan dari `dist\`)
//! - Merekam jejak aktivitas terakhir (Breadcrumbs) dari Thread Audio & UI
//! - Menggunakan `AddVectoredExceptionHandler(1, ...)` + `SetUnhandledExceptionFilter` + `std::panic::set_hook`
//!   sehingga crash native dari DLL VST3/OpenGL tidak pernah bisa melewati pencatatan `crash_native.txt`!

use chrono::{Local, NaiveDate};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::OnceLock;
use std::thread;

pub struct RinkaLogger {
    sender: SyncSender<Option<(String, String)>>,
}

static GLOBAL_LOGGER: OnceLock<RinkaLogger> = OnceLock::new();
static BREADCRUMBS: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
static CRASH_LOGGED: AtomicBool = AtomicBool::new(false);

/// Catat jejak aktivitas penting (mis. memuat VST, mengubah Bypass, membuka/menutup GUI VST, memulai stream)
/// agar jika terjadi crash native di dalam DLL pihak ketiga, penyebabnya tercatat jelas di `crash_native.txt`.
pub fn record_breadcrumb(action: impl AsRef<str>) {
    if let Some(mut guard) = BREADCRUMBS.try_lock() {
        let now = Local::now().format("%H:%M:%S%.3f");
        let tid = {
            #[cfg(windows)]
            unsafe {
                crate::win32::ffi::GetCurrentThreadId()
            }
            #[cfg(not(windows))]
            {
                0u32
            }
        };
        let tname = std::thread::current().name().unwrap_or("main/worker").to_string();
        if guard.len() >= 15 {
            guard.pop_front();
        }
        guard.push_back(format!("[{}] (Thread {} '{}') {}", now, tid, tname, action.as_ref()));
    }
}

pub fn get_recent_breadcrumbs() -> String {
    if let Some(guard) = BREADCRUMBS.try_lock() {
        if guard.is_empty() {
            return "  (Belum ada aktivitas tercatat)".to_string();
        }
        guard
            .iter()
            .enumerate()
            .map(|(i, s)| format!("  {}. {}", i + 1, s))
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        "  (Sedang diakses oleh thread lain)".to_string()
    }
}

/// Kembalikan direktori tempat `RinkaSoundBridge.exe` berada (mis. `dist\`).
/// Struktur direktori aplikasi mengikuti tata letak standar Rinka SoundBridge:
/// - `RinkaSoundBridge.exe`
/// - `Savedata\` (seluruh file konfigurasi `.json`, `.bak`, dan profil)
/// - `Modules\`  (file core `.dll`, modul ASIO, aset ikon, dsb.)
/// - `Logs\`     (seluruh file log harian `.txt` dan `crash_native.txt`)
pub fn get_base_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            return exe_dir.to_path_buf();
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Direktori `Savedata\` untuk menyimpan semua file konfigurasi dan state aplikasi.
pub fn get_savedata_dir() -> PathBuf {
    let d = get_base_dir().join("Savedata");
    let _ = fs::create_dir_all(&d);
    d
}

/// Direktori `Modules\` untuk menyimpan file core DLL (`rinka_soundbridge_core.dll`) dan modul pendukung.
pub fn get_modules_dir() -> PathBuf {
    let d = get_base_dir().join("Modules");
    let _ = fs::create_dir_all(&d);
    d
}

/// Direktori `Logs\` untuk menyimpan semua file log `.txt`.
pub fn get_log_dir() -> PathBuf {
    let d = get_base_dir().join("Logs");
    let _ = fs::create_dir_all(&d);
    d
}

/// Dapatkan daftar direktori `Logs\` aktif.
pub fn get_all_log_dirs() -> Vec<PathBuf> {
    let _ = get_savedata_dir();
    let _ = get_modules_dir();
    vec![get_log_dir()]
}

pub fn init_logger() {
    GLOBAL_LOGGER.get_or_init(|| {
        for d in get_all_log_dirs() {
            let _ = fs::create_dir_all(&d);
            cleanup_old_logs(&d, 14);
        }

        let (tx, rx) = mpsc::sync_channel::<Option<(String, String)>>(5000);
        let dirs_clone = get_all_log_dirs();
        thread::Builder::new()
            .name("rinka-logger".into())
            .spawn(move || {
                let mut current_date = String::new();
                let mut current_files: Vec<File> = Vec::new();

                while let Ok(Some((level, msg))) = rx.recv() {
                    let now = Local::now();
                    let today_str = now.format("%Y-%m-%d").to_string();
                    if today_str != current_date || current_files.is_empty() {
                        current_date = today_str.clone();
                        current_files.clear();
                        for d in &dirs_clone {
                            let path = d.join(format!("{}.txt", today_str));
                            if let Ok(f) = OpenOptions::new().create(true).append(true).open(path) {
                                current_files.push(f);
                            }
                        }
                    }
                    let timestamp = now.format("%H:%M:%S").to_string();
                    let line = format!("[{}] [{:<5}] {}", timestamp, level, msg);
                    println!("{}", line);
                    for f in &mut current_files {
                        let _ = writeln!(f, "{}", line);
                        let _ = f.flush();
                    }
                }
            })
            .ok();

        RinkaLogger { sender: tx }
    });
}

fn cleanup_old_logs(log_dir: &Path, keep_days: i64) {
    let cutoff = Local::now().date_naive() - chrono::Duration::days(keep_days);
    if let Ok(entries) = fs::read_dir(log_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.len() == 14 && name.ends_with(".txt") {
                if let Ok(day) = NaiveDate::parse_from_str(&name[..10], "%Y-%m-%d") {
                    if day < cutoff {
                        let _ = fs::remove_file(entry.path());
                    }
                }
            }
        }
    }
}

pub fn log_info(msg: impl AsRef<str>) {
    init_logger();
    record_breadcrumb(msg.as_ref());
    if let Some(logger) = GLOBAL_LOGGER.get() {
        let _ = logger.sender.try_send(Some(("INFO".into(), msg.as_ref().to_string())));
    }
}

pub fn log_warn(msg: impl AsRef<str>) {
    init_logger();
    record_breadcrumb(format!("WARN: {}", msg.as_ref()));
    if let Some(logger) = GLOBAL_LOGGER.get() {
        let _ = logger.sender.try_send(Some(("WARN".into(), msg.as_ref().to_string())));
    }
}

pub fn log_error(msg: impl AsRef<str>) {
    init_logger();
    record_breadcrumb(format!("ERROR: {}", msg.as_ref()));
    if let Some(logger) = GLOBAL_LOGGER.get() {
        let _ = logger.sender.try_send(Some(("ERROR".into(), msg.as_ref().to_string())));
    }
}

pub fn shutdown_logger() {
    crate::win32::reset_gpu_boost_clock_lock_on_exit();
    if let Some(logger) = GLOBAL_LOGGER.get() {
        let _ = logger.sender.try_send(None);
    }
}

pub fn write_crash_report_to_disk(report: &str) {
    let today_name = format!("{}.txt", Local::now().format("%Y-%m-%d"));
    for log_dir in get_all_log_dirs() {
        let _ = fs::create_dir_all(&log_dir);
        let crash_path = log_dir.join("crash_native.txt");
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&crash_path) {
            let _ = writeln!(f, "{}", report);
            let _ = f.sync_all();
        }
        let today_path = log_dir.join(&today_name);
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&today_path) {
            let _ = writeln!(f, "{}", report);
            let _ = f.sync_all();
        }
    }
}

/// Periksa apakah kode exception Windows adalah crash fatal yang wajib dicatat ke `crash_native.txt`.
#[cfg(windows)]
fn is_fatal_exception_code(code: u32) -> bool {
    matches!(
        code,
        0xC0000005 // EXCEPTION_ACCESS_VIOLATION
        | 0xC0000374 // STATUS_HEAP_CORRUPTION
        | 0xC00000FD // EXCEPTION_STACK_OVERFLOW
        | 0xC0000094 // EXCEPTION_INT_DIVIDE_BY_ZERO
        | 0xC000001D // EXCEPTION_ILLEGAL_INSTRUCTION
        | 0xC0000409 // STATUS_STACK_BUFFER_OVERRUN
        | 0xC0000006 // EXCEPTION_IN_PAGE_ERROR
        | 0xC0000025 // EXCEPTION_NONCONTINUABLE_EXCEPTION
        | 0xC0000026 // EXCEPTION_INVALID_DISPOSITION
        | 0xC000008C // EXCEPTION_ARRAY_BOUNDS_EXCEEDED
        | 0xC0000096 // EXCEPTION_PRIV_INSTRUCTION
    )
}

#[cfg(windows)]
fn explain_win32_exception(code: u32, info0: usize, info1: usize) -> (&'static str, String) {
    match code {
        0xC0000005 => {
            let op = match info0 {
                0 => "MEMBACA (Read)",
                1 => "MENULIS (Write)",
                8 => "MENGEKSEKUSI (DEP Execute)",
                _ => "MENGAKSES",
            };
            let detail = if info1 == 0 {
                format!(
                    "Plugin VST / Driver mencoba {} Null Pointer (alamat 0x0000000000000000).",
                    op
                )
            } else {
                format!(
                    "Plugin VST / Driver mencoba {} alamat memori tidak valid (0x{:016X}) yang sudah dilepas (Use-After-Free) atau di luar batas buffer.",
                    op, info1
                )
            };
            ("EXCEPTION_ACCESS_VIOLATION (Pelanggaran Akses Memori)", detail)
        }
        0xC0000374 => (
            "STATUS_HEAP_CORRUPTION (Kerusakan Memori Heap)",
            "Sebuah DLL eksternal (plugin VST3 atau driver) menulis melewati batas alokasi memorinya atau membebaskan memori dua kali.".to_string(),
        ),
        0xC00000FD => (
            "EXCEPTION_STACK_OVERFLOW (Kehabisan Memori Stack)",
            "Rekursi tak terbatas atau alokasi stack terlalu besar di dalam thread.".to_string(),
        ),
        0xC0000094 => (
            "EXCEPTION_INT_DIVIDE_BY_ZERO (Pembagian Bulat dengan Nol)",
            "Operasi pembagian bilangan bulat dengan angka 0.".to_string(),
        ),
        0xC000001D => (
            "EXCEPTION_ILLEGAL_INSTRUCTION (Instruksi CPU Tidak Valid)",
            "Eksekusi instruksi CPU yang tidak didukung atau lompatan ke pointer fungsi yang korup.".to_string(),
        ),
        0xC0000409 => (
            "STATUS_STACK_BUFFER_OVERRUN (Korupsi Stack / FastFail)",
            "Kerusakan security cookie pada stack akibat buffer overrun di kode C/C++.".to_string(),
        ),
        _ => (
            "WINDOWS_NATIVE_EXCEPTION (Crash Native OS / Hardware)",
            format!("Proses mengalami exception perangkat keras/OS dengan kode 0x{:08X}.", code),
        ),
    }
}

#[cfg(windows)]
unsafe fn log_ep_if_fatal(ep: *mut crate::win32::ffi::EXCEPTION_POINTERS, handler_kind: &str) {
    if ep.is_null() || (*ep).ExceptionRecord.is_null() {
        return;
    }
    let rec = &*(*ep).ExceptionRecord;
    let code = rec.ExceptionCode;
    if !is_fatal_exception_code(code) {
        return;
    }
    // Pastikan satu crash hanya ditulis satu kali (mencegah duplikasi antara Vectored & Unhandled Filter)
    if CRASH_LOGGED.swap(true, Ordering::SeqCst) {
        return;
    }

    let addr = rec.ExceptionAddress as usize;
    let info0 = if rec.NumberParameters >= 1 { rec.ExceptionInformation[0] } else { 0 };
    let info1 = if rec.NumberParameters >= 2 { rec.ExceptionInformation[1] } else { 0 };

    let (exc_name, exc_desc) = explain_win32_exception(code, info0, info1);
    let (mod_path, mod_base, mod_offset) = crate::win32::resolve_module_from_address(addr);
    let mod_short = Path::new(&mod_path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&mod_path);

    let mut regs_str = String::new();
    if !(*ep).ContextRecord.is_null() {
        let ctx = &*(*ep).ContextRecord;
        regs_str = format!(
            "  RIP = 0x{:016X} | RSP = 0x{:016X} | RBP = 0x{:016X}\n  RAX = 0x{:016X} | RBX = 0x{:016X} | RCX = 0x{:016X} | RDX = 0x{:016X}",
            ctx.Rip, ctx.Rsp, ctx.Rbp, ctx.Rax, ctx.Rbx, ctx.Rcx, ctx.Rdx
        );
    }

    let tid = crate::win32::ffi::GetCurrentThreadId();
    let breadcrumbs = get_recent_breadcrumbs();
    let bt = std::backtrace::Backtrace::force_capture();
    let now_str = Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string();

    let report = format!(
        "\n================================================================================\n\
         [CRASH REPORT NATIVE WINDOWS - {}] Waktu: {}\n\
         ================================================================================\n\
         1. KODE & JENIS CRASH : 0x{:08X} - {}\n\
         2. PENJELASAN MASALAH : {}\n\
         3. MODUL PENYEBAB     : {} (Offset: +0x{:X})\n\
            Lokasi File Penuh  : {}\n\
            Alamat Instruksi   : 0x{:016X} (Base Modul: 0x{:016X})\n\
            Thread ID Windows  : {}\n\
         4. REGISTER CPU x64   :\n{}\n\
         5. RIWAYAT AKTIVITAS TERAKHIR SEBELUM CRASH (BREADCRUMBS):\n{}\n\
         6. CALL STACK / BACKTRACE :\n{}\n\
         ================================================================================\n",
        handler_kind,
        now_str,
        code,
        exc_name,
        exc_desc,
        mod_short,
        mod_offset,
        mod_path,
        addr,
        mod_base,
        tid,
        regs_str,
        breadcrumbs,
        bt
    );

    crate::win32::reset_gpu_boost_clock_lock_on_exit();
    write_crash_report_to_disk(&report);
}

/// Vectored Exception Handler (dipasang di urutan pertama NTDLL agar tidak bisa ditimpa oleh DLL VST3/OpenGL).
#[cfg(windows)]
unsafe extern "system" fn vectored_seh_crash_filter(
    ep: *mut crate::win32::ffi::EXCEPTION_POINTERS,
) -> i32 {
    log_ep_if_fatal(ep, "VECTORED SEH");
    0 // EXCEPTION_CONTINUE_SEARCH
}

/// Fallback Top-Level Unhandled Exception Filter.
#[cfg(windows)]
unsafe extern "system" fn native_seh_crash_filter(
    ep: *mut crate::win32::ffi::EXCEPTION_POINTERS,
) -> i32 {
    log_ep_if_fatal(ep, "UNHANDLED SEH");
    crate::win32::ffi::EXCEPTION_EXECUTE_HANDLER
}

pub fn install_crash_handlers() {
    init_logger();
    let now_boot = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

    for log_dir in get_all_log_dirs() {
        let _ = fs::create_dir_all(&log_dir);
        let crash_path = log_dir.join("crash_native.txt");

        let need_reset = if crash_path.exists() {
            match fs::read_to_string(&crash_path) {
                Ok(content) => {
                    let has_real_crash =
                        content.contains("[CRASH REPORT") || content.contains("Panic di thread");
                    let too_large = content.len() > 512 * 1024;
                    !has_real_crash || too_large
                }
                Err(_) => true,
            }
        } else {
            true
        };

        if need_reset {
            let header = format!(
                "# ==============================================================================\n\
                 # Rinka SoundBridge v2.0 - Log Diagnostik Crash (Vectored SEH & Rust Panic)\n\
                 # Status Handler : AKTIF (Sesi Terakhir Dimulai: {})\n\
                 # Lokasi Log     : {}\n\
                 # Catatan        : Jika tidak ada blok [CRASH REPORT] di bawah baris ini,\n\
                 #                  berarti sesi berjalan normal tanpa crash.\n\
                 # ==============================================================================\n",
                now_boot,
                crash_path.display()
            );
            let _ = fs::write(&crash_path, header.as_bytes());
        } else if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&crash_path) {
            let _ = writeln!(
                f,
                "\n# [INFO] Sesi baru dimulai pada {} (Handler Crash v2.0 Aktif)",
                now_boot
            );
        }
    }

    // 1. Pasang Vectored Exception Handler (Prioritas Pertama di NTDLL - tidak bisa ditimpa oleh DLL VST3!)
    //    serta Unhandled Exception Filter sebagai cadangan.
    #[cfg(windows)]
    unsafe {
        let _ = crate::win32::ffi::AddVectoredExceptionHandler(1, Some(vectored_seh_crash_filter));
        let _ = crate::win32::ffi::SetUnhandledExceptionFilter(Some(native_seh_crash_filter));
    }

    // 2. Pasang penangkap Panic Rust lengkap dengan Backtrace & Breadcrumbs
    std::panic::set_hook(Box::new(move |info| {
        let bt = std::backtrace::Backtrace::force_capture();
        let thread_name = std::thread::current().name().unwrap_or("unknown").to_string();
        let breadcrumbs = get_recent_breadcrumbs();
        let now_str = Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string();

        let location_str = if let Some(loc) = info.location() {
            format!("{}:{}:{}", loc.file(), loc.line(), loc.column())
        } else {
            "(lokasi tidak diketahui)".to_string()
        };

        let report = format!(
            "\n================================================================================\n\
             [CRASH REPORT RUST PANIC] Waktu: {}\n\
             ================================================================================\n\
             1. THREAD PENYEBAB    : '{}'\n\
             2. LOKASI KODE SUMBER : {}\n\
             3. PESAN PANIC        : {}\n\
             4. RIWAYAT AKTIVITAS TERAKHIR SEBELUM CRASH (BREADCRUMBS):\n{}\n\
             5. BACKTRACE          :\n{}\n\
             ================================================================================\n",
            now_str, thread_name, location_str, info, breadcrumbs, bt
        );

        write_crash_report_to_disk(&report);
    }));
}

/// Pastikan Unhandled Exception Filter kita tetap berada di puncak setelah memuat DLL VST3 pihak ketiga.
pub fn reassert_crash_handlers() {
    #[cfg(windows)]
    unsafe {
        let _ = crate::win32::ffi::SetUnhandledExceptionFilter(Some(native_seh_crash_filter));
    }
}
