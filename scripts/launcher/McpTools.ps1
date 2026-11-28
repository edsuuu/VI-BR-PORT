# The tools of the launcher's MCP server (scripts\launcher-mcp.ps1). They only read and change
# the saved settings, through the core's functions: no tool starts the game, builds, or runs a
# command, and none takes a path, a command or a free environment variable. An id that is not
# in options.json, a value the option does not take and an option that is not on this branch
# are refused. Dot-source after Launcher.ps1.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# The version is read once, when the server starts (Get-LauncherVersion asks git for the
# commit): the tools themselves never run a program.
$script:McpVersion = $null

function Get-McpVersion {
    if ($null -eq $script:McpVersion) { $script:McpVersion = Get-LauncherVersion }
    return $script:McpVersion
}

function Get-McpLanguage {
    param($Arguments)
    if ($null -ne $Arguments -and $Arguments.Contains('language') -and $null -ne $Arguments['language']) {
        $asked = [string]$Arguments['language']
        $languages = Get-LauncherLanguages
        if ($asked -notin $languages) {
            throw (Get-LauncherText -Key 'error.bad_value' -Arguments @('language', $asked, ($languages -join ', ')))
        }
        return $asked
    }
    return Get-LauncherLanguage -Config (Get-LauncherConfig)
}

# --- Input schemas -------------------------------------------------------------------------

function New-McpSchema {
    param([System.Collections.IDictionary]$Properties, [string[]]$Required)
    $schema = [ordered]@{}
    $schema['type'] = 'object'
    $props = [ordered]@{}
    if ($null -ne $Properties) { foreach ($k in $Properties.Keys) { $props[$k] = $Properties[$k] } }
    $schema['properties'] = $props
    if ($null -ne $Required -and $Required.Count -gt 0) { $schema['required'] = $Required }
    $schema['additionalProperties'] = $false
    return $schema
}

function New-McpProperty {
    param([Parameter(Mandatory)]$Type, [string]$Description, [object[]]$Enum)
    $p = [ordered]@{}
    $p['type'] = $Type
    if ($Description) { $p['description'] = $Description }
    if ($null -ne $Enum -and $Enum.Count -gt 0) { $p['enum'] = $Enum }
    return $p
}

function Get-McpOptionIds {
    param([switch]$TogglesOnly)
    $ids = @()
    foreach ($o in Get-LauncherOptions) {
        if ($TogglesOnly -and $o['kind'] -ne 'toggle') { continue }
        $ids += [string]$o['id']
    }
    return , $ids
}

# --- The tools -----------------------------------------------------------------------------

# Every tool: name, the language key of its description, its input schema and its handler.
function Get-McpTools {
    param([string]$Language)
    $idText = Get-LauncherText -Key 'mcp.param.id' -Language $Language
    $valueText = Get-LauncherText -Key 'mcp.param.value' -Language $Language
    $languageText = Get-LauncherText -Key 'mcp.param.language' -Language $Language
    $resetText = Get-LauncherText -Key 'mcp.param.reset_id' -Language $Language
    $allIds = Get-McpOptionIds
    $toggleIds = Get-McpOptionIds -TogglesOnly
    $languages = @(Get-LauncherLanguages)

    $tools = @()
    $tools += , @{ Name = 'launcher_list_options'; Text = 'mcp.tool.list_options'; Handler = { param($a) Invoke-McpListOptions -Arguments $a }
        Schema = (New-McpSchema -Properties ([ordered]@{ language = (New-McpProperty -Type 'string' -Description $languageText -Enum $languages) })) }
    $tools += , @{ Name = 'launcher_get_config'; Text = 'mcp.tool.get_settings'; Handler = { param($a) Invoke-McpGetConfig }
        Schema = (New-McpSchema) }
    $tools += , @{ Name = 'launcher_set_option'; Text = 'mcp.tool.set_option'; Handler = { param($a) Invoke-McpSetOption -Id $a['id'] -Value $a['value'] }
        Schema = (New-McpSchema -Required @('id', 'value') -Properties ([ordered]@{
                    id = (New-McpProperty -Type 'string' -Description $idText -Enum $allIds)
                    value = (New-McpProperty -Type @('string', 'boolean', 'number') -Description $valueText)
                })) }
    $tools += , @{ Name = 'launcher_enable'; Text = 'mcp.tool.enable_option'; Handler = { param($a) Invoke-McpToggle -Id $a['id'] -On $true }
        Schema = (New-McpSchema -Required @('id') -Properties ([ordered]@{ id = (New-McpProperty -Type 'string' -Description $idText -Enum $toggleIds) })) }
    $tools += , @{ Name = 'launcher_disable'; Text = 'mcp.tool.disable_option'; Handler = { param($a) Invoke-McpToggle -Id $a['id'] -On $false }
        Schema = (New-McpSchema -Required @('id') -Properties ([ordered]@{ id = (New-McpProperty -Type 'string' -Description $idText -Enum $toggleIds) })) }
    $tools += , @{ Name = 'launcher_reset'; Text = 'mcp.tool.reset'; Handler = { param($a) Invoke-McpReset -Id $a['id'] }
        Schema = (New-McpSchema -Properties ([ordered]@{ id = (New-McpProperty -Type 'string' -Description $resetText -Enum $allIds) })) }
    $tools += , @{ Name = 'launcher_env'; Text = 'mcp.tool.env'; Handler = { param($a) Invoke-McpEnv }
        Schema = (New-McpSchema) }
    $tools += , @{ Name = 'launcher_version'; Text = 'mcp.tool.version'; Handler = { param($a) Get-McpVersion }
        Schema = (New-McpSchema) }
    return $tools
}

