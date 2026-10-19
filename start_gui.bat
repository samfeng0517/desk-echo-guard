@echo off
REM Double-click to open the Desk Echo Guard settings window (no console).
cd /d "%~dp0"
if exist ".venv\Scripts\pythonw.exe" (
  start "" ".venv\Scripts\pythonw.exe" -m echoguard.gui
) else (
  start "" pyw -m echoguard.gui
)
