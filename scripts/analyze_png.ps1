param([Parameter(Mandatory=$true)][string]$Path)
Add-Type -AssemblyName System.Drawing
$bmp = New-Object System.Drawing.Bitmap $Path
$w = $bmp.Width; $h = $bmp.Height
$colors = @{}
$step = 3
for ($y = 0; $y -lt $h; $y += $step) {
    for ($x = 0; $x -lt $w; $x += $step) {
        $c = $bmp.GetPixel($x, $y)
        $key = "{0:X2}{1:X2}{2:X2}" -f $c.R, $c.G, $c.B
        if ($colors.ContainsKey($key)) { $colors[$key]++ } else { $colors[$key] = 1 }
    }
}
"size ${w}x${h}"
"distinct colors (sampled): " + $colors.Count
"top 12 colors:"
$colors.GetEnumerator() | Sort-Object Value -Descending | Select-Object -First 12 |
    ForEach-Object { "  #{0}  x{1}" -f $_.Key, $_.Value }
$bmp.Dispose()
