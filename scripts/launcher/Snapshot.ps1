# The window drawn off screen into a PNG, never shown: how the window is checked without
# taking the screen (scripts\launcher.ps1 -Snapshot <png> -View play|preparing|more
# [-Language en|pt-BR]). The Preparing view is drawn as Play would show it in its third step.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# What Invoke-LauncherPlay reports up to the warm-up step, for the Preparing view's picture.
function Get-LauncherSnapshotReports {
    $count = $script:LauncherSteps.Count
    $reports = @()
    foreach ($r in @(@('build', 1, 'done'), @('shaders', 2, 'done'), @('warm', 3, 'running'))) {
        $done = [int]$r[1] - 1
        if ($r[2] -eq 'done') { $done = [int]$r[1] }
        $reports += [ordered]@{ step = $r[0]; index = $r[1]; count = $count; state = $r[2]; message = $null; percent = [int][Math]::Round(100.0 * $done / $count) }
    }
    return $reports
}

function Save-LauncherWindowSnapshot {
    param([Parameter(Mandatory)][string]$Path, [ValidateSet('play', 'preparing', 'more', 'options')][string]$View = 'play', [string]$Language)
    $window = New-LauncherWindow -View $View -Language $Language
    $ui = $script:LauncherUi
    if ($ui.View -eq 'preparing') {
        Reset-LauncherPrepView
        foreach ($report in Get-LauncherSnapshotReports) { Update-LauncherPrepView -Report $report }
    }
    # The motion's end state: no night over the scene, the glow at its brightest.
    (Get-LauncherUiElement 'Night').Opacity = 0.0
    (Get-LauncherUiElement 'SunGlow').Opacity = [double](Get-LauncherToken 'motion.glowMax')
    $root = $ui.Root
    $window.Content = $null
    $width = [double](Get-LauncherToken 'size.windowWidth')
    $height = [double](Get-LauncherToken 'size.windowHeight')
    $root.Measure((New-Object System.Windows.Size ($width, $height)))
    $root.Arrange((New-Object System.Windows.Rect (0, 0, $width, $height)))
    $root.UpdateLayout()
    $bitmap = New-Object System.Windows.Media.Imaging.RenderTargetBitmap ([int]$width, [int]$height, 96.0, 96.0, [System.Windows.Media.PixelFormats]::Pbgra32)
    $bitmap.Render($root)
    $encoder = New-Object System.Windows.Media.Imaging.PngBitmapEncoder
    $encoder.Frames.Add([System.Windows.Media.Imaging.BitmapFrame]::Create($bitmap))
    $full = [System.IO.Path]::GetFullPath($Path)
    $dir = Split-Path -Parent $full
    if ($dir -and -not (Test-Path -LiteralPath $dir)) { New-Item -ItemType Directory -Path $dir -Force | Out-Null }
    $stream = [System.IO.File]::Create($full)
    try { $encoder.Save($stream) } finally { $stream.Dispose() }
    $window.Close()
    return $full
}
