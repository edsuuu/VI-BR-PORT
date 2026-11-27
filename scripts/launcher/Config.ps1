# The saved settings: %APPDATA%\VI-BR-PORT\launcher.json (VICTORY_LAUNCHER_CONFIG=<file> puts
# them elsewhere, for tests). The window and the MCP server read and write the same file. Keys
# this version does not know are kept; a file that does not read is set aside as
# launcher.json.bad and the defaults are used.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# The path of the file set aside the last time one did not read, or $null.
$script:LauncherConfigSetAside = $null

function Get-LauncherConfigPath {
    $override = [Environment]::GetEnvironmentVariable('VICTORY_LAUNCHER_CONFIG')
    if ($override) { return [System.IO.Path]::GetFullPath($override) }
    return (Join-Path ([Environment]::GetFolderPath('ApplicationData')) 'VI-BR-PORT\launcher.json')
}

# Where the launcher keeps its other files (the preparing stamp, logs, captured frames).
function Get-LauncherDataDir {
    return (Split-Path -Parent (Get-LauncherConfigPath))
}

function New-LauncherConfig {
    $config = [ordered]@{}
    $config['format'] = 1
    $config['options'] = [ordered]@{}
    $config['game_exe'] = $null
    $config['language'] = $null
    return $config
}

# The saved file as it is, with 'options' always a dictionary. Values are not checked here
# (Get-LauncherSettings does): a value this version does not know stays in the file.
function Get-LauncherConfig {
    $path = Get-LauncherConfigPath
    if (-not (Test-Path -LiteralPath $path)) { return New-LauncherConfig }
    try {
        $config = Read-LauncherJson -Path $path
        if ($config -isnot [System.Collections.IDictionary]) { throw 'the settings file is not a JSON object' }
    } catch {
        $bad = "$path.bad"
        Move-Item -LiteralPath $path -Destination $bad -Force
        $script:LauncherConfigSetAside = $bad
        return New-LauncherConfig
    }
    if (-not $config.Contains('options') -or ($config['options'] -isnot [System.Collections.IDictionary])) {
        $config['options'] = [ordered]@{}
    }
    foreach ($key in 'game_exe', 'language') { if (-not $config.Contains($key)) { $config[$key] = $null } }
    return $config
}

function Save-LauncherConfig {
    param([Parameter(Mandatory)][System.Collections.IDictionary]$Config)
    Write-LauncherJson -Path (Get-LauncherConfigPath) -Data $Config
}

# A saved setting that is not an option: game_exe (the title's executable the user dumped) or
# language (a file name of lang\ without .json).
function Set-LauncherSetting {
    param([Parameter(Mandatory)][string]$Name, [AllowNull()][AllowEmptyString()][string]$Value)
    if ($Name -notin 'game_exe', 'language') {
        throw (Get-LauncherText -Key 'error.unknown_setting' -Arguments @($Name))
    }
    # (Not $Value itself: a [string] parameter turns $null back into ''.)
    $saved = $null
    if (-not [string]::IsNullOrWhiteSpace($Value)) { $saved = $Value.Trim() }
    if ($Name -eq 'language' -and $null -ne $saved -and $saved -notin (Get-LauncherLanguages)) {
        throw (Get-LauncherText -Key 'error.bad_value' -Arguments @($Name, $saved, ((Get-LauncherLanguages) -join ', ')))
    }
    if ($Name -eq 'game_exe' -and $null -ne $saved) { $saved = [System.IO.Path]::GetFullPath($saved) }
    $config = Get-LauncherConfig
    $config[$Name] = $saved
    Save-LauncherConfig -Config $config
    return Get-LauncherSettings -Config $config
}
