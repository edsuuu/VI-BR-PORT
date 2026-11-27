# Loads the launcher's core: dot-source this file (. scripts\launcher\Launcher.ps1) from the
# entry script, the window or the MCP server, then call the functions of the files below.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$script:LauncherDir = $PSScriptRoot
$script:LauncherRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..'))

foreach ($part in 'Json', 'Text', 'Config', 'Options', 'PlayScript', 'Version') {
    . (Join-Path $PSScriptRoot "$part.ps1")
}
