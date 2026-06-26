@echo off
setlocal EnableExtensions EnableDelayedExpansion
title Cortex Portable Uninstaller

echo.
echo ============================================
echo   Cortex Portable Uninstaller
echo ============================================
echo.

set "INSTALL_DIR=%LOCALAPPDATA%\Cortex"
set "START_MENU=%APPDATA%\Microsoft\Windows\Start Menu\Programs"
set "START_LNK=%START_MENU%\Cortex.lnk"
set "DESKTOP_LNK=%USERPROFILE%\Desktop\Cortex.lnk"

REM --- Kill running cortex.exe ---
tasklist /FI "IMAGENAME eq cortex.exe" 2>nul | find /I "cortex.exe" >nul
if not errorlevel 1 (
  echo Stopping running Cortex ...
  taskkill /F /IM cortex.exe >nul 2>&1
  timeout /t 1 /nobreak >nul
)

REM --- Remove install dir ---
if exist "%INSTALL_DIR%" (
  echo Removing %INSTALL_DIR% ...
  rmdir /S /Q "%INSTALL_DIR%"
) else (
  echo Install directory not found; skipping.
)

REM --- Remove Start Menu shortcut ---
if exist "%START_LNK%" (
  echo Removing Start Menu shortcut ...
  del /F /Q "%START_LNK%" >nul 2>&1
)

REM --- Remove desktop shortcut ---
if exist "%DESKTOP_LNK%" (
  echo Removing desktop shortcut ...
  del /F /Q "%DESKTOP_LNK%" >nul 2>&1
)

echo.
echo ============================================
echo   Cortex uninstalled.
echo ============================================
echo.
echo User data preserved:
echo   %USERPROFILE%\.cortex
echo   %USERPROFILE%\.claude
echo   %USERPROFILE%\Documents\Cortex Brain
echo.
echo Delete those folders manually if you also want to wipe your data.
echo.
pause
endlocal
exit /b 0
