param(
    [Parameter(Mandatory=$true)][string]$TitlePrefix,
    [Parameter(Mandatory=$true)][string]$OutFile
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class WinCap2 {
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    public delegate bool EnumWindowsProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc cb, IntPtr l);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, System.Text.StringBuilder t, int n);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
    public struct RECT { public int Left, Top, Right, Bottom; }
}
"@
[WinCap2]::SetProcessDPIAware() | Out-Null

$target = [IntPtr]::Zero
$cb = {
    param($h, $l)
    if ([WinCap2]::IsWindowVisible($h)) {
        $sb = New-Object System.Text.StringBuilder 512
        [WinCap2]::GetWindowText($h, $sb, 512) | Out-Null
        if ($sb.ToString().StartsWith($TitlePrefix)) {
            $script:target = $h
            return $false
        }
    }
    return $true
}
[WinCap2]::EnumWindows($cb, [IntPtr]::Zero) | Out-Null
if ($target -eq [IntPtr]::Zero) { throw "no visible window with title prefix '$TitlePrefix'" }

[WinCap2]::ShowWindow($target, 9) | Out-Null   # SW_RESTORE
[WinCap2]::SetForegroundWindow($target) | Out-Null
Start-Sleep -Milliseconds 900
$r = New-Object WinCap2+RECT
[WinCap2]::GetWindowRect($target, [ref]$r) | Out-Null
$w = $r.Right - $r.Left
$hgt = $r.Bottom - $r.Top
if ($w -le 0 -or $hgt -le 0) { throw "bad rect ${w}x${hgt}" }

$bmp = New-Object System.Drawing.Bitmap $w, $hgt
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($r.Left, $r.Top, 0, 0, $bmp.Size)
$g.Dispose()
$bmp.Save($OutFile, [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()
Write-Output ("saved {0} ({1}x{2}, hwnd {3})" -f $OutFile, $w, $hgt, $target)
