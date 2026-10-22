@echo off
setlocal
cd /d "%~dp0"

echo ============================================================================
echo   Rinka SoundBridge v2.0 - FULL RUST NATIVE BUILDER (MSVC x86_64)
echo   ASIO DSP RSB Pro Architecture + Zero-Blackout Audio Engine
echo ============================================================================
echo.

if exist "src\main.rs" goto :found_src
if exist "rinka_soundbridge_v2_rust\src\main.rs" (
    echo [INFO] Detected subfolder rinka_soundbridge_v2_rust, changing directory...
    cd rinka_soundbridge_v2_rust
    goto :found_src
)

echo [ERROR] File src\main.rs not found in folder:
echo   %CD%
echo.
echo CAUSE:
echo   The "src" folder has not been extracted into this directory!
echo.
echo SOLUTION:
echo   Please extract the ENTIRE contents of the ZIP archive so the "src" folder
echo   is located right next to "Cargo.toml" and "build.bat".
echo.
pause
exit /b 1

:found_src
where cargo >nul 2>nul
if %ERRORLEVEL% equ 0 goto :found_cargo

echo [ERROR] Cargo or Rust was not detected in your Windows PATH!
echo Please make sure Rust is installed from https://rustup.rs
echo If you just installed Rust, please restart your PC or open a new terminal.
echo.
pause
exit /b 1

:found_cargo
echo [1/4] Checking ASIO requirements (VC++ Runtime ^& FlexASIO Engine)...
if not exist "%SystemRoot%\System32\vcruntime140.dll" (
    echo [INFO] Installing Microsoft Visual C++ Redistributable x64...
    winget install --id Microsoft.VCRedist.2015+.x64 -e --accept-source-agreements --accept-package-agreements --silent >nul 2>nul
)
if not exist "%ProgramFiles%\FlexASIO\x64\FlexASIO.dll" (
    echo [INFO] FlexASIO Engine not detected, installing automatically via winget...
    winget install --id dechamps.FlexASIO -e --accept-source-agreements --accept-package-agreements --silent >nul 2>nul
)
echo [OK] Requirements check completed.
echo.

echo [2/4] Compiling RinkaSoundBridge.exe + Core Module (rinka_soundbridge_core.dll)...
echo This may take a few minutes on the first build while downloading crates...
echo.
taskkill /F /IM RinkaSoundBridge.exe >nul 2>nul
cargo build --release
if %ERRORLEVEL% neq 0 goto :compile_error

echo.
echo [3/4] Organizing dist\ folder layout (RinkaSoundBridge.exe, Savedata, Modules, Logs)...
if not exist "dist" mkdir "dist"
if not exist "dist\Savedata" mkdir "dist\Savedata"
if not exist "dist\Modules" mkdir "dist\Modules"
if not exist "dist\Logs" mkdir "dist\Logs"

REM 1. Copy RinkaSoundBridge.exe to dist\ and clean up any old root duplicate
copy /Y "target\release\RinkaSoundBridge.exe" "dist\RinkaSoundBridge.exe" >nul
if exist ".\RinkaSoundBridge.exe" del /F /Q ".\RinkaSoundBridge.exe" >nul 2>nul

REM 2. Copy core DLL module and icon to dist\Modules\
if exist "target\release\rinka_soundbridge_core.dll" (
    if exist "dist\Modules\rinka_soundbridge_core.dll.old" del /F /Q "dist\Modules\rinka_soundbridge_core.dll.old" >nul 2>nul
    if exist "dist\Modules\rinka_soundbridge_core.dll" move /Y "dist\Modules\rinka_soundbridge_core.dll" "dist\Modules\rinka_soundbridge_core.dll.old" >nul 2>nul
    copy /Y "target\release\rinka_soundbridge_core.dll" "dist\Modules\rinka_soundbridge_core.dll" >nul
    copy /Y "target\release\rinka_soundbridge_core.dll" "dist\rinka_soundbridge_core.dll" >nul 2>nul
    echo [OK] ASIO Core Module placed in dist\Modules\rinka_soundbridge_core.dll
)
if exist "icon.ico" (
    copy /Y "icon.ico" "dist\icon.ico" >nul
    copy /Y "icon.ico" "dist\Modules\icon.ico" >nul
)

REM 3. Place initial default configuration into dist\Savedata\
if not exist "dist\Savedata\rinka_soundbridge_config.json" (
    if exist "rinka_soundbridge_config.json" copy /Y "rinka_soundbridge_config.json" "dist\Savedata\rinka_soundbridge_config.json" >nul
    if exist "dist\rinka_soundbridge_config.json" copy /Y "dist\rinka_soundbridge_config.json" "dist\Savedata\rinka_soundbridge_config.json" >nul
)

