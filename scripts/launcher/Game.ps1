# What "Play" does, in four steps that report through a callback:
#   build    target\<build>\victory.exe must be there (it is never built here: the message says
#            which cargo command builds it);
#   shaders  `victory shaders <the game's folder> --jobs N`, when victory.exe or one of the
#            switches the shader cache is keyed on changed since the last time (README.md);
#   warm     `victory warm --used --jobs N`, after a run drew with pipelines not made ahead yet
#            or after the shaders were translated again;
#   open     scripts\play.ps1 with the resolved parameters and switches; it returns when the
#            game exits.
# What was done is kept in prepared.json next to the saved settings. Get-LauncherPlayPlan only
# describes the steps (a dry run: nothing is started); Invoke-LauncherPlay runs them.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$script:LauncherSteps = @('build', 'shaders', 'warm', 'open')
# The folders play.ps1 leaves alone when it clears the VICTORY_ switches of its window.
$script:LauncherKeptPaths = @('VICTORY_CONFIG', 'VICTORY_GAME_DIR', 'VICTORY_CACHE_DIR', 'VICTORY_DATA_DIR')

function Get-LauncherVictoryPath {
    param([Parameter(Mandatory)][string]$Build)
    return (Join-Path $script:LauncherRoot "target\$Build\victory.exe")
}

function Get-LauncherBuildCommand {
    param([Parameter(Mandatory)][string]$Build)
    if ($Build -eq 'diag') { return 'cargo build --locked --profile diag -p victory --features diagnostics' }
    return 'cargo build --locked --release -p victory'
}

function Get-LauncherStampPath {
    return (Join-Path (Get-LauncherDataDir) 'prepared.json')
}

function Get-LauncherStamp {
    $path = Get-LauncherStampPath
    $stamp = Invoke-LauncherLocked -Path $path -Action {
        if (Test-Path -LiteralPath $path) {
            try { Read-LauncherJson -Path $path } catch { }
        }
    }
    if ($stamp -is [System.Collections.IDictionary]) { return $stamp }
    return [ordered]@{ format = 1 }
}

# One build's part of the stamp, written back at once.
function Set-LauncherStampEntry {
    param([Parameter(Mandatory)][string]$Build, [Parameter(Mandatory)][hashtable]$Values)
    Invoke-LauncherLocked -Path (Get-LauncherStampPath) -Action {
        $stamp = Get-LauncherStamp
        if (-not $stamp.Contains($Build) -or $stamp[$Build] -isnot [System.Collections.IDictionary]) { $stamp[$Build] = [ordered]@{} }
        foreach ($k in $Values.Keys) { $stamp[$Build][$k] = $Values[$k] }
        Write-LauncherJson -Path (Get-LauncherStampPath) -Data $stamp
    }
}

function Get-LauncherNow {
    return [long][Math]::Floor(([DateTime]::UtcNow - [DateTime]'1970-01-01').TotalSeconds)
}

# The hash of the switches the shader cache is keyed on, as the run will have them.
function Get-LauncherTranslationKey {
    param([Parameter(Mandatory)][System.Collections.IDictionary]$Environment)
    $text = New-Object System.Text.StringBuilder
    foreach ($name in Get-LauncherTranslationSwitches) {
        $value = ''
        if ($Environment.Contains($name)) { $value = [string]$Environment[$name] }
        [void]$text.Append("$name=$value`n")
    }
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        $bytes = $sha.ComputeHash([System.Text.Encoding]::UTF8.GetBytes($text.ToString()))
    } finally { $sha.Dispose() }
    return (($bytes | ForEach-Object { $_.ToString('x2') }) -join '')
}

function Format-LauncherArgument {
    param([AllowEmptyString()][string]$Value)
    if ($Value -eq '' -or $Value -match '[\s"]') { return '"' + $Value.Replace('"', '\"') + '"' }
    return $Value
}

