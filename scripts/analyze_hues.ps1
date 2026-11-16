param([Parameter(Mandatory=$true)][string]$Path, [string[]]$Find)
Add-Type -AssemblyName System.Drawing
$bmp = New-Object System.Drawing.Bitmap $Path
$colors = @{}
$step = 2
for ($y = 0; $y -lt $bmp.Height; $y += $step) {
    for ($x = 0; $x -lt $bmp.Width; $x += $step) {
        $c = $bmp.GetPixel($x, $y)
        $key = "{0:X2}{1:X2}{2:X2}" -f $c.R, $c.G, $c.B
        if ($colors.ContainsKey($key)) { $colors[$key]++ } else { $colors[$key] = 1 }
    }
}
"total sampled colors: " + $colors.Count
foreach ($f in $Find) {
    if ($colors.ContainsKey($f)) { "FOUND ${f}: " + $colors[$f] } else { "MISSING ${f}" }
}
# Cluster report: garden hues (blue-ish stems, pink-ish flowers)
$blue = 0; $pink = 0
foreach ($e in $colors.GetEnumerator()) {
    $r = [Convert]::ToInt32($e.Key.Substring(0,2),16)
    $g = [Convert]::ToInt32($e.Key.Substring(2,2),16)
    $b = [Convert]::ToInt32($e.Key.Substring(4,2),16)
    if ($b -gt ($r + 40) -and $b -gt ($g + 40) -and $b -gt 80) { $blue += $e.Value }
    if ($r -gt ($b + 40) -and $r -gt 150 -and $g -lt 150) { $pink += $e.Value }
}
"blue-cluster samples: $blue"
"pink-cluster samples: $pink"
$bmp.Dispose()
