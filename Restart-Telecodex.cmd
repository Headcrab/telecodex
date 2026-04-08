@echo off
setlocal
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\Restart-Telecodex.ps1"
echo.
pause
