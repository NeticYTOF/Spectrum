$ErrorActionPreference = 'Continue'
# One focused QA pass: garden at 144x42 and 72x22, frozen phase.
$exe = "C:\Github Code etc v2\Spectrum\target\release\spectrum.exe"
$out = "C:\Github Code etc v2\Spectrum\qa"
New-Item -ItemType Directory -Force -Path $out | Out-Null

$shots = @(
    @{ Name = 'garden-144x42'; Mode = '6' },
    @{ Name = 'garden-72x22'; Mode = '6' }
)

foreach ($s in $shots) {
    Get-Process spectrum -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Milliseconds 300
    # Frozen at the drop: deterministic phase, garden fully bloomed.
    Start-Process -FilePath 'cmd.exe' -ArgumentList "/c title SPECTRUM-QA && `"$exe`" --demo --mode $($s.Mode) --freeze-at 4.5 --exit-after 8"
    Start-Sleep -Seconds 3
    & powershell -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'capture_by_prefix.ps1') -TitlePrefix 'SPECTRUM-QA' -OutFile (Join-Path $out "$($s.Name).png")
    Start-Sleep -Seconds 6
}
Get-Process spectrum -ErrorAction SilentlyContinue | Stop-Process -Force
Write-Output "qa captures done"
