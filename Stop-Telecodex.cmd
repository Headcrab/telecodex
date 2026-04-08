@echo off
setlocal
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\Stop-Telecodex.ps1"
echo.
pause
