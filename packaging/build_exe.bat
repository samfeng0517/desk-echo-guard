@echo off
REM Build dist\DeskEchoGuard.exe locally (same command as the CI workflow).
cd /d "%~dp0\.."
python -m pip install -r requirements.txt -r requirements-siri.txt pyinstaller || exit /b 1
python -m PyInstaller --noconfirm --clean --onefile --windowed --name DeskEchoGuard ^
  --icon echoguard\assets\icon.ico --add-data "echoguard\assets;echoguard\assets" ^
  --paths . ^
  --collect-all livekit --collect-all pyaec --collect-data soundcard ^
  --collect-data openwakeword --hidden-import openwakeword.model ^
  --exclude-module torch --exclude-module matplotlib ^
  --exclude-module pytest --exclude-module IPython ^
  packaging\desk_echo_guard.py || exit /b 1
echo Built dist\DeskEchoGuard.exe
