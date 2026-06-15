Cortex - Portable Install
=========================

This zip is a no-admin, no-NSIS portable installer for Cortex.

Contents
--------
  cortex.exe       - the Cortex desktop app (Windows x64)
  install.bat      - copies cortex.exe to %LOCALAPPDATA%\Cortex,
                     creates a Start Menu shortcut, optionally a
                     desktop shortcut, then launches Cortex
  uninstall.bat    - removes the install + shortcuts
                     (your data in ~/.cortex, ~/.claude, and
                      Documents\Cortex Brain is left intact)
  README.txt       - this file

Install
-------
  1. Extract this zip anywhere (Desktop is fine).
  2. Double-click install.bat.
  3. When prompted, answer "y" to a desktop shortcut if you want one.
  4. Cortex launches automatically.

If Windows SmartScreen warns about the .bat or .exe:
  click "More info" then "Run anyway". The build is unsigned until
  a code-signing cert is attached.

If Cortex is already running, the installer will offer to close it
for you before swapping in the new build.

Uninstall
---------
  1. Keep the original extracted folder, or re-extract the zip.
  2. Double-click uninstall.bat.
  3. Your user data (chat history, brain notes, Claude config) is
     NOT touched - the script tells you where it lives so you can
     remove it manually if you also want a clean wipe.

Manual install (if the .bat is blocked)
---------------------------------------
  1. Create the folder:    %LOCALAPPDATA%\Cortex
  2. Copy cortex.exe into it.
  3. Pin it to Start Menu by right-clicking the exe.

Where things live
-----------------
  Install dir:       %LOCALAPPDATA%\Cortex\cortex.exe
  Start Menu link:   %APPDATA%\Microsoft\Windows\Start Menu\Programs\Cortex.lnk
  Desktop link:      %USERPROFILE%\Desktop\Cortex.lnk  (only if you opted in)

  User config:       %USERPROFILE%\.cortex
  Claude config:     %USERPROFILE%\.claude
  Brain notes:       %USERPROFILE%\Documents\Cortex Brain

Questions or bugs: open an issue at the Cortex repo.
