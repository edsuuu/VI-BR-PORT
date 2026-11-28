# The MCP server's self-check: starts scripts\launcher-mcp.ps1 as a child process (with no
# window) on a temporary settings file (VICTORY_LAUNCHER_CONFIG), talks to it over stdin and
# stdout as a client does, and checks every answer and the saved file. Nothing is started but
# the server; the user's settings are never touched.
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\launcher\Test-Mcp.ps1
#   pwsh -NoProfile -File scripts\launcher\Test-Mcp.ps1
# -Shell names the PowerShell that runs the server (by default the one running this script).
param([string]$Shell)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. (Join-Path $PSScriptRoot 'Launcher.ps1')

if (-not $Shell) { $Shell = (Get-Process -Id $PID).Path }
$server = Join-Path $script:LauncherRoot 'scripts\launcher-mcp.ps1'
$tempDir = Join-Path ([System.IO.Path]::GetTempPath()) ('launcher-mcp-test-' + [guid]::NewGuid().ToString('n'))
New-Item -ItemType Directory -Path $tempDir -Force | Out-Null
$configPath = Join-Path $tempDir 'launcher.json'
# This process reads the same file (Resolve-LauncherRun, the texts), never the user's.
$savedOverride = $env:VICTORY_LAUNCHER_CONFIG
$env:VICTORY_LAUNCHER_CONFIG = $configPath

$script:Failures = 0
$script:Passes = 0
$script:NextId = 0
$script:Buffer = New-Object System.Collections.Generic.List[byte]
$script:AllBytes = New-Object System.Collections.Generic.List[byte]
$script:Strict = New-Object System.Text.UTF8Encoding $false, $true

function Test-Case {
    param([Parameter(Mandatory)][string]$Name, [Parameter(Mandatory)][scriptblock]$Body)
    try {
        $problems = @(& $Body | Where-Object { $_ })
    } catch {
        $problems = @("threw: $($_.Exception.Message)")
    }
    if ($problems.Count -eq 0) {
        $script:Passes++
        Write-Output "ok    $Name"
    } else {
        $script:Failures++
        Write-Output "FAIL  $Name"
        foreach ($p in $problems) { Write-Output "        $p" }
    }
}

# --- The child process ---------------------------------------------------------------------

function Start-McpChild {
    $info = New-Object System.Diagnostics.ProcessStartInfo
    $info.FileName = $Shell
    $info.Arguments = '-NoProfile -NonInteractive -ExecutionPolicy Bypass -File "{0}"' -f $server
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardInput = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $info.EnvironmentVariables['VICTORY_LAUNCHER_CONFIG'] = $configPath
    return [System.Diagnostics.Process]::Start($info)
}

# Writes one line to the server, as UTF-8 without BOM.
function Send-McpRaw {
    param([Parameter(Mandatory)][string]$Line)
    $bytes = (New-Object System.Text.UTF8Encoding $false).GetBytes($Line + "`n")
    $stream = $script:Child.StandardInput.BaseStream
    $stream.Write($bytes, 0, $bytes.Length)
    $stream.Flush()
}

# The next line the server wrote, decoded as strict UTF-8; $null when none came in time.
function Receive-McpLine {
    param([int]$TimeoutMs = 20000)
    $stream = $script:Child.StandardOutput.BaseStream
    $chunk = New-Object byte[] 65536
    $deadline = [DateTime]::UtcNow.AddMilliseconds($TimeoutMs)
    while ($true) {
        $at = $script:Buffer.IndexOf([byte]10)
        if ($at -ge 0) {
            $bytes = $script:Buffer.GetRange(0, $at).ToArray()
            $script:Buffer.RemoveRange(0, $at + 1)
            return $script:Strict.GetString($bytes)
        }
        $left = [int]($deadline - [DateTime]::UtcNow).TotalMilliseconds
        if ($left -le 0) { return $null }
        if ($null -eq $script:Pending) { $script:Pending = $stream.ReadAsync($chunk, 0, $chunk.Length) ; $script:PendingChunk = $chunk }
        if (-not $script:Pending.Wait($left)) { return $null }
        $count = $script:Pending.Result
        $got = $script:PendingChunk
        $script:Pending = $null
        if ($count -eq 0) { return $null }
        for ($i = 0; $i -lt $count; $i++) { $script:Buffer.Add($got[$i]); $script:AllBytes.Add($got[$i]) }
    }
}
$script:Pending = $null
$script:PendingChunk = $null

