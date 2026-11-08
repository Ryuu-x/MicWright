@echo off
rem Double-click launcher: builds (if needed) and starts MicWright.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0run.ps1" %*