# A PowerShell single-quoted string. PowerShell takes the typographic quotes U+2018 to U+201B
# for single quotes too (a folder named "Dad's games" with U+2019 would end the string), so
# every one of them is doubled.
$script:LauncherSingleQuotes = @([char]0x27, [char]0x2018, [char]0x2019, [char]0x201A, [char]0x201B)

function ConvertTo-LauncherQuoted {
    param([AllowEmptyString()][string]$Value)
    $text = New-Object System.Text.StringBuilder
    foreach ($c in $Value.ToCharArray()) {
        if ($c -in $script:LauncherSingleQuotes) { [void]$text.Append($c) }
        [void]$text.Append($c)
    }
    return "'" + $text.ToString() + "'"
}

# The PowerShell line that starts the game through scripts\play.ps1.
function Get-LauncherPlayCommand {
    param([Parameter(Mandatory)]$Run, [Parameter(Mandatory)][string]$GameExe)
    $parts = @('&', (ConvertTo-LauncherQuoted (Get-PlayScriptPath)),
        '-Exe', (ConvertTo-LauncherQuoted $GameExe),
        '-Build', (ConvertTo-LauncherQuoted $Run['Build']),
        '-Console', (ConvertTo-LauncherQuoted $Run['Console']))
    if ($Run['RayTracing']) { $parts += '-RayTracing' }
    $pairs = @()
    foreach ($k in $Run['More'].Keys) { $pairs += ('{0}={1}' -f (ConvertTo-LauncherQuoted $k), (ConvertTo-LauncherQuoted $Run['More'][$k])) }
    $parts += ('-More @{' + ($pairs -join '; ') + '}')
    return ($parts -join ' ')
}

