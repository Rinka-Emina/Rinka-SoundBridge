@echo off
setlocal enabledelayedexpansion
title Rinka SoundBridge - Compiler (.exe)

echo ============================================================
echo   Rinka SoundBridge - PyInstaller Build Tool
echo ============================================================
echo.

:: 1. Cek Python terpasang di sistem
python --version >nul 2>&1
if errorlevel 1 (
    echo [ERROR] Python tidak terdeteksi di PATH Windows.
    echo         Pastikan Python sudah diinstal dan opsi "Add to PATH" dicentang.
    pause
    exit /b 1
)

:: 2. Auto-Install Dependencies dari requirements.txt
echo [1/4] Memeriksa dan menyinkronkan pustaka (requirements.txt)...
if exist "requirements.txt" (
    pip install -r requirements.txt
    if errorlevel 1 (
        echo.
        echo [ERROR] Gagal memasang dependensi dari requirements.txt.
        pause
        exit /b 1
    )
) else (
    echo [WARN] Berkas 'requirements.txt' tidak ditemukan. Memasang dependensi default...
    pip install pedalboard sounddevice numpy PyQt6 psutil
    if errorlevel 1 (
        echo.
        echo [ERROR] Gagal memasang dependensi audio/GUI.
        pause
        exit /b 1
    )
)

:: 3. Cek & Pasang PyInstaller
echo.
echo [2/4] Memeriksa paket compiler (PyInstaller)...
python -c "import PyInstaller" >nul 2>&1
if errorlevel 1 (
    echo [INFO] PyInstaller belum terpasang. Memasang PyInstaller sekarang...
    pip install pyinstaller
    if errorlevel 1 (
        echo.
        echo [ERROR] Gagal memasang PyInstaller.
        pause
        exit /b 1
    )
) else (
    echo [OK] PyInstaller sudah siap digunakan.
)

:: 4. Deteksi file icon
echo.
set ICON_ARG=
if exist "icon.ico" (
    echo [3/4] [OK] Berkas 'icon.ico' ditemukan! Menyematkan ikon kustom...
    set ICON_ARG=--icon="icon.ico"
) else (
    echo [3/4] [SKIP] Berkas 'icon.ico' tidak ditemukan. Menggunakan ikon bawaan.
)

:: 5. Proses Kompilasi PyInstaller
echo.
echo [4/4] Memulai proses compile ke file .exe tunggal...
echo       (Proses bundling C++ DSP & WASAPI memakan waktu 30 - 60 detik)
echo.

pyinstaller --noconfirm --onefile --windowed ^
    --name "RinkaSoundBridge" ^
    !ICON_ARG! ^
    --collect-all pedalboard ^
    --collect-all sounddevice ^
    --hidden-import=psutil ^
    rinka_soundbridge.py

if errorlevel 1 (
    echo.
    echo [ERROR] Proses kompilasi gagal. Periksa log error di atas.
    pause
    exit /b 1
)

echo.
echo ============================================================
echo [SELESAI] File executable berhasil dibuat tanpa error!
echo Lokasi: dist\RinkaSoundBridge.exe
echo ============================================================
echo.
pause