function Receive-McpMessage {
    $line = Receive-McpLine
    if ($null -eq $line) { throw 'no answer from the server' }
    return ConvertFrom-LauncherJsonText -Text $line
}

# Sends a request and returns its answer (which must carry the same id).
function Send-McpRequest {
    param([Parameter(Mandatory)][string]$Method, $Params)
    $script:NextId++
    $message = [ordered]@{ jsonrpc = '2.0'; id = $script:NextId; method = $Method }
    if ($null -ne $Params) { $message['params'] = $Params }
    Send-McpRaw -Line (ConvertTo-Json -InputObject $message -Depth 16 -Compress)
    $answer = Receive-McpMessage
    if ($answer['id'] -ne $script:NextId) { throw "answer to id $($script:NextId) came with id $($answer['id'])" }
    return $answer
}

function Invoke-McpTestTool {
    param([Parameter(Mandatory)][string]$Name, $Arguments)
    if ($null -eq $Arguments) { $Arguments = [ordered]@{} }
    return Send-McpRequest -Method 'tools/call' -Params ([ordered]@{ name = $Name; arguments = $Arguments })
}

# The result of a tool that worked, as data; a problem line when it did not.
function Get-McpToolData {
    param($Answer)
    if (-not $Answer.Contains('result')) { return "no result: $(ConvertTo-Json -InputObject $Answer -Depth 8 -Compress)" }
    if ($Answer['result']['isError']) { return "isError: $($Answer['result']['content'][0]['text'])" }
    return ConvertFrom-LauncherJsonText -Text $Answer['result']['content'][0]['text']
}

function Get-SavedOptions {
    return (Read-LauncherJson -Path $configPath)['options']
}

function Test-Saved {
    param([string]$Id, $Expected)
    $saved = Get-SavedOptions
    if (-not $saved.Contains($Id)) { return "the saved file has no $Id" }
    if ($saved[$Id] -ne $Expected -or $saved[$Id].GetType() -ne $Expected.GetType()) { return "the saved file has $Id=$($saved[$Id]), expected $Expected" }
}

function Test-Refused {
    param($Answer, [string]$Contains)
    if (-not $Answer.Contains('result')) { return "a protocol error instead of a refusal: $($Answer['error']['message'])" }
    if (-not $Answer['result']['isError']) { return 'not refused' }
    $text = [string]$Answer['result']['content'][0]['text']
    if (-not $text) { return 'a refusal with no text' }
    if ($Contains -and $text -notlike "*$Contains*") { return "the refusal '$text' does not name '$Contains'" }
}

function Test-ProtocolError {
    param($Answer, [int]$Code)
    if (-not $Answer.Contains('error')) { return "no error (expected $Code)" }
    if ($Answer['error']['code'] -ne $Code) { return "error $($Answer['error']['code']) '$($Answer['error']['message'])', expected $Code" }
}

# --- The checks ----------------------------------------------------------------------------

