@echo off
rem The launcher. A double-click opens its window and leaves no console window open; with
rem arguments (launcher.cmd -Get, -Set ray_tracing=on, -Check ...) it prints here.
if "%~1"=="" (
    start "" powershell.exe -NoProfile -ExecutionPolicy Bypass -STA -WindowStyle Hidden -File "%~dp0scripts\launcher.ps1"
    exit /b 0
)
powershell.exe -NoProfile -ExecutionPolicy Bypass -STA -File "%~dp0scripts\launcher.ps1" %*
exit /b %errorlevel%