REM 4. Clean up legacy loose files in dist root
if exist "dist\rinka_soundbridge_core.dll.old" del /F /Q "dist\rinka_soundbridge_core.dll.old" >nul 2>nul
if exist "dist\rinka_soundbridge_config.json.bak" del /F /Q "dist\rinka_soundbridge_config.json.bak" >nul 2>nul
if exist "dist\rinka_soundbridge_config.json.tmp" del /F /Q "dist\rinka_soundbridge_config.json.tmp" >nul 2>nul

echo.
echo [4/4] Registering ^& Synchronizing ASIO Drivers in Windows Registry...
set "DLL_ABS=%~dp0dist\Modules\rinka_soundbridge_core.dll"
reg add "HKCU\Software\Classes\CLSID\{D8E4A973-42B1-4C9A-9F31-8A7B6C5D4E03}" /ve /d "Rinka SoundBridge ASIO" /f >nul 2>nul
reg add "HKCU\Software\Classes\CLSID\{D8E4A973-42B1-4C9A-9F31-8A7B6C5D4E03}\InprocServer32" /ve /d "%DLL_ABS%" /f >nul 2>nul
reg add "HKCU\Software\Classes\CLSID\{D8E4A973-42B1-4C9A-9F31-8A7B6C5D4E03}\InprocServer32" /v "ThreadingModel" /d "Both" /f >nul 2>nul
reg add "HKCU\Software\Classes\CLSID\{D8E4A971-42B1-4C9A-9F31-8A7B6C5D4E01}" /ve /d "Rinka SoundBridge ASIO" /f >nul 2>nul
reg add "HKCU\Software\Classes\CLSID\{D8E4A971-42B1-4C9A-9F31-8A7B6C5D4E01}\InprocServer32" /ve /d "%DLL_ABS%" /f >nul 2>nul
reg add "HKCU\Software\Classes\CLSID\{D8E4A971-42B1-4C9A-9F31-8A7B6C5D4E01}\InprocServer32" /v "ThreadingModel" /d "Both" /f >nul 2>nul
reg add "HKCU\SOFTWARE\ASIO\Rinka SoundBridge ASIO" /v "CLSID" /d "{D8E4A973-42B1-4C9A-9F31-8A7B6C5D4E03}" /f >nul 2>nul
reg add "HKCU\SOFTWARE\ASIO\Rinka SoundBridge ASIO" /v "Description" /d "Rinka SoundBridge ASIO" /f >nul 2>nul
reg add "HKCU\Software\Classes\CLSID\{D8E4A975-42B1-4C9A-9F31-8A7B6C5D4E05}" /ve /d "RSB ASIO" /f >nul 2>nul
reg add "HKCU\Software\Classes\CLSID\{D8E4A975-42B1-4C9A-9F31-8A7B6C5D4E05}\InprocServer32" /ve /d "%DLL_ABS%" /f >nul 2>nul
reg add "HKCU\Software\Classes\CLSID\{D8E4A975-42B1-4C9A-9F31-8A7B6C5D4E05}\InprocServer32" /v "ThreadingModel" /d "Both" /f >nul 2>nul
reg add "HKCU\SOFTWARE\ASIO\RSB ASIO" /v "CLSID" /d "{D8E4A975-42B1-4C9A-9F31-8A7B6C5D4E05}" /f >nul 2>nul
reg add "HKCU\SOFTWARE\ASIO\RSB ASIO" /v "Description" /d "RSB ASIO" /f >nul 2>nul
"dist\RinkaSoundBridge.exe" --register-asio

echo.
echo ============================================================================
echo [DONE] Build ^& ASIO Driver Registration Completed Successfully!
echo   Main App: dist\RinkaSoundBridge.exe
echo   Savedata: dist\Savedata\
echo   Modules:  dist\Modules\rinka_soundbridge_core.dll
echo   Logs:     dist\Logs\
echo.
echo   IMPORTANT: If Vonovox or your DAW is currently open, please click "Stop"
echo   and "Reload Devices" (or restart Vonovox) so it loads the updated DLL!
echo ============================================================================
echo.
start "" "dist\RinkaSoundBridge.exe"
pause
exit /b 0

:compile_error
echo.
echo ============================================================================
echo [ERROR] Compilation failed!
echo Please check the error messages from Cargo above.
echo ============================================================================
echo.
pause
exit /b 1