Write-Output "server: $Shell -File $server"
Write-Output "settings: $configPath"
$script:Child = Start-McpChild
try {
    Test-Case 'initialize answers with the client''s protocol version, the tools capability and the server''s name' {
        $a = Send-McpRequest -Method 'initialize' -Params ([ordered]@{ protocolVersion = '2025-06-18'; capabilities = [ordered]@{}; clientInfo = [ordered]@{ name = 'test'; version = '1' } })
        $r = $a['result']
        if ($r['protocolVersion'] -ne '2025-06-18') { "protocolVersion $($r['protocolVersion'])" }
        if ($r['serverInfo']['name'] -ne 'vi-br-port-launcher') { "serverInfo.name $($r['serverInfo']['name'])" }
        if ($r['serverInfo']['version'] -ne (Get-LauncherWorkspaceVersion)) { "serverInfo.version $($r['serverInfo']['version'])" }
        if ($r['capabilities']['tools'] -isnot [System.Collections.IDictionary]) { 'capabilities.tools is not an object' }
    }

    Test-Case 'initialize with a version it does not know answers with the newest it supports' {
        $a = Send-McpRequest -Method 'initialize' -Params ([ordered]@{ protocolVersion = '1999-01-01'; capabilities = [ordered]@{} })
        if ($a['result']['protocolVersion'] -ne '2025-11-25') { "protocolVersion $($a['result']['protocolVersion'])" }
    }

    Test-Case 'notifications/initialized gets no answer, ping gets an empty result' {
        Send-McpRaw -Line '{"jsonrpc":"2.0","method":"notifications/initialized"}'
        $a = Send-McpRequest -Method 'ping'
        if (-not $a.Contains('result') -or $a['result'].Count -ne 0) { 'ping did not answer {}' }
    }

    Test-Case 'tools/list lists the eight tools, each with a description and an object schema' {
        $a = Send-McpRequest -Method 'tools/list'
        $tools = @($a['result']['tools'])
        $names = @($tools | ForEach-Object { $_['name'] })
        $expected = @('launcher_list_options', 'launcher_get_config', 'launcher_set_option', 'launcher_enable', 'launcher_disable', 'launcher_reset', 'launcher_env', 'launcher_version')
        if (($names -join ',') -ne ($expected -join ',')) { "tools: $($names -join ', ')" }
        foreach ($t in $tools) {
            if (-not $t['description']) { "$($t['name']) has no description" }
            if ($t['inputSchema']['type'] -ne 'object') { "$($t['name']) has no object schema" }
            if ($t['inputSchema']['additionalProperties'] -ne $false) { "$($t['name']) takes arguments it does not list" }
        }
        $set = @($tools | Where-Object { $_['name'] -eq 'launcher_set_option' })[0]
        if ((@($set['inputSchema']['required']) -join ',') -ne 'id,value') { 'launcher_set_option does not require id and value' }
        $enable = @($tools | Where-Object { $_['name'] -eq 'launcher_enable' })[0]
        $toggles = @(Get-LauncherOptions | Where-Object { $_['kind'] -eq 'toggle' } | ForEach-Object { $_['id'] })
        if ((@($enable['inputSchema']['properties']['id']['enum']) -join ',') -ne ($toggles -join ',')) { 'launcher_enable does not list exactly the on/off options' }
    }

    Test-Case 'launcher_list_options gives every option with its value, default, availability and texts' {
        $data = Get-McpToolData (Invoke-McpTestTool -Name 'launcher_list_options' -Arguments ([ordered]@{ language = 'pt-BR' }))
        if ($data -is [string]) { return $data }
        $listed = @($data['options'])
        $all = @(Get-LauncherOptions)
        if ($listed.Count -ne $all.Count) { "$($listed.Count) options listed, options.json has $($all.Count)" }
        foreach ($o in $all) {
            $entry = @($listed | Where-Object { $_['id'] -eq $o['id'] })
            if ($entry.Count -ne 1) { "$($o['id']) is not listed once"; continue }
            $e = $entry[0]
            if ($e['available'] -ne [bool]$o['available']) { "$($o['id']) available $($e['available'])" }
            if ([string]$e['default'] -ne [string]$o['default']) { "$($o['id']) default $($e['default'])" }
            if ([string]$e['value'] -ne [string]$o['default']) { "$($o['id']) value $($e['value']) on a new settings file" }
            if ($e['label'] -ne (Get-LauncherText -Key "option.$($o['id']).label" -Language 'pt-BR')) { "$($o['id']) label is not the pt-BR one" }
            if (@($e['values']).Count -ne @(Get-LauncherOptionValues -Option $o).Count) { "$($o['id']) values" }
        }
    }

    Test-Case 'launcher_set_option sets a toggle and saves it at once' {
        $data = Get-McpToolData (Invoke-McpTestTool -Name 'launcher_set_option' -Arguments ([ordered]@{ id = 'ray_tracing'; value = $true }))
        if ($data -is [string]) { return $data }
        if (-not $data['changed'].Contains('ray_tracing')) { 'changed does not name ray_tracing' }
        Test-Saved -Id 'ray_tracing' -Expected $true
    }

    Test-Case 'launcher_set_option takes a choice value given as a number' {
        $data = Get-McpToolData (Invoke-McpTestTool -Name 'launcher_set_option' -Arguments ([ordered]@{ id = 'card_memory'; value = 2048 }))
        if ($data -is [string]) { return $data }
        Test-Saved -Id 'card_memory' -Expected '2048'
    }

    Test-Case 'launcher_enable turns capture_frames on and names build=diag, which the rule changed too' {
        $data = Get-McpToolData (Invoke-McpTestTool -Name 'launcher_enable' -Arguments ([ordered]@{ id = 'capture_frames' }))
        if ($data -is [string]) { return $data }
        $changed = @($data['changed'].Keys) -join ','
        if ($changed -ne 'build,capture_frames') { "changed: $changed" }
        Test-Saved -Id 'capture_frames' -Expected $true
        Test-Saved -Id 'build' -Expected 'diag'
    }

    Test-Case 'launcher_set_option build=release turns capture_frames off' {
        $data = Get-McpToolData (Invoke-McpTestTool -Name 'launcher_set_option' -Arguments ([ordered]@{ id = 'build'; value = 'release' }))
        if ($data -is [string]) { return $data }
        if (-not $data['changed'].Contains('capture_frames')) { 'changed does not name capture_frames' }
        Test-Saved -Id 'capture_frames' -Expected $false
    }

    Test-Case 'launcher_disable turns water_light off and saves it' {
        $data = Get-McpToolData (Invoke-McpTestTool -Name 'launcher_disable' -Arguments ([ordered]@{ id = 'water_light' }))
        if ($data -is [string]) { return $data }
        Test-Saved -Id 'water_light' -Expected $false
    }

    Test-Case 'an id that is not in options.json is refused and nothing is saved' {
        $before = [System.IO.File]::ReadAllText($configPath)
        Test-Refused (Invoke-McpTestTool -Name 'launcher_set_option' -Arguments ([ordered]@{ id = 'VICTORY_LOG'; value = 'trace' })) -Contains 'VICTORY_LOG'
        Test-Refused (Invoke-McpTestTool -Name 'launcher_enable' -Arguments ([ordered]@{ id = 'no_such_option' })) -Contains 'no_such_option'
        if ([System.IO.File]::ReadAllText($configPath) -ne $before) { 'the saved file changed' }
    }

    Test-Case 'a value the option does not take is refused and nothing is saved' {
        $before = [System.IO.File]::ReadAllText($configPath)
        Test-Refused (Invoke-McpTestTool -Name 'launcher_set_option' -Arguments ([ordered]@{ id = 'profile'; value = 'series-z' })) -Contains 'series-z'
        Test-Refused (Invoke-McpTestTool -Name 'launcher_set_option' -Arguments ([ordered]@{ id = 'ray_tracing'; value = 'maybe' })) -Contains 'maybe'
        Test-Refused (Invoke-McpTestTool -Name 'launcher_enable' -Arguments ([ordered]@{ id = 'profile' })) -Contains 'profile'
        if ([System.IO.File]::ReadAllText($configPath) -ne $before) { 'the saved file changed' }
    }

    Test-Case 'an option or value not on this branch is refused with the reason' {
        $reflex = Get-LauncherOption -Id 'reflex'
        Test-Refused (Invoke-McpTestTool -Name 'launcher_enable' -Arguments ([ordered]@{ id = 'reflex' })) -Contains $reflex['note']
        Test-Refused (Invoke-McpTestTool -Name 'launcher_set_option' -Arguments ([ordered]@{ id = 'image'; value = 'dlss' })) -Contains 'DLSS'
        $saved = Get-SavedOptions
        if ($saved.Contains('reflex') -or $saved.Contains('image')) { 'an unavailable option was saved' }
    }

    Test-Case 'arguments outside the schema, a missing one and an unknown tool are -32602' {
        Test-ProtocolError (Invoke-McpTestTool -Name 'launcher_set_option' -Arguments ([ordered]@{ id = 'ray_tracing'; value = $true; path = 'C:\x' })) -Code -32602
        Test-ProtocolError (Invoke-McpTestTool -Name 'launcher_set_option' -Arguments ([ordered]@{ id = 'ray_tracing' })) -Code -32602
        Test-ProtocolError (Invoke-McpTestTool -Name 'launcher_set_option' -Arguments ([ordered]@{ id = 'ray_tracing'; value = @(1, 2) })) -Code -32602
        Test-ProtocolError (Invoke-McpTestTool -Name 'launcher_run_game') -Code -32602
    }

    Test-Case 'an unknown method is -32601' {
        Test-ProtocolError (Send-McpRequest -Method 'resources/list') -Code -32601
    }

    Test-Case 'a malformed line is answered -32700 and the server goes on' {
        Send-McpRaw -Line '{"jsonrpc":"2.0","id":'
        $a = Receive-McpMessage
        Test-ProtocolError $a -Code -32700
        if ($null -ne $a['id']) { 'the parse error carries an id' }
        Send-McpRaw -Line 'not json at all'
        Test-ProtocolError (Receive-McpMessage) -Code -32700
        $ping = Send-McpRequest -Method 'ping'
        if (-not $ping.Contains('result')) { 'no ping answer after the bad lines' }
    }

    Test-Case 'a batch is answered with an array' {
        Send-McpRaw -Line '[{"jsonrpc":"2.0","id":"a","method":"ping"},{"jsonrpc":"2.0","method":"notifications/initialized"},{"jsonrpc":"2.0","id":"b","method":"ping"}]'
        $line = Receive-McpLine
        if ($null -eq $line) { return 'no answer' }
        $answers = ConvertFrom-LauncherJsonText -Text $line
        $ids = @($answers | ForEach-Object { $_['id'] }) -join ','
        if ($ids -ne 'a,b') { "answers: $ids" }
    }

    Test-Case 'launcher_env returns the resolved run as a dry run' {
        $data = Get-McpToolData (Invoke-McpTestTool -Name 'launcher_env')
        if ($data -is [string]) { return $data }
        $expected = Resolve-LauncherRun
        if (-not $data['dry_run']) { 'dry_run is not true' }
        if ($data['play']['Build'] -ne $expected['Build'] -or $data['play']['Console'] -ne $expected['Console'] -or $data['play']['RayTracing'] -ne $expected['RayTracing']) { 'play.ps1 arguments differ from Resolve-LauncherRun' }
        foreach ($k in $expected['Environment'].Keys) {
            if (-not $data['environment'].Contains($k) -or [string]$data['environment'][$k] -ne [string]$expected['Environment'][$k]) { "environment $k differs" }
        }
        if (@($data['environment'].Keys).Count -ne @($expected['Environment'].Keys).Count) { 'environment has other switches' }
        if ((@($data['steps'] | ForEach-Object { $_['id'] }) -join ',') -ne 'build,shaders,warm,open') { 'steps' }
    }

    Test-Case 'launcher_reset with an id puts only that option back' {
        $data = Get-McpToolData (Invoke-McpTestTool -Name 'launcher_reset' -Arguments ([ordered]@{ id = 'ray_tracing' }))
        if ($data -is [string]) { return $data }
        Test-Saved -Id 'ray_tracing' -Expected $false
        Test-Saved -Id 'water_light' -Expected $false
    }

    Test-Case 'launcher_reset with no id puts every option back and names what moved' {
        $data = Get-McpToolData (Invoke-McpTestTool -Name 'launcher_reset')
        if ($data -is [string]) { return $data }
        $changed = @($data['changed'].Keys | Sort-Object) -join ','
        if ($changed -ne 'card_memory,water_light') { "changed: $changed" }
        $saved = Get-SavedOptions
        foreach ($o in Get-LauncherOptions) {
            if (-not $saved.Contains($o['id']) -or [string]$saved[$o['id']] -ne [string]$o['default']) { "$($o['id']) is not back to $($o['default'])" }
        }
    }

    Test-Case 'launcher_get_config and launcher_version answer' {
        $config = Get-McpToolData (Invoke-McpTestTool -Name 'launcher_get_config')
        if ($config -is [string]) { return $config }
        if ($config['path'] -ne $configPath) { "path $($config['path'])" }
        $version = Get-McpToolData (Invoke-McpTestTool -Name 'launcher_version')
        if ($version -is [string]) { return $version }
        if ($version['version'] -ne (Get-LauncherWorkspaceVersion)) { "version $($version['version'])" }
    }
} finally {
    $script:Child.StandardInput.Close()
    if (-not $script:Child.WaitForExit(20000)) { $script:Child.Kill() }
}

Test-Case 'the server exits cleanly when stdin closes, with no BOM and only protocol lines on stdout' {
    if ($script:Child.ExitCode -ne 0) { "exit code $($script:Child.ExitCode)" }
    $bytes = $script:AllBytes.ToArray()
    if ($bytes.Length -ge 3 -and $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF) { 'stdout starts with a BOM' }
    $rest = Receive-McpLine -TimeoutMs 2000
    if ($null -ne $rest) { "more output after the last answer: $rest" }
}

Test-Case 'the server wrote nothing but the settings file' {
    $files = @(Get-ChildItem -LiteralPath $tempDir -Recurse -Force | ForEach-Object { $_.Name })
    if (($files -join ',') -ne 'launcher.json') { "files: $($files -join ', ')" }
}

$env:VICTORY_LAUNCHER_CONFIG = $savedOverride
Remove-Item -LiteralPath $tempDir -Recurse -Force -ErrorAction SilentlyContinue
Write-Output ("{0} passed, {1} failed ({2}, server on {3})" -f $script:Passes, $script:Failures, $PSVersionTable.PSVersion, (Split-Path -Leaf $Shell))
exit $script:Failures