# The tools as tools/list returns them.
function Get-McpToolList {
    param([string]$Language)
    $list = @()
    foreach ($t in @(Get-McpTools -Language $Language)) {
        $entry = [ordered]@{}
        $entry['name'] = $t.Name
        $entry['description'] = Get-LauncherText -Key $t.Text -Language $Language
        $entry['inputSchema'] = $t.Schema
        $list += , $entry
    }
    return , $list
}

# --- Arguments -----------------------------------------------------------------------------

function Test-McpType {
    param($Value, [string]$Type)
    switch ($Type) {
        'string' { return ($Value -is [string]) }
        'boolean' { return ($Value -is [bool]) }
        'number' { return (($Value -is [int]) -or ($Value -is [long]) -or ($Value -is [double]) -or ($Value -is [decimal]) -or ($Value -is [single])) }
    }
    return $false
}

# Checks the arguments against the tool's schema (names, required ones, types); a wrong one
# throws an ArgumentException, which the server answers with -32602. Values are checked by the
# tool itself, whose refusal is a result with isError.
function Test-McpArguments {
    param([Parameter(Mandatory)]$Tool, $Arguments)
    $schema = $Tool.Schema
    $properties = $schema['properties']
    foreach ($k in $Arguments.Keys) {
        if (-not $properties.Contains($k)) {
            throw (New-Object System.ArgumentException (Get-LauncherText -Key 'error.unknown_argument' -Arguments @($Tool.Name, $k)))
        }
        $types = @($properties[$k]['type'])
        $fits = $false
        foreach ($type in $types) { if (Test-McpType -Value $Arguments[$k] -Type $type) { $fits = $true } }
        if (-not $fits) {
            throw (New-Object System.ArgumentException (Get-LauncherText -Key 'error.bad_argument' -Arguments @($Tool.Name, $k, ($types -join ' | '))))
        }
    }
    if ($schema.Contains('required')) {
        foreach ($k in $schema['required']) {
            if (-not $Arguments.Contains($k)) {
                throw (New-Object System.ArgumentException (Get-LauncherText -Key 'error.missing_argument' -Arguments @($Tool.Name, $k)))
            }
        }
    }
}

# --- Handlers ------------------------------------------------------------------------------

function Get-McpOptionNote {
    param([Parameter(Mandatory)]$Option, $Value)
    if (-not $Option['available']) {
        if ($Option.Contains('note')) { return [string]$Option['note'] }
        return $null
    }
    if ($Option['kind'] -eq 'choice') {
        foreach ($v in $Option['values']) {
            if ([string]$v['value'] -eq [string]$Value -and $v.Contains('note')) { return [string]$v['note'] }
        }
    }
    return $null
}

# The option with this id; an empty or unknown id is refused with the list of ids.
function Get-McpOption {
    param([AllowNull()][AllowEmptyString()][string]$Id)
    if ([string]::IsNullOrWhiteSpace($Id)) {
        $ids = (Get-LauncherOptions | ForEach-Object { $_["id"] }) -join ", "
        throw (Get-LauncherText -Key "error.unknown_option" -Arguments @([string]$Id, $ids))
    }
    return Get-LauncherOption -Id $Id
}

