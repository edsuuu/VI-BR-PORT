# The options of options.json: loading and checking the file, a saved value made valid, one
# option changed (with the rules between options) and saved, and the settings resolved into
# what scripts/play.ps1 is started with.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$script:LauncherOptionsCache = $null

function Get-LauncherOptionsDocument {
    $path = Join-Path $script:LauncherDir 'options.json'
    $stamp = (Get-Item -LiteralPath $path).LastWriteTimeUtc
    if ($null -eq $script:LauncherOptionsCache -or $script:LauncherOptionsCache.Stamp -ne $stamp) {
        $doc = Read-LauncherJson -Path $path
        $problems = @(Test-LauncherOptionsDocument -Document $doc)
        if ($problems.Count -gt 0) { throw ("options.json: " + ($problems -join '; ')) }
        $script:LauncherOptionsCache = @{ Stamp = $stamp; Document = $doc }
    }
    return $script:LauncherOptionsCache.Document
}

function Get-LauncherOptions {
    return @((Get-LauncherOptionsDocument)['options'])
}

function Get-LauncherOption {
    param([Parameter(Mandatory)][string]$Id)
    foreach ($o in Get-LauncherOptions) { if ($o['id'] -eq $Id) { return $o } }
    $ids = (Get-LauncherOptions | ForEach-Object { $_['id'] }) -join ', '
    throw (Get-LauncherText -Key 'error.unknown_option' -Arguments @($Id, $ids))
}

# The switches the shader cache is keyed on, as options.json lists them.
function Get-LauncherTranslationSwitches {
    return @((Get-LauncherOptionsDocument)['translation_switches']['names'])
}

function Test-LauncherEffect {
    param($Effect, [string]$Where)
    if ($null -eq $Effect -or $Effect -isnot [System.Collections.IDictionary]) { return "$Where has no effect object" }
    foreach ($k in $Effect.Keys) {
        if ($k -notin 'play', 'env', 'jobs') { "$Where has an unknown effect '$k'" }
    }
    if ($Effect.Contains('play')) {
        foreach ($k in $Effect['play'].Keys) {
            if ($k -notin 'Console', 'Build', 'RayTracing') { "$Where sets an unknown play.ps1 parameter '$k'" }
        }
    }
    if ($Effect.Contains('env')) {
        foreach ($k in $Effect['env'].Keys) {
            if ($k -notlike 'VICTORY_*') { "$Where sets '$k', which is not a VICTORY_ switch" }
        }
    }
}

# Every problem of an options document (empty when it is valid).
function Test-LauncherOptionsDocument {
    param([Parameter(Mandatory)]$Document)
    if (-not $Document.Contains('options')) { return 'no options list' }
    $ids = @{}
    foreach ($o in $Document['options']) {
        $id = $o['id']
        if (-not $id) { 'an option has no id'; continue }
        if ($ids.Contains($id)) { "option '$id' is listed twice" }
        $ids[$id] = $o
        foreach ($k in 'kind', 'group', 'available', 'default') {
            if (-not $o.Contains($k)) { "option '$id' has no '$k'" }
        }
        if (-not $o.Contains('kind')) { continue }
        switch ($o['kind']) {
            'toggle' {
                if ($o['default'] -isnot [bool]) { "toggle '$id' has a default that is not true or false" }
                foreach ($side in 'on', 'off') { Test-LauncherEffect -Effect $o[$side] -Where "'$id' $side" }
            }
            'choice' {
                $values = @($o['values'] | ForEach-Object { [string]$_['value'] })
                if ($values.Count -lt 2) { "choice '$id' has fewer than two values" }
                if ([string]$o['default'] -notin $values) { "choice '$id' has a default that is not one of its values" }
                foreach ($v in $o['values']) { Test-LauncherEffect -Effect $v['effect'] -Where "'$id'='$($v['value'])'" }
            }
            default { "option '$id' has an unknown kind '$($o['kind'])'" }
        }
    }
    foreach ($o in $Document['options']) {
        if (-not $o.Contains('requires')) { continue }
        $id = $o['id']
        if ($o['kind'] -ne 'toggle') { "only a toggle can require another option ('$id')"; continue }
        $target = $o['requires']['option']
        if (-not $ids.Contains($target)) { "'$id' requires an unknown option '$target'"; continue }
        $values = @($ids[$target]['values'] | ForEach-Object { [string]$_['value'] })
        if ([string]$o['requires']['value'] -notin $values) { "'$id' requires a value '$target' does not have" }
    }
}

