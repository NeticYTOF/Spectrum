$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class WinEnum {
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    public delegate bool EnumWindowsProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc cb, IntPtr l);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, System.Text.StringBuilder t, int n);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetClassName(IntPtr h, System.Text.StringBuilder t, int n);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
}
"@
[WinEnum]::SetProcessDPIAware() | Out-Null

$found = @()
$cb = {
    param($h, $l)
    if ([WinEnum]::IsWindowVisible($h)) {
        $sb = New-Object System.Text.StringBuilder 512
        [WinEnum]::GetWindowText($h, $sb, 512) | Out-Null
        $title = $sb.ToString()
        if ($title -ne '') {
            $cn = New-Object System.Text.StringBuilder 256
            [WinEnum]::GetClassName($h, $cn, 256) | Out-Null
            $pid2 = 0
            [WinEnum]::GetWindowThreadProcessId($h, [ref]$pid2) | Out-Null
            $script:found += [PSCustomObject]@{ Hwnd = $h; Title = $title; Class = $cn.ToString(); Pid = $pid2 }
        }
    }
    return $true
}
[WinEnum]::EnumWindows($cb, [IntPtr]::Zero) | Out-Null
$found | ForEach-Object { "{0}|{1}|{2}|{3}" -f $_.Hwnd, $_.Pid, $_.Class, $_.Title }