# The option with this id if it can be changed on this branch; else a refusal that says why.
function Get-McpAvailableOption {
    param([AllowNull()][AllowEmptyString()][string]$Id)
    $option = Get-McpOption -Id $Id
    if (-not $option["available"]) {
        $note = Get-McpOptionNote -Option $option
        if ($note) { throw (Get-LauncherText -Key 'error.unavailable_because' -Arguments @($Id, $note)) }
        throw (Get-LauncherText -Key 'error.unavailable' -Arguments @($Id))
    }
    return $option
}

function Invoke-McpListOptions {
    param($Arguments)
    $language = Get-McpLanguage -Arguments $Arguments
    $settings = Get-LauncherSettings
    $list = @()
    foreach ($o in Get-LauncherOptions) {
        $id = [string]$o['id']
        $entry = [ordered]@{}
        $entry['id'] = $id
        $entry['label'] = Get-LauncherText -Key "option.$id.label" -Language $language
        $entry['hint'] = Get-LauncherText -Key "option.$id.hint" -Language $language
        $entry['group'] = [string]$o['group']
        $entry['group_label'] = Get-LauncherText -Key "group.$($o['group'])" -Language $language
        $entry['kind'] = [string]$o['kind']
        $entry['available'] = [bool]$o['available']
        if (-not $o['available']) {
            $entry['unavailable'] = Get-LauncherText -Key 'common.unavailable' -Language $language
            $entry['note'] = Get-McpOptionNote -Option $o
        }
        $entry['default'] = $o['default']
        $entry['value'] = $settings['options'][$id]
        $values = @()
        foreach ($v in Get-LauncherOptionValues -Option $o) {
            $item = [ordered]@{}
            $item['value'] = $v
            if ($o['kind'] -eq 'toggle') {
                if ($v) { $item['label'] = Get-LauncherText -Key 'common.on' -Language $language }
                else { $item['label'] = Get-LauncherText -Key 'common.off' -Language $language }
            } else {
                $item['label'] = Get-LauncherText -Key "option.$id.value.$v" -Language $language
            }
            $item['available'] = [bool](Test-LauncherValueAvailable -Option $o -Value $v)
            if ($o['available'] -and -not $item['available']) { $item['note'] = Get-McpOptionNote -Option $o -Value $v }
            $values += , $item
        }
        $entry['values'] = $values
        if ($o.Contains('requires')) {
            $requires = [ordered]@{}
            $requires['option'] = [string]$o['requires']['option']
            $requires['value'] = [string]$o['requires']['value']
            $entry['requires'] = $requires
        }
        $list += , $entry
    }
    $out = [ordered]@{}
    $out['language'] = $language
    $out['options'] = $list
    return $out
}

function Invoke-McpGetConfig {
    $settings = Get-LauncherSettings
    $out = [ordered]@{}
    $out['options'] = $settings['options']
    $out['game_exe'] = $settings['game_exe']
    $out['language'] = $settings['language']
    $out['path'] = $settings['path']
    if ($script:LauncherConfigSetAside) { $out['set_aside'] = $script:LauncherConfigSetAside }
    return $out
}

# What a change did: the option and value asked for, every option that moved (the ones a rule
# between options moved too) and the saved settings after it.
function New-McpChange {
    param([string]$Id, $Value, [string[]]$Changed, $Settings)
    $moved = [ordered]@{}
    foreach ($k in @($Changed)) { if ($k) { $moved[$k] = $Settings['options'][$k] } }
    $out = [ordered]@{}
    if ($Id) {
        $out['id'] = $Id
        $out['value'] = $Value
    }
    $out['changed'] = $moved
    $out['saved'] = $Settings['path']
    $out['options'] = $Settings['options']
    return $out
}

