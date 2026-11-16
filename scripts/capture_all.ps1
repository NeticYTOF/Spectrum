param(
    [Parameter(Mandatory=$true)][string]$Exe,
    [Parameter(Mandatory=$true)][string]$OutDir,
    [string]$Title = "SPECTRUM-AUTO"
)
$ErrorActionPreference = 'Continue'
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

$shots = @(
    @{ Name = 'mode1-spectrum'; Args = '--demo --mode 1 --theme spectrum --exit-after 9' },
    @{ Name = 'mode2-spectrum'; Args = '--demo --mode 2 --theme spectrum --exit-after 9' },
    @{ Name = 'mode3-spectrum'; Args = '--demo --mode 3 --theme spectrum --exit-after 9' },
    @{ Name = 'mode4-spectrum'; Args = '--demo --mode 4 --theme fire --exit-after 9' },
    @{ Name = 'mode5-spectrum'; Args = '--demo --mode 5 --theme vapor --exit-after 9' },
    @{ Name = 'mode1-fire16';  Args = '--demo --mode 1 --theme fire --colors 16 --exit-after 9' },
    @{ Name = 'mode1-matrix';  Args = '--demo --mode 1 --theme matrix --exit-after 9' }
)

# A tiny cmd wrapper that sets the window title before launching the exe.
$wrapper = Join-Path $env:TEMP 'spectrum_autorun.cmd'
@"
@echo off
title $Title
"$Exe" %*
"@ | Set-Content -Path $wrapper -Encoding ASCII

foreach ($s in $shots) {
    # Kill any leftover instance, then launch fresh.
    Get-Process spectrum -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Milliseconds 400

    Start-Process -FilePath 'cmd.exe' -ArgumentList "/c `"$wrapper`" $($s.Args)"
    # The demo cycle: 0-4s build, 4-6s drop. Capture mid-drop.
    Start-Sleep -Seconds 5
    $out = Join-Path $OutDir ($s.Name + '.png')
    & powershell -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'capture_by_prefix.ps1') -TitlePrefix $Title -OutFile $out
    Start-Sleep -Seconds 5   # let it expire on its own
}
Get-Process spectrum -ErrorAction SilentlyContinue | Stop-Process -Force
Write-Output "done"
