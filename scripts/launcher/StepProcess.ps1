# One step of Play run as a process: started directly (no cmd.exe between, so nothing in a
# path is read as %NAME% or as cmd syntax), with the line Format-LauncherCommandLine makes,
# and its output and errors written to the step's log by this process as they come.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# The environment of a step's process: this one's, without the VICTORY_ switches it may carry
# (as play.ps1 clears them), plus the step's own.
function New-LauncherProcessInfo {
    param([Parameter(Mandatory)]$Step)
    $info = New-Object System.Diagnostics.ProcessStartInfo
    $info.FileName = $Step['file']
    $info.Arguments = Format-LauncherCommandLine -Arguments @($Step['arguments'])
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $info.WorkingDirectory = $Step['cwd']
    foreach ($name in @($info.EnvironmentVariables.Keys)) {
        if ($name -like 'VICTORY_*' -and $name -notin $script:LauncherKeptPaths) { $info.EnvironmentVariables.Remove($name) }
    }
    foreach ($k in $Step['environment'].Keys) { $info.EnvironmentVariables[$k] = [string]$Step['environment'][$k] }
    return $info
}

function Stop-LauncherProcessTree {
    param([Parameter(Mandatory)][int]$Id)
    $ErrorActionPreference = 'Continue'
    & "$env:SystemRoot\System32\taskkill.exe" /PID $Id /T /F 2>&1 | Out-Null
}

function Get-LauncherLastLogLine {
    param([string]$Path)
    try {
        $stream = New-Object System.IO.FileStream($Path, 'Open', 'Read', 'ReadWrite')
        try {
            $start = [Math]::Max(0, $stream.Length - 4096)
            [void]$stream.Seek($start, 'Begin')
            $reader = New-Object System.IO.StreamReader($stream)
            $lines = $reader.ReadToEnd() -split "`r?`n" | Where-Object { $_.Trim() }
            if ($lines) { return [string]@($lines)[-1] }
        } finally { $stream.Dispose() }
    } catch { }
    return $null
}

# The output and the errors of a process copied into one log: a read is kept waiting on each,
# and what arrives is written as soon as Wait-LauncherLogPump sees it (on this thread only, so
# the two never write over each other).
function New-LauncherLogPump {
    param([Parameter(Mandatory)][System.Diagnostics.Process]$Process, [Parameter(Mandatory)][string]$Log)
    $logDir = Split-Path -Parent $Log
    if (-not (Test-Path -LiteralPath $logDir)) { New-Item -ItemType Directory -Path $logDir -Force | Out-Null }
    $pump = @{
        Log = New-Object System.IO.FileStream($Log, 'Create', 'Write', 'ReadWrite')
        Streams = @($Process.StandardOutput.BaseStream, $Process.StandardError.BaseStream)
        Buffers = @((New-Object byte[] 65536), (New-Object byte[] 65536))
        Reads = @($null, $null)
        Open = @($true, $true)
    }
    return $pump
}

# Waits up to $TimeoutMs for output, writes what came; returns whether a stream is still open.
function Wait-LauncherLogPump {
    param([Parameter(Mandatory)]$Pump, [int]$TimeoutMs = 250)
    $waiting = New-Object System.Collections.Generic.List[System.Threading.Tasks.Task]
    foreach ($i in 0, 1) {
        if (-not $Pump.Open[$i]) { continue }
        if ($null -eq $Pump.Reads[$i]) { $Pump.Reads[$i] = $Pump.Streams[$i].ReadAsync($Pump.Buffers[$i], 0, $Pump.Buffers[$i].Length) }
        $waiting.Add($Pump.Reads[$i])
    }
    if ($waiting.Count -eq 0) { return $false }
    [void][System.Threading.Tasks.Task]::WaitAny($waiting.ToArray(), $TimeoutMs)
    $wrote = $false
    foreach ($i in 0, 1) {
        $read = $Pump.Reads[$i]
        if ($null -eq $read -or -not $read.IsCompleted) { continue }
        $Pump.Reads[$i] = $null
        $count = 0
        if ($read.Status -eq 'RanToCompletion') { $count = $read.Result }
        if ($count -le 0) { $Pump.Open[$i] = $false; continue }
        $Pump.Log.Write($Pump.Buffers[$i], 0, $count)
        $wrote = $true
    }
    if ($wrote) { $Pump.Log.Flush() }
    return ($Pump.Open[0] -or $Pump.Open[1])
}

function Close-LauncherLogPump {
    param([Parameter(Mandatory)]$Pump)
    $Pump.Log.Dispose()
}

# Runs one step's process to its end; $State.Cancel = $true (from another thread) stops it.
function Invoke-LauncherStep {
    param([Parameter(Mandatory)]$Step, $Progress, $State)
    $process = [System.Diagnostics.Process]::Start((New-LauncherProcessInfo -Step $Step))
    $pump = $null
    try {
        $pump = New-LauncherLogPump -Process $process -Log $Step['log']
        $last = $null
        $exitedAt = $null
        while ($true) {
            $open = Wait-LauncherLogPump -Pump $pump
            if ($process.HasExited) {
                # Done once the output is all in, or a moment after the exit when a process it
                # started keeps the output open.
                if ($null -eq $exitedAt) { $exitedAt = [DateTime]::UtcNow }
                if (-not $open -or ([DateTime]::UtcNow - $exitedAt).TotalSeconds -gt 2) { break }
                continue
            }
            if ($null -ne $State -and $State['Cancel']) {
                Stop-LauncherProcessTree -Id $process.Id
                $process.WaitForExit()
                return 'cancelled'
            }
            $line = Get-LauncherLastLogLine -Path $Step['log']
            if ($line -and $line -ne $last) {
                $last = $line
                Send-LauncherProgress -Progress $Progress -Step $Step['id'] -State 'running' -Message $line
            }
            # (No output at all: Wait-LauncherLogPump returned at once with both streams shut.)
            if (-not $open) { [void]$process.WaitForExit(250) }
        }
        $process.WaitForExit()
        return $process.ExitCode
    } finally {
        if ($null -ne $pump) { Close-LauncherLogPump -Pump $pump }
        $process.Dispose()
    }
}