# Windows PowerShell runs play.ps1: it is on every Windows, at a path that does not move (a
# PowerShell 7 from the Store runs from a folder that cannot always be started by its path).
function Get-LauncherPowerShellPath {
    return (Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe')
}

function New-LauncherStep {
    param([string]$Id, [bool]$Needed, [string]$Reason, [string]$File, [string[]]$Arguments, $Environment)
    $step = [ordered]@{}
    $step['id'] = $Id
    $step['needed'] = $Needed
    $step['reason'] = $Reason
    $step['file'] = $File
    $step['arguments'] = $Arguments
    $step['environment'] = $Environment
    $step['cwd'] = $script:LauncherRoot
    $step['log'] = $null
    $step['command'] = $null
    if ($File) {
        $step['log'] = Join-Path (Get-LauncherDataDir) "logs\$Id.log"
        $step['command'] = ((@($File) + @($Arguments)) | ForEach-Object { Format-LauncherArgument $_ }) -join ' '
    }
    return $step
}

# What Play would do with these settings, step by step, without starting anything: for each
# step whether it is needed and why, the program, its arguments, the switches it gets and its
# log. 'problem' says what stops Play (no victory.exe, no executable of the game).
function Get-LauncherPlayPlan {
    param($Settings, [string]$Language)
    if ($null -eq $Settings) { $Settings = Get-LauncherSettings }
    $run = Resolve-LauncherRun -Settings $Settings
    $build = $run['Build']
    $victory = Get-LauncherVictoryPath -Build $build
    $jobs = [string]$run['Jobs']
    $gameExe = $Settings['game_exe']
    $problem = $null
    if (-not (Test-Path -LiteralPath $victory)) {
        $problem = Get-LauncherText -Key 'error.no_victory' -Language $Language -Arguments @("target\$build\victory.exe", (Get-LauncherBuildCommand -Build $build))
    } elseif (-not $gameExe) {
        $problem = Get-LauncherText -Key 'error.no_game_exe' -Language $Language
    } elseif (-not (Test-Path -LiteralPath $gameExe)) {
        $problem = Get-LauncherText -Key 'error.game_exe_missing' -Language $Language -Arguments @($gameExe)
    }
    $shownExe = $gameExe
    if (-not $shownExe) { $shownExe = '<game executable>' }
    $gameDir = Split-Path -Parent $shownExe
    if (-not $gameDir) { $gameDir = '<game folder>' }

    $key = Get-LauncherTranslationKey -Environment $run['Environment']
    $entry = [ordered]@{}
    $stamp = Get-LauncherStamp
    if ($stamp.Contains($build) -and $stamp[$build] -is [System.Collections.IDictionary]) { $entry = $stamp[$build] }
    $hash = $null
    if (Test-Path -LiteralPath $victory) { $hash = (Get-FileHash -LiteralPath $victory -Algorithm SHA256).Hash.ToLowerInvariant() }

    $shadersReason = 'up to date'
    if (-not $entry.Contains('victory_hash')) { $shadersReason = 'never translated with this build' }
    elseif ($entry['victory_hash'] -ne $hash) { $shadersReason = 'victory.exe changed' }
    elseif ($entry['translation_key'] -ne $key) { $shadersReason = 'a translation switch changed' }
    $needShaders = ($shadersReason -ne 'up to date')

    $lastRun = $null
    if ($entry.Contains('last_run_at')) { $lastRun = [long]$entry['last_run_at'] }
    $warmAt = $null
    if ($entry.Contains('warm_at')) { $warmAt = [long]$entry['warm_at'] }
    $warmReason = 'up to date'
    if ($null -eq $lastRun) { $warmReason = 'no run has written its pipelines down yet' }
    elseif ($needShaders) { $warmReason = 'the shaders are translated again' }
    elseif ($null -eq $warmAt -or $lastRun -gt $warmAt) { $warmReason = 'a run drew with pipelines since the last warm-up' }
    $needWarm = ($null -ne $lastRun) -and ($warmReason -ne 'up to date')

    $environment = $run['Environment']
    $steps = @(
        (New-LauncherStep -Id 'build' -Needed $false -Reason (Get-LauncherBuildCommand -Build $build)),
        (New-LauncherStep -Id 'shaders' -Needed $needShaders -Reason $shadersReason -File $victory -Arguments @('shaders', $gameDir, '--jobs', $jobs) -Environment $environment),
        (New-LauncherStep -Id 'warm' -Needed $needWarm -Reason $warmReason -File $victory -Arguments @('warm', '--used', '--jobs', $jobs) -Environment $environment),
        (New-LauncherStep -Id 'open' -Needed $true -Reason 'scripts\play.ps1' -File (Get-LauncherPowerShellPath) -Arguments @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-Command', (Get-LauncherPlayCommand -Run $run -GameExe $shownExe)) -Environment ([ordered]@{}))
    )
    $plan = [ordered]@{}
    $plan['problem'] = $problem
    $plan['build'] = $build
    $plan['victory'] = $victory
    $plan['victory_hash'] = $hash
    $plan['translation_key'] = $key
    $plan['run'] = $run
    $plan['steps'] = $steps
    return $plan
}

# The environment of a step's process: this one's, without the VICTORY_ switches it may carry
# (as play.ps1 clears them), plus the step's own.
function New-LauncherProcessInfo {
    param([Parameter(Mandatory)]$Step)
    $cmd = $env:ComSpec
    if (-not $cmd) { $cmd = 'cmd.exe' }
    $logDir = Split-Path -Parent $Step['log']
    if (-not (Test-Path -LiteralPath $logDir)) { New-Item -ItemType Directory -Path $logDir -Force | Out-Null }
    $line = ((@($Step['file']) + @($Step['arguments'])) | ForEach-Object { '"' + $_ + '"' }) -join ' '
    $info = New-Object System.Diagnostics.ProcessStartInfo
    $info.FileName = $cmd
    # cmd /s keeps every quote between its first and last; the output goes to the step's log.
    $info.Arguments = '/d /s /c "' + $line + ' > "' + $Step['log'] + '" 2>&1"'
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
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

function Send-LauncherProgress {
    param($Progress, [string]$Step, [string]$State, [string]$Message)
    if ($null -eq $Progress) { return }
    $index = [Array]::IndexOf($script:LauncherSteps, $Step) + 1
    $count = $script:LauncherSteps.Count
    $done = $index - 1
    if ($State -in 'done', 'skipped') { $done = $index }
    $report = [ordered]@{}
    $report['step'] = $Step
    $report['index'] = $index
    $report['count'] = $count
    $report['state'] = $State
    $report['message'] = $Message
    $report['percent'] = [int][Math]::Round(100.0 * $done / $count)
    & $Progress $report
}

# Runs one step's process to its end; $State.Cancel = $true (from another thread) stops it.
function Invoke-LauncherStep {
    param([Parameter(Mandatory)]$Step, $Progress, $State)
    $process = [System.Diagnostics.Process]::Start((New-LauncherProcessInfo -Step $Step))
    try {
        $last = $null
        while (-not $process.WaitForExit(250)) {
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
        }
        $process.WaitForExit()
        return $process.ExitCode
    } finally { $process.Dispose() }
}

# Play: the steps of Get-LauncherPlayPlan, each reported to $Progress as it starts and ends
# (a dictionary: step, index, count, state = running | done | skipped | failed | cancelled,
# message, percent). -DryRun returns the command lines of the steps that would run and starts
# nothing. Returns 'done', 'cancelled', or throws with what went wrong.
function Invoke-LauncherPlay {
    param($Settings, [scriptblock]$Progress, $State, [string]$Language, [switch]$DryRun)
    if ($null -eq $Settings) { $Settings = Get-LauncherSettings }
    $plan = Get-LauncherPlayPlan -Settings $Settings -Language $Language
    if ($DryRun) {
        return @($plan['steps'] | Where-Object { $_['needed'] -and $_['command'] } | ForEach-Object { $_['command'] })
    }
    Send-LauncherProgress -Progress $Progress -Step 'build' -State 'running'
    if ($plan['problem']) {
        Send-LauncherProgress -Progress $Progress -Step 'build' -State 'failed' -Message $plan['problem']
        throw $plan['problem']
    }
    Send-LauncherProgress -Progress $Progress -Step 'build' -State 'done'
    $build = $plan['build']
    foreach ($step in $plan['steps'] | Select-Object -Skip 1) {
        $id = $step['id']
        if (-not $step['needed']) {
            Send-LauncherProgress -Progress $Progress -Step $id -State 'skipped' -Message $step['reason']
            continue
        }
        Send-LauncherProgress -Progress $Progress -Step $id -State 'running' -Message $step['reason']
        $code = Invoke-LauncherStep -Step $step -Progress $Progress -State $State
        if ($code -eq 'cancelled') {
            Send-LauncherProgress -Progress $Progress -Step $id -State 'cancelled'
            return 'cancelled'
        }
        $now = Get-LauncherNow
        switch ($id) {
            'shaders' { if ($code -eq 0) { Set-LauncherStampEntry -Build $build -Values @{ victory_hash = $plan['victory_hash']; translation_key = $plan['translation_key']; shaders_at = $now } } }
            'warm' { if ($code -eq 0) { Set-LauncherStampEntry -Build $build -Values @{ warm_at = $now } } }
            'open' { Set-LauncherStampEntry -Build $build -Values @{ last_run_at = $now } }
        }
        if ($code -ne 0 -and $id -ne 'open') {
            $name = Get-LauncherText -Key "prep.step.$id" -Language $Language
            $message = Get-LauncherText -Key 'prep.failed' -Language $Language -Arguments @($name, $code, $step['log'])
            Send-LauncherProgress -Progress $Progress -Step $id -State 'failed' -Message $message
            throw $message
        }
        Send-LauncherProgress -Progress $Progress -Step $id -State 'done'
    }
    return 'done'
}
