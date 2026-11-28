# The launcher's version: Cargo.toml's [workspace.package] version and, when git and the
# repository are there, the short commit.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# $null when Cargo.toml is not there or cannot be read: only the version row says so.
function Get-LauncherWorkspaceVersion {
    $section = $null
    $lines = @()
    try {
        $lines = [System.IO.File]::ReadAllLines((Join-Path $script:LauncherRoot 'Cargo.toml'))
    } catch {
        return $null
    }
    foreach ($line in $lines) {
        $trimmed = $line.Trim()
        if ($trimmed -match '^\[(.+)\]$') { $section = $Matches[1].Trim(); continue }
        if ($section -eq 'workspace.package' -and $trimmed -match '^version\s*=\s*"([^"]+)"') { return $Matches[1] }
    }
    return $null
}

function Get-LauncherCommit {
    if ($null -eq (Get-Command git -ErrorAction SilentlyContinue)) { return $null }
    # (Windows PowerShell turns a native program's error output into an error under 'Stop'.)
    $ErrorActionPreference = 'Continue'
    try {
        $out = & git -C $script:LauncherRoot rev-parse --short HEAD 2>$null
        if ($LASTEXITCODE -ne 0 -or -not $out) { return $null }
        return ([string]@($out)[0]).Trim()
    } catch {
        return $null
    }
}

# { version (or $null), commit (or $null), text: "v<version> (<commit>)" or "v<version>", with
# "?" for a version that could not be read }.
function Get-LauncherVersion {
    param([string]$Language)
    $version = Get-LauncherWorkspaceVersion
    $commit = Get-LauncherCommit
    $shown = $version
    if (-not $shown) { $shown = '?' }
    $info = [ordered]@{}
    $info['version'] = $version
    $info['commit'] = $commit
    if ($commit) { $info['text'] = Get-LauncherText -Key 'panel.version_commit' -Language $Language -Arguments @($shown, $commit) }
    else { $info['text'] = Get-LauncherText -Key 'panel.version' -Language $Language -Arguments @($shown) }
    return $info
}