# The values an option takes, as the saved file and the MCP server write them.
function Get-LauncherOptionValues {
    param([Parameter(Mandatory)]$Option)
    if ($Option['kind'] -eq 'toggle') { return @($true, $false) }
    return @($Option['values'] | ForEach-Object { [string]$_['value'] })
}

# A value as typed (a string from the command line, a boolean or a number from JSON) made the
# option's own value: a boolean for a toggle, one of the listed strings for a choice.
function ConvertTo-LauncherOptionValue {
    param([Parameter(Mandatory)]$Option, [AllowNull()]$Value)
    $id = $Option['id']
    if ($Option['kind'] -eq 'toggle') {
        if ($Value -is [bool]) { return $Value }
        $text = ([string]$Value).Trim().ToLowerInvariant()
        if ($text -in 'on', 'true', '1', 'yes', 'enable', 'enabled') { return $true }
        if ($text -in 'off', 'false', '0', 'no', 'disable', 'disabled') { return $false }
        throw (Get-LauncherText -Key 'error.bad_value' -Arguments @($id, [string]$Value, 'on, off'))
    }
    $values = Get-LauncherOptionValues -Option $Option
    $text = ([string]$Value).Trim()
    foreach ($v in $values) { if ($v -eq $text) { return $v } }
    throw (Get-LauncherText -Key 'error.bad_value' -Arguments @($id, $text, ($values -join ', ')))
}

# Whether a value can be used on this branch: the option is available, and (for a choice) the
# value is not marked unavailable.
function Test-LauncherValueAvailable {
    param([Parameter(Mandatory)]$Option, [AllowNull()]$Value)
    if (-not $Option['available']) { return $false }
    if ($Option['kind'] -ne 'choice') { return $true }
    foreach ($v in $Option['values']) {
        if ([string]$v['value'] -eq [string]$Value) { return (-not ($v.Contains('available') -and -not $v['available'])) }
    }
    return $false
}

# The saved value of one option if it is valid and usable, else its default.
function Get-LauncherSavedValue {
    param([Parameter(Mandatory)]$Option, [Parameter(Mandatory)]$Config)
    $saved = $Config['options']
    if ($Option['available'] -and $saved.Contains($Option['id'])) {
        try {
            $value = ConvertTo-LauncherOptionValue -Option $Option -Value $saved[$Option['id']]
            if (Test-LauncherValueAvailable -Option $Option -Value $value) { return $value }
        } catch { }
    }
    return $Option['default']
}

# The settings in use: every option's value (saved and valid, else the default), the title's
# executable and the language.
function Get-LauncherSettings {
    param($Config)
    if ($null -eq $Config) { $Config = Get-LauncherConfig }
    $values = [ordered]@{}
    foreach ($o in Get-LauncherOptions) { $values[$o['id']] = Get-LauncherSavedValue -Option $o -Config $Config }
    $settings = [ordered]@{}
    $settings['options'] = $values
    $settings['game_exe'] = $Config['game_exe']
    $settings['language'] = Get-LauncherLanguage -Config $Config
    $settings['path'] = Get-LauncherConfigPath
    return $settings
}

