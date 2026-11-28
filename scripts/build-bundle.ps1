# Builds and packs one of the two downloads, released apart, each one executable:
#   scripts\build-bundle.ps1                 target\bundle\VI-BR-PORT-<version>-windows-x64.zip
#                                            (vi-launcher.exe alone: the engine is its own download)
#   scripts\build-bundle.ps1 -NoLauncher     target\bundle\VIctory-<version>-windows-x64.zip
#                                            (victory.exe alone: the engine, for the launcher or play.ps1)
# -Channel   stable (the plain names: what a release of main ships) or dev (the default here:
#            "dev-" before the file names, and the build calls itself dev in its titles)
# -Version   the version in the file names (default: Cargo.toml's)
# -Built     use what target\release already has (the release workflow builds first)
# -Out       the folder the zip goes to (default: target\bundle)
param(
    [string]$Version,
    [switch]$NoLauncher,
    [switch]$Built,
    [ValidateSet('stable', 'dev')][string]$Channel = 'dev',
    [string]$Out
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
if (-not $Version) {
    $section = $null
    foreach ($line in [System.IO.File]::ReadAllLines((Join-Path $root 'Cargo.toml'))) {
        $t = $line.Trim()
        if ($t -match '^\[(.+)\]$') { $section = $Matches[1]; continue }
        if ($section -eq 'workspace.package' -and $t -match '^version\s*=\s*"([^"]+)"') { $Version = $Matches[1]; break }
    }
}
if (-not $Out) { $Out = Join-Path $root 'target\bundle' }
if (-not [System.IO.Path]::IsPathRooted($Out)) { $Out = Join-Path $root $Out }

# The builds read VI_CHANNEL when they are compiled (their titles say dev).
if ($Channel -eq 'dev') { $env:VI_CHANNEL = 'dev' } else { Remove-Item Env:VI_CHANNEL -ErrorAction SilentlyContinue }
if (-not $Built) {
    $packages = if ($NoLauncher) { @('-p', 'victory') } else { @('-p', 'vi-launcher') }
    Push-Location $root
    try { cargo build --locked --release @packages; if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE } } finally { Pop-Location }
}

$release = Join-Path $root 'target\release'
$name = if ($NoLauncher) { "VIctory-$Version-windows-x64" } else { "VI-BR-PORT-$Version-windows-x64" }
if ($Channel -eq 'dev') { $name = "dev-$name" }
$stage = Join-Path $Out $name
if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Recurse -Force }
New-Item -ItemType Directory -Force $stage | Out-Null
if ($NoLauncher) {
    Copy-Item -LiteralPath (Join-Path $release 'victory.exe') $stage
} else {
    Copy-Item -LiteralPath (Join-Path $release 'vi-launcher.exe') $stage
}
# The executable and its licence; how to use it is in the repository's README.
Copy-Item -LiteralPath (Join-Path $root 'LICENSE') $stage
$zip = Join-Path $Out "$name.zip"
if (Test-Path -LiteralPath $zip) { Remove-Item -LiteralPath $zip -Force }
Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zip
Remove-Item -LiteralPath $stage -Recurse -Force
Write-Host $zip