function Invoke-McpSetOption {
    param([AllowNull()][AllowEmptyString()][string]$Id, [AllowNull()]$Value)
    $option = Get-McpAvailableOption -Id $Id
    $value = ConvertTo-LauncherOptionValue -Option $option -Value $Value
    if (-not (Test-LauncherValueAvailable -Option $option -Value $value)) {
        $note = Get-McpOptionNote -Option $option -Value $value
        $what = "$Id=$value"
        if ($note) { throw (Get-LauncherText -Key 'error.unavailable_because' -Arguments @($what, $note)) }
        throw (Get-LauncherText -Key 'error.value_unavailable' -Arguments @($Id, [string]$value))
    }
    $settings = Set-LauncherOption -Id $Id -Value $value
    return New-McpChange -Id $Id -Value $settings['options'][$Id] -Changed $settings['changed'] -Settings $settings
}

function Invoke-McpToggle {
    param([AllowNull()][AllowEmptyString()][string]$Id, [bool]$On)
    $option = Get-McpOption -Id $Id
    if ($option['kind'] -ne 'toggle') {
        throw (Get-LauncherText -Key 'error.not_toggle' -Arguments @($Id, ((Get-LauncherOptionValues -Option $option) -join ', ')))
    }
    return Invoke-McpSetOption -Id $Id -Value $On
}

function Invoke-McpReset {
    param([AllowNull()][AllowEmptyString()][string]$Id)
    if ($Id) {
        $option = Get-McpAvailableOption -Id $Id
        return Invoke-McpSetOption -Id $Id -Value $option['default']
    }
    $before = (Get-LauncherSettings)['options']
    $settings = Reset-LauncherConfig
    $changed = @()
    foreach ($k in $settings['options'].Keys) {
        if (-not $before.Contains($k) -or $before[$k] -ne $settings['options'][$k]) { $changed += $k }
    }
    return New-McpChange -Changed $changed -Settings $settings
}

# What Play would run with the saved settings: a dry run, nothing is started.
function Invoke-McpEnv {
    $settings = Get-LauncherSettings
    $plan = Get-LauncherPlayPlan -Settings $settings -Language $settings['language']
    $run = $plan['run']
    $play = [ordered]@{}
    $play['Build'] = $run['Build']
    $play['Console'] = $run['Console']
    $play['RayTracing'] = $run['RayTracing']
    $environment = [ordered]@{}
    foreach ($k in ($run['Environment'].Keys | Sort-Object)) { $environment[$k] = $run['Environment'][$k] }
    $steps = @()
    foreach ($s in $plan['steps']) {
        $step = [ordered]@{}
        $step['id'] = $s['id']
        $step['needed'] = $s['needed']
        $step['reason'] = $s['reason']
        $step['command'] = $s['command']
        $steps += , $step
    }
    $out = [ordered]@{}
    $out['dry_run'] = $true
    $out['problem'] = $plan['problem']
    $out['play'] = $play
    $out['more'] = $run['More']
    $out['jobs'] = $run['Jobs']
    $out['environment'] = $environment
    $out['steps'] = $steps
    return $out
}

# --- tools/call ----------------------------------------------------------------------------

function New-McpTextResult {
    param([string]$Text, [bool]$IsError)
    $content = [ordered]@{}
    $content['type'] = 'text'
    $content['text'] = $Text
    $result = [ordered]@{}
    $result['content'] = @(, $content)
    $result['isError'] = $IsError
    return $result
}

# Runs one tool. A tool that is not there or arguments that do not fit its schema throw an
# ArgumentException (the server answers -32602); a tool's own refusal or failure is a result
# with isError and the reason as plain text.
function Invoke-McpToolCall {
    param([AllowNull()][AllowEmptyString()][string]$Name, $Arguments)
    if ($null -eq $Arguments) { $Arguments = [ordered]@{} }
    if ($Arguments -isnot [System.Collections.IDictionary]) {
        throw (New-Object System.ArgumentException (Get-LauncherText -Key 'error.bad_argument' -Arguments @($Name, 'arguments', 'object')))
    }
    $tool = $null
    foreach ($t in @(Get-McpTools)) { if ($t.Name -eq $Name) { $tool = $t } }
    if ($null -eq $tool) { throw (New-Object System.ArgumentException (Get-LauncherText -Key 'error.unknown_tool' -Arguments @($Name))) }
    Test-McpArguments -Tool $tool -Arguments $Arguments
    try {
        $data = & $tool.Handler $Arguments
        $text = (ConvertTo-Json -InputObject $data -Depth 32).Replace("`r`n", "`n")
        return New-McpTextResult -Text $text -IsError $false
    } catch {
        return New-McpTextResult -Text $_.Exception.Message -IsError $true
    }
}
