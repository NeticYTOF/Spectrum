@echo off
REM Sequential demo screenshot automation: one window per mode.
REM Usage: capture_all.cmd <exe-path> <out-dir>
setlocal
set EXE=%~1
set OUTDIR=%~2
set SCRIPTS=C:\Github Code etc v2\Spectrum\scripts

call :shot mode1 --mode 1 --theme spectrum
call :shot mode2 --mode 2 --theme spectrum
call :shot mode3 --mode 3 --theme spectrum
call :shot mode4 --mode 4 --theme spectrum
call :shot mode5 --mode 5 --theme fire
call :shot mode1-fire --mode 1 --theme fire --colors 16
exit /b 0

:shot
set NAME=%~1
shift
set REST=%*
start "" cmd /c ""%EXE%" --demo --exit-after 12 %REST% || pause"
timeout /t 5 /nobreak >nul
powershell -ExecutionPolicy Bypass -File "%SCRIPTS%\capture_by_prefix.ps1" -TitlePrefix "SPECTRUM" -OutFile "%OUTDIR%\%NAME%.png"
echo captured %NAME%
timeout /t 8 /nobreak >nul
exit /b 0
