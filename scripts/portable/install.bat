@echo off
setlocal EnableExtensions EnableDelayedExpansion
title Cortex Portable Installer

echo.
echo ============================================
echo   Cortex Portable Installer
echo ============================================
echo.

set "SRC_EXE=%~dp0cortex.exe"
set "INSTALL_DIR=%LOCALAPPDATA%\Cortex"
set "DEST_EXE=%INSTALL_DIR%\cortex.exe"
set "START_MENU=%APPDATA%\Microsoft\Windows\Start Menu\Programs"
set "START_LNK=%START_MENU%\Cortex.lnk"
set "DESKTOP_LNK=%USERPROFILE%\Desktop\Cortex.lnk"

if not exist "%SRC_EXE%" (
  echo ERROR: cortex.exe not found next to this installer.
  echo Expected at: %SRC_EXE%
  echo.
  pause
  exit /b 1
)

REM --- Detect running cortex.exe ---
tasklist /FI "IMAGENAME eq cortex.exe" 2>nul | find /I "cortex.exe" >nul
if not errorlevel 1 (
  echo Cortex is currently running and must be closed to continue.
  set /p KILLOK=Close it now? (y/n^):
  if /I "!KILLOK!"=="y" (
    taskkill /F /IM cortex.exe >nul 2>&1
    timeout /t 1 /nobreak >nul
  ) else (
    echo Install cancelled.
    pause
    exit /b 1
  )
)

REM --- Create install directory ---
if not exist "%INSTALL_DIR%" (
  echo Creating %INSTALL_DIR% ...
  mkdir "%INSTALL_DIR%" || (
    echo ERROR: failed to create %INSTALL_DIR%
    pause
    exit /b 1
  )
)

REM --- Copy the executable ---
echo Copying cortex.exe to %INSTALL_DIR% ...
copy /Y "%SRC_EXE%" "%DEST_EXE%" >nul
if errorlevel 1 (
  echo ERROR: copy failed.
  pause
  exit /b 1
)

REM --- Create Start Menu shortcut ---
echo Creating Start Menu shortcut ...
powershell -NoProfile -ExecutionPolicy Bypass -Command ^
  "$ws = New-Object -ComObject WScript.Shell;" ^
  "$s = $ws.CreateShortcut('%START_LNK%');" ^
  "$s.TargetPath = '%DEST_EXE%';" ^
  "$s.WorkingDirectory = '%INSTALL_DIR%';" ^
  "$s.IconLocation = '%DEST_EXE%,0';" ^
  "$s.Description = 'Cortex desktop app';" ^
  "$s.Save()"

REM --- Optional desktop shortcut ---
set /p DESKOK=Create desktop shortcut? (y/n^):
if /I "!DESKOK!"=="y" (
  powershell -NoProfile -ExecutionPolicy Bypass -Command ^
    "$ws = New-Object -ComObject WScript.Shell;" ^
    "$s = $ws.CreateShortcut('%DESKTOP_LNK%');" ^
    "$s.TargetPath = '%DEST_EXE%';" ^
    "$s.WorkingDirectory = '%INSTALL_DIR%';" ^
    "$s.IconLocation = '%DEST_EXE%,0';" ^
    "$s.Description = 'Cortex desktop app';" ^
    "$s.Save()"
  echo Desktop shortcut created.
)

echo.
echo ============================================
echo   Cortex installed to %INSTALL_DIR%
echo ============================================
echo.
echo Launching Cortex ...
start "" "%DEST_EXE%"

echo.
echo You can uninstall later by running uninstall.bat from the original zip.
timeout /t 3 /nobreak >nul
endlocal
exit /b 0
