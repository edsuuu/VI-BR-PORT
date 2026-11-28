# Starts the title with the settings this fork is measured with (README.md, "How to test it with
# the frame rate we get").
#   scripts\play.ps1 -Exe <path of your own copy's executable> -Build release
# -Safe         without the newer, faster ways (the first thing to try when something is wrong)
# -RayTracing   the title's own ray-traced indirect light: lit interiors, about a third slower
#               (its shaders are another translation: run `victory shaders` without VICTORY_RT_MISS)
# -Build        target\release (what the README's benchmark is of) or target\diag (the default)
# -Console      series-s (the default: the profile for 8 GB cards) or series-x
# -More         other settings, or other values for these: -More @{VICTORY_THIN='0'}
# Run it from a normal window, not as administrator: the driver then keeps its shader cache elsewhere.
param(
    [Parameter(Mandatory)][string]$Exe,
    [switch]$Safe,
    [switch]$RayTracing,
    [ValidateSet('diag', 'release')][string]$Build = 'diag',
    [ValidateSet('series-s', 'series-x')][string]$Console = 'series-s',
    [hashtable]$More = @{}
)
$settings = @{
    # The base: the title's own upscaler, 1080p, and the pipelines a run draws with written down
    # (`victory warm --used` makes them ahead of the next run).
    VICTORY_UPSCALER = '6'; VICTORY_MEMORY_CARD_MB = '1024'; VICTORY_DISPLAY_MODE = '1920x1080'
    VICTORY_PIPELINE_BOOK = '1'; VICTORY_NO_WATCHDOG = '1'; VICTORY_D3D12_DRED = 'light'; VICTORY_SKIP_PIPELINES = 'none'
    VICTORY_TEXTURE_NO_WAIT = '1'; VICTORY_RELAY = '2'; VICTORY_WIDE_BACK = '1'; VICTORY_CARD_REHASH_RUNS = '1'
    VICTORY_JOIN_COPIES = '1'; VICTORY_INDIRECT_WINDOW = '1'; VICTORY_RELAY_COPY = '1'; VICTORY_CARD_STEPS = '2'
    VICTORY_INDIRECT_BATCH = '2'; VICTORY_MAP_THREADS = '1'; VICTORY_TITLE_PRIORITIES = '0'; VICTORY_CARD_AWAKE = '0'
    VICTORY_SHOW_AFTER = '1'; VICTORY_CARD_BUFFERS_MIN_KB = '64'
    # The title's buffer of constants as a copy in the card's memory (the faster way on NVIDIA).
    VICTORY_PORT_CONSTANTS = '0'
    # The stand-in for the ray-traced indirect light: a faint constant inside rooms, raised where a
    # room is dark; outdoors the game's own light with a floor.
    VICTORY_AMBIENT = 'constant:0.1:0.5'; VICTORY_AMBIENT_OUTSIDE = '1:0.16'
    VICTORY_AMBIENT_KEY = '0.2:8:room:air:geo'; VICTORY_AMBIENT_AIR = '0.8'
    # Day and night outdoors: no whitish water by day.
    VICTORY_AMBIENT_DAY = '0.5:2'; VICTORY_AMBIENT_CONTRAST = '4:8'; VICTORY_AMBIENT_CUT = '3'
    VICTORY_AMBIENT_AIR_SOFT = '1'; VICTORY_AMBIENT_EDGE = '4'
    # Programs that need the paired 64-lane form; the last, the sky's CS_SortRayTiles, takes the
    # light blocks off the clouds and the rectangle off the sky by the bridge. Translation switch.
    VICTORY_WAVE_PAIRED_FOR = '5dc3b25986622949,6f225b641c5b0708,1aa247ed3653b616,a1bf269d5b19f025'
    # The glass of the cell window's prepass tests depth first, so it no longer draws over Lucia's
    # hair and shoulder. Translation switch.
    VICTORY_EARLY_DEPTH_FOR = 'd16aa6328e7f5025'
}
if ($Safe) {
    $settings.VICTORY_UNDER_WIDE = '0'; $settings.VICTORY_LAND_ECHO = '0'
} else {
    # Thin lists, the shared guard and the journal, the list of changed copies, the lean compare.
    $settings.VICTORY_THIN = '1'; $settings.VICTORY_JOIN_PARTS = '8'; $settings.VICTORY_UPLOAD_AHEAD = '1'
    $settings.VICTORY_PORT_GUARD = '1'; $settings.VICTORY_PORT_JOURNAL = '1'
    $settings.VICTORY_CARD_TOUCHED = '1'; $settings.VICTORY_CARD_COMPARE_EVERY = '30'
}
# Every ray test answers "nothing hit", unless the title's own ray tracing is asked for. The shader
# cache is keyed on this one: `victory shaders` has to run with the same value.
if ($RayTracing) { $settings.VICTORY_BVH_BUILDER = '1' } else { $settings.VICTORY_RT_MISS = '1' }
foreach ($k in $More.Keys) { $settings[$k] = $More[$k] }

$victory = Join-Path $PSScriptRoot "..\target\$Build\victory.exe"
if (-not (Test-Path $victory)) { Write-Host "No $victory. Build it first (README.md)."; return }
if (-not (Test-Path $Exe)) { Write-Host "No $Exe."; return }
# (Other settings left over in this window go; where your files are stays.)
$paths = 'VICTORY_CONFIG', 'VICTORY_GAME_DIR', 'VICTORY_CACHE_DIR', 'VICTORY_DATA_DIR'
Get-ChildItem Env:VICTORY_* | Where-Object { $_.Name -notin $paths } | ForEach-Object { Remove-Item "Env:$($_.Name)" }
foreach ($k in $settings.Keys) { Set-Item "Env:$k" $settings[$k] }
$Exe = (Resolve-Path $Exe).Path
Push-Location (Join-Path $PSScriptRoot '..')   # victory.toml is looked for here
try { & $victory run $Exe --console $Console --execute } finally { Pop-Location }
