# One process at a time reads or changes a file the window and the MCP server share: a named
# mutex per file (its full path, hashed), held around a read or a whole read-change-write so a
# save never drops another process's change and a read never meets a file being swapped in.
# The mutex is held by the thread, and taking it again on the same thread is allowed.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-LauncherLockName {
    param([Parameter(Mandatory)][string]$Path)
    $full = [System.IO.Path]::GetFullPath($Path).ToLowerInvariant()
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        $bytes = $sha.ComputeHash([System.Text.Encoding]::UTF8.GetBytes($full))
    } finally { $sha.Dispose() }
    $hex = (($bytes | ForEach-Object { $_.ToString('x2') }) -join '').Substring(0, 32)
    return "Local\VI-BR-PORT-launcher-$hex"
}

# Runs $Action holding the file's lock and returns what it returns. (The variables here have
# names an action will not use: the action sees its caller's variables.)
function Invoke-LauncherLocked {
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][scriptblock]$Action, [int]$TimeoutMs = 10000)
    $lockMutex = New-Object System.Threading.Mutex($false, (Get-LauncherLockName -Path $Path))
    $lockOwned = $false
    try {
        try {
            $lockOwned = $lockMutex.WaitOne($TimeoutMs)
        } catch [System.Threading.AbandonedMutexException] {
            # A process that held it ended without letting go: it is ours now.
            $lockOwned = $true
        }
        if (-not $lockOwned) {
            throw (Get-LauncherText -Key 'error.file_busy' -Language (Get-LauncherLanguage -Config $null) -Arguments @($Path))
        }
        & $Action
    } finally {
        if ($lockOwned) { $lockMutex.ReleaseMutex() }
        $lockMutex.Dispose()
    }
}
