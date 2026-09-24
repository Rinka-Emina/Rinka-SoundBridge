@echo off
setlocal
title Rinka SoundBridge - Launcher

echo ============================================================
echo   Menjalankan Rinka SoundBridge (Source Mode)
echo ============================================================
echo.

:: 1. Cek instalasi Python
python --version >nul 2>&1
if errorlevel 1 (
    echo [ERROR] Python belum terpasang di sistem atau belum masuk ke PATH Windows!
    echo Silakan install Python 3.10+ dari https://www.python.org/
    pause
    exit /b 1
)

:: 2. Cek dan install otomatis requirements jika belum ada
echo [1/2] Memeriksa dependensi modul...
python -c "import PyQt6, sounddevice, pedalboard, numpy" >nul 2>&1
if errorlevel 1 (
    echo [INFO] Menginstal modul yang dibutuhkan dari requirements.txt...
    pip install -r requirements.txt
    if errorlevel 1 (
        echo [ERROR] Gagal menginstal dependensi. Periksa koneksi internet Anda.
        pause
        exit /b 1
    )
)

:: 3. Jalankan aplikasi utama
echo [2/2] Membuka Rinka SoundBridge...
python rinka_soundbridge.py

:: Jika aplikasi ditutup normal atau terjadi error, tahan terminal
if errorlevel 1 (
    echo.
    echo [CRASH / ERROR] Aplikasi tertutup secara tidak normal.
    pause
)