# Sets one option, applies the rules between options, saves at once and returns the new
# settings, with 'changed' naming every option whose value moved.
function Set-LauncherOption {
    param([Parameter(Mandatory)][string]$Id, [Parameter(Mandatory)][AllowNull()][AllowEmptyString()]$Value)
    $option = Get-LauncherOption -Id $Id
    if (-not $option['available']) { throw (Get-LauncherText -Key 'error.unavailable' -Arguments @($Id)) }
    $value = ConvertTo-LauncherOptionValue -Option $option -Value $Value
    if (-not (Test-LauncherValueAvailable -Option $option -Value $value)) {
        throw (Get-LauncherText -Key 'error.value_unavailable' -Arguments @($Id, [string]$value))
    }
    $config = Get-LauncherConfig
    $before = (Get-LauncherSettings -Config $config)['options']
    $after = [ordered]@{}
    foreach ($k in $before.Keys) { $after[$k] = $before[$k] }
    $after[$Id] = $value
    # A toggle that needs another option's value brings it along when turned on...
    if ($option.Contains('requires') -and $value -eq $true) {
        $target = Get-LauncherOption -Id $option['requires']['option']
        $needed = [string]$option['requires']['value']
        if (-not (Test-LauncherValueAvailable -Option $target -Value $needed)) {
            throw (Get-LauncherText -Key 'error.value_unavailable' -Arguments @($target['id'], $needed))
        }
        $after[$target['id']] = $needed
    }
    # ...and goes off when that option takes another value.
    foreach ($o in Get-LauncherOptions) {
        if (-not $o.Contains('requires') -or $o['requires']['option'] -ne $Id) { continue }
        if ($after[$o['id']] -eq $true -and [string]$after[$Id] -ne [string]$o['requires']['value']) { $after[$o['id']] = $false }
    }
    $changed = @()
    foreach ($k in $after.Keys) {
        if ($after[$k] -ne $before[$k]) {
            $config['options'][$k] = $after[$k]
            $changed += $k
        }
    }
    $config['options'][$Id] = $after[$Id]
    Save-LauncherConfig -Config $config
    $settings = Get-LauncherSettings -Config $config
    $settings['changed'] = $changed
    return $settings
}

# Every option back to its default (saved as the defaults); the title's executable, the
# language and unknown keys stay.
function Reset-LauncherConfig {
    $config = Get-LauncherConfig
    $values = [ordered]@{}
    foreach ($o in Get-LauncherOptions) { $values[$o['id']] = $o['default'] }
    $config['options'] = $values
    Save-LauncherConfig -Config $config
    return Get-LauncherSettings -Config $config
}

# What one value of an option adds to a run.
function Get-LauncherEffect {
    param([Parameter(Mandatory)]$Option, [AllowNull()]$Value)
    if ($Option['kind'] -eq 'toggle') {
        if ($Value) { return $Option['on'] }
        return $Option['off']
    }
    foreach ($v in $Option['values']) { if ([string]$v['value'] -eq [string]$Value) { return $v['effect'] } }
    return [ordered]@{}
}

# The settings resolved into a run of scripts/play.ps1: its parameters (Build, Console,
# RayTracing), the switches handed to it through -More (an empty value removes one), the --jobs
# of the preparing steps, and Environment: every switch the run ends up with, play.ps1's own
# included. Options that are not available are never sent.
function Resolve-LauncherRun {
    param($Settings)
    if ($null -eq $Settings) { $Settings = Get-LauncherSettings }
    $play = [ordered]@{ Build = 'diag'; Console = 'series-s'; RayTracing = $false }
    $more = [ordered]@{}
    $jobs = $null
    $placeholders = @{ '{capture_dir}' = (Join-Path (Get-LauncherDataDir) 'captures') }
    foreach ($o in Get-LauncherOptions) {
        $value = $Settings['options'][$o['id']]
        if (-not (Test-LauncherValueAvailable -Option $o -Value $value)) { continue }
        $effect = Get-LauncherEffect -Option $o -Value $value
        if ($null -eq $effect) { continue }
        if ($effect.Contains('play')) { foreach ($k in $effect['play'].Keys) { $play[$k] = $effect['play'][$k] } }
        if ($effect.Contains('env')) {
            foreach ($k in $effect['env'].Keys) {
                $text = [string]$effect['env'][$k]
                foreach ($p in $placeholders.Keys) { $text = $text.Replace($p, $placeholders[$p]) }
                $more[$k] = $text
            }
        }
        if ($effect.Contains('jobs')) { $jobs = [int]$effect['jobs'] }
    }
    $environment = Get-PlayScriptSettings -RayTracing:([bool]$play['RayTracing'])
    foreach ($k in $more.Keys) {
        if ($more[$k] -eq '') { if ($environment.Contains($k)) { $environment.Remove($k) } }
        else { $environment[$k] = $more[$k] }
    }
    $run = [ordered]@{}
    $run['Build'] = [string]$play['Build']
    $run['Console'] = [string]$play['Console']
    $run['RayTracing'] = [bool]$play['RayTracing']
    $run['Jobs'] = $jobs
    $run['More'] = $more
    $run['Environment'] = $environment
    return $run
}
