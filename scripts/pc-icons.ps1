# Draws the PC key icons that take the place of the pad's buttons (keys from bindings.ini), 128x128 with
# transparency, into scripts\pc-icons, and a sheet of all of them (scripts\pc-icons-sheet.png) to look at.
# The texture mods (VICTORY_TEXTURE_MODS) are made from these once the title's own button icons are dumped.
Add-Type -AssemblyName System.Drawing
$out = Join-Path $PSScriptRoot 'pc-icons'
New-Item -ItemType Directory -Force $out | Out-Null
# button = (key text, wide keycap, mouse)
$keys = [ordered]@{
    'A' = @('Enter', $true, $false); 'B' = @('Backspace', $true, $false); 'X' = @('Space', $true, $false); 'Y' = @('F', $false, $false)
    'LB' = @('Tab', $true, $false); 'RB' = @('Q', $false, $false); 'LT' = @('R', $false, $true); 'RT' = @('L', $false, $true)
    'LS' = @('Ctrl', $true, $false); 'RS' = @('M', $false, $true); 'Menu' = @('Esc', $false, $false); 'View' = @('V', $false, $false)
    'DpadUp' = @([string][char]0x2191, $false, $false); 'DpadDown' = @([string][char]0x2193, $false, $false)
    'DpadLeft' = @([string][char]0x2190, $false, $false); 'DpadRight' = @([string][char]0x2192, $false, $false)
    'LStick' = @('WASD', $true, $false); 'RStick' = @('', $false, $true)
}
function New-Round([single]$x, [single]$y, [single]$w, [single]$h, [single]$r) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $p.AddArc($x, $y, $r * 2, $r * 2, 180, 90); $p.AddArc($x + $w - $r * 2, $y, $r * 2, $r * 2, 270, 90)
    $p.AddArc($x + $w - $r * 2, $y + $h - $r * 2, $r * 2, $r * 2, 0, 90); $p.AddArc($x, $y + $h - $r * 2, $r * 2, $r * 2, 90, 90)
    $p.CloseFigure(); $p
}
# White keys with dark marks, as the console's own button icons are drawn.
$white = [System.Drawing.Color]::FromArgb(255, 245, 245, 245)
$dark = [System.Drawing.Color]::FromArgb(255, 24, 24, 28)
foreach ($name in $keys.Keys) {
    $text, $wide, $mouse = $keys[$name]
    $bmp = New-Object System.Drawing.Bitmap 128, 128, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = 'AntiAlias'; $g.TextRenderingHint = 'AntiAliasGridFit'; $g.Clear([System.Drawing.Color]::Transparent)
    $pen = New-Object System.Drawing.Pen $dark, 6
    if ($mouse) {
        # A mouse: body, the line between the buttons, the pressed button filled (L, R, M = wheel).
        $body = New-Round 30 10 68 108 34
        $g.FillPath((New-Object System.Drawing.SolidBrush $white), $body)
        # The marks inside a white rim (the body inset), so the outline stays whole and nothing bleeds past it.
        $inner = New-Round 37 17 54 94 27
        $g.SetClip($inner)
        $fill = New-Object System.Drawing.SolidBrush $dark
        switch ($text) {
            'L' { $g.FillRectangle($fill, 37, 17, 27, 37) }
            'R' { $g.FillRectangle($fill, 64, 17, 27, 37) }
        }
        $g.ResetClip()
        $g.SetClip($body)
        $g.DrawLine($pen, 64, 10, 64, 54); $g.DrawLine($pen, 30, 54, 98, 54)
        $g.ResetClip()
        # The wheel, on the line between the buttons.
        if ($text -eq 'M') { $g.FillPath($fill, (New-Round 56 20 16 26 8)) }
    } else {
        $w = if ($wide) { 120 } else { 104 }
        $x = (128 - $w) / 2
        $cap = New-Round $x 12 $w 104 22
        $g.FillPath((New-Object System.Drawing.SolidBrush $white), $cap)
        $size = switch ($text.Length) { { $_ -le 1 } { 54 } 2 { 44 } 3 { 34 } 4 { 28 } 5 { 24 } default { 19 } }
        $font = New-Object System.Drawing.Font 'Segoe UI Semibold', $size, ([System.Drawing.FontStyle]::Bold), ([System.Drawing.GraphicsUnit]::Pixel)
        $fmt = New-Object System.Drawing.StringFormat; $fmt.Alignment = 'Center'; $fmt.LineAlignment = 'Center'
        $g.DrawString($text, $font, (New-Object System.Drawing.SolidBrush $dark), (New-Object System.Drawing.RectangleF 0, 14, 128, 104), $fmt)
    }
    $bmp.Save("$out\$name.png", [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose(); $bmp.Dispose()
}
# The sheet: each icon over the game's dark interface grey, its pad button under it.
$names = @($keys.Keys)
$cols = 6; $rows = [math]::Ceiling($names.Count / $cols)
$sheet = New-Object System.Drawing.Bitmap ($cols * 160), ($rows * 170)
$g = [System.Drawing.Graphics]::FromImage($sheet)
$g.Clear([System.Drawing.Color]::FromArgb(255, 40, 38, 48)); $g.TextRenderingHint = 'AntiAliasGridFit'
$label = New-Object System.Drawing.Font 'Segoe UI', 14, ([System.Drawing.GraphicsUnit]::Pixel)
for ($i = 0; $i -lt $names.Count; $i++) {
    $x = ($i % $cols) * 160 + 16; $y = [math]::Floor($i / $cols) * 170 + 8
    $icon = [System.Drawing.Image]::FromFile("$out\$($names[$i]).png")
    $g.DrawImage($icon, $x, $y, 128, 128); $icon.Dispose()
    $g.DrawString($names[$i], $label, [System.Drawing.Brushes]::Gainsboro, $x + 40, $y + 134)
}
$sheet.Save("$out-sheet.png", [System.Drawing.Imaging.ImageFormat]::Png)
"ok $($names.Count)"
