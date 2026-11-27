# The launcher: with no mode it opens the window (scripts\launcher\Window.ps1); the modes below
# need no window and print to the console.
#   -Check              the self-check (scripts\launcher\Test-Launcher.ps1)
#   -Get                the saved settings, as JSON
#   -Set id=value ...   changes options (or game_exe=<path>, language=<en|pt-BR>) and saves them
#   -Reset              every option back to its default
#   -PrintEnv           what Play would run: play.ps1's parameters, the switches, the steps
#   -Version            v<version> (<commit>)
#   -Snapshot <png>     draws the window off screen into a picture, never showing it
#                       (-View play|preparing|more, -Language en|pt-BR: drawn, not saved)
# The settings live in %APPDATA%\VI-BR-PORT\launcher.json (VICTORY_LAUNCHER_CONFIG=<file> overrides).
param(
    [switch]$Check,
    [switch]$Get,
    [string[]]$Set,
    [switch]$Reset,
    [switch]$PrintEnv,
    [switch]$Version,
    [string]$Snapshot,
    [ValidateSet('play', 'preparing', 'more', 'options')][string]$View = 'play',
    [string]$Language
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. (Join-Path $PSScriptRoot 'launcher\Launcher.ps1')

function Write-LauncherSetAside {
    if ($script:LauncherConfigSetAside) { Write-Warning (Get-LauncherText -Key 'cli.broken_file' -Arguments @($script:LauncherConfigSetAside)) }
}

try {
    if ($Check) {
        & (Join-Path $PSScriptRoot 'launcher\Test-Launcher.ps1')
        exit $LASTEXITCODE
    }

    if ($Version) {
        (Get-LauncherVersion).text
        exit 0
    }

    if ($Reset) {
        $null = Reset-LauncherConfig
        Write-LauncherSetAside
        Get-LauncherText -Key 'cli.saved' -Arguments @(Get-LauncherConfigPath)
        exit 0
    }

    if ($Set) {
        foreach ($assignment in $Set) {
            $at = $assignment.IndexOf('=')
            if ($at -lt 1) { throw (Get-LauncherText -Key 'error.bad_assignment') }
            $id = $assignment.Substring(0, $at).Trim()
            $value = $assignment.Substring($at + 1).Trim()
            if ($id -in 'game_exe', 'language') { $settings = Set-LauncherSetting -Name $id -Value $value }
            else { $settings = Set-LauncherOption -Id $id -Value $value }
            Write-LauncherSetAside
            foreach ($k in @($id) + @($settings['changed'] | Where-Object { $_ -and $_ -ne $id })) {
                if ($settings['options'].Contains($k)) { '{0} = {1}' -f $k, $settings['options'][$k] }
                else { '{0} = {1}' -f $k, $settings[$k] }
            }
        }
        Get-LauncherText -Key 'cli.saved' -Arguments @(Get-LauncherConfigPath)
        exit 0
    }

    if ($Get) {
        $settings = Get-LauncherSettings
        Write-LauncherSetAside
        ConvertTo-LauncherJsonText -Data $settings
        exit 0
    }

    if ($PrintEnv) {
        $plan = Get-LauncherPlayPlan
        Write-LauncherSetAside
        $run = $plan['run']
        $line = 'scripts\play.ps1 -Build {0} -Console {1}' -f $run['Build'], $run['Console']
        if ($run['RayTracing']) { $line += ' -RayTracing' }
        "# play.ps1"
        $line
        "# -More"
        foreach ($k in $run['More'].Keys) { '{0}={1}' -f $k, $run['More'][$k] }
        "# every switch of the run"
        foreach ($k in ($run['Environment'].Keys | Sort-Object)) { '{0}={1}' -f $k, $run['Environment'][$k] }
        "# steps"
        foreach ($s in $plan['steps']) {
            $needed = 'skip'
            if ($s['needed']) { $needed = 'run' }
            '{0,-8} {1,-4} {2}' -f $s['id'], $needed, $s['reason']
            if ($s['command']) { '         ' + $s['command'] }
        }
        if ($plan['problem']) { '# ' + $plan['problem'] }
        exit 0
    }

    $window = Join-Path $PSScriptRoot 'launcher\Window.ps1'
    if (-not (Test-Path -LiteralPath $window)) {
        Get-LauncherText -Key 'cli.window_missing' -Arguments @($window, (Get-LauncherText -Key 'cli.usage'))
        exit 1
    }
    . $window
    if ($Snapshot) {
        Save-LauncherWindowSnapshot -Path $Snapshot -View $View -Language $Language
        exit 0
    }
    Show-LauncherWindow
} catch {
    # A wrong id or value is said in one line, not as a script error.
    [Console]::Error.WriteLine($_.Exception.Message)
    exit 1
}
