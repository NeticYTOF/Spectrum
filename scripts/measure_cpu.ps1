# Measures spectrum.exe CPU usage over N seconds while the demo runs.
param(
    [Parameter(Mandatory=$true)][string]$Exe,
    [int]$Seconds = 6
)

$wrapper = Join-Path $env:TEMP 'spectrum_cpu_probe.cmd'
@"
@echo off
title SPECTRUM-CPU
"$Exe" %*
"@ | Set-Content -Path $wrapper -Encoding ASCII

Get-Process spectrum -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 300
Start-Process -FilePath 'cmd.exe' -ArgumentList "/c `"$wrapper`" --demo --mode 1 --exit-after 40"
Start-Sleep -Seconds 2   # let it start rendering

$p = Get-Process spectrum -ErrorAction Stop
$a = $p.TotalProcessorTime.TotalMilliseconds
$s1 = (Get-Date)
Start-Sleep -Seconds $Seconds
$p.Refresh()
$b = $p.TotalProcessorTime.TotalMilliseconds
$elapsed = ((Get-Date) - $s1).TotalSeconds
$cpuPct = 100.0 * (($b - $a) / 1000.0) / $elapsed / [Environment]::ProcessorCount
"active: {0:N1}% of one core avg over {1:N1}s (proc {2})" -f $cpuPct, $elapsed, $p.Id

# Memory bound check
$p.Refresh()
"working set: {0:N1} MB" -f ($p.WorkingSet64 / 1MB)

# Clean up
Stop-Process -Id $p.Id -Force
