@echo off
setlocal
cd /d "%~dp0"

echo ============================================================================
echo   Rinka SoundBridge v2.0 - Test & Verification Runner
echo ============================================================================
echo.

if not exist "dist\RinkaSoundBridge.exe" (
    echo [INFO] Binary dist\RinkaSoundBridge.exe not found yet.
    echo Building the application first via build.bat...
    echo.
    call "%~dp0build.bat"
)

if exist "dist\RinkaSoundBridge.exe" (
    echo.
    echo ============================================================================
    echo [OK] Launching Rinka SoundBridge v2.0 for live audio testing...
    echo ============================================================================
    start "" "dist\RinkaSoundBridge.exe"
    exit /b 0
) else (
    echo.
    echo [ERROR] Build failed or dist\RinkaSoundBridge.exe could not be created.
    echo Please review the build errors above.
    pause
    exit /b 1
)
