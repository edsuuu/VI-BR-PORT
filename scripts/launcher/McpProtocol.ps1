# The Model Context Protocol part of the launcher's MCP server: one JSON-RPC 2.0 message in,
# at most one answer out. Handles initialize, notifications/initialized, ping, tools/list and
# tools/call; a method it does not know is -32601, bad params -32602, a line that is not JSON
# -32700. Dot-source after Launcher.ps1 and McpTools.ps1.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# Newest first. initialize answers with the client's version when it is one of these.
$script:McpProtocolVersions = @('2025-11-25', '2025-06-18', '2025-03-26', '2024-11-05')
$script:McpServerName = 'vi-br-port-launcher'

function New-McpResponse {
    param($Id, $Result)
    $r = [ordered]@{}
    $r['jsonrpc'] = '2.0'
    $r['id'] = $Id
    $r['result'] = $Result
    return $r
}

function New-McpError {
    param($Id, [int]$Code, [string]$Message)
    $e = [ordered]@{}
    $e['code'] = $Code
    $e['message'] = $Message
    $r = [ordered]@{}
    $r['jsonrpc'] = '2.0'
    $r['id'] = $Id
    $r['error'] = $e
    return $r
}

function Get-McpInitializeResult {
    param($Params)
    $asked = $null
    if ($Params -is [System.Collections.IDictionary] -and $Params.Contains('protocolVersion')) { $asked = [string]$Params['protocolVersion'] }
    $version = $script:McpProtocolVersions[0]
    if ($asked -and $asked -in $script:McpProtocolVersions) { $version = $asked }
    $info = [ordered]@{}
    $info['name'] = $script:McpServerName
    $info['version'] = [string](Get-McpVersion)['version']
    $result = [ordered]@{}
    $result['protocolVersion'] = $version
    $result['capabilities'] = [ordered]@{ tools = [ordered]@{} }
    $result['serverInfo'] = $info
    $result['instructions'] = Get-LauncherText -Key 'mcp.server.description'
    return $result
}

# One parsed message (a dictionary) to its answer, or $null for a notification.
function Invoke-McpMessage {
    param($Message)
    if ($Message -isnot [System.Collections.IDictionary]) { return New-McpError -Id $null -Code -32600 -Message 'Invalid Request' }
    $hasId = $Message.Contains('id')
    $id = $Message['id']
    $method = $Message['method']
    if ($Message['jsonrpc'] -ne '2.0' -or $method -isnot [string]) {
        # An answer to something this server never asked has no reply.
        if (-not $Message.Contains('method') -and ($Message.Contains('result') -or $Message.Contains('error'))) { return $null }
        return New-McpError -Id $id -Code -32600 -Message 'Invalid Request'
    }
    $params = $Message['params']
    if ($null -ne $params -and $params -isnot [System.Collections.IDictionary]) {
        if (-not $hasId) { return $null }
        return New-McpError -Id $id -Code -32602 -Message 'params must be an object'
    }
    if (-not $hasId) {
        # notifications/initialized, notifications/cancelled and the others need no answer.
        return $null
    }
    try {
        switch ($method) {
            'initialize' { return New-McpResponse -Id $id -Result (Get-McpInitializeResult -Params $params) }
            'ping' { return New-McpResponse -Id $id -Result ([ordered]@{}) }
            'tools/list' {
                $result = [ordered]@{}
                $result['tools'] = Get-McpToolList -Language (Get-LauncherLanguage -Config (Get-LauncherConfig))
                return New-McpResponse -Id $id -Result $result
            }
            'tools/call' {
                $name = $null
                $arguments = $null
                if ($null -ne $params) {
                    $name = $params['name']
                    $arguments = $params['arguments']
                }
                if ($name -isnot [string]) { return New-McpError -Id $id -Code -32602 -Message 'tools/call needs a tool name' }
                return New-McpResponse -Id $id -Result (Invoke-McpToolCall -Name $name -Arguments $arguments)
            }
            default { return New-McpError -Id $id -Code -32601 -Message "Method not found: $method" }
        }
    } catch [System.ArgumentException] {
        return New-McpError -Id $id -Code -32602 -Message $_.Exception.Message
    } catch {
        return New-McpError -Id $id -Code -32603 -Message $_.Exception.Message
    }
}

# One line of the input to the line to write back, or $null when there is nothing to answer.
# A batch (a JSON array, from protocol 2025-03-26) is answered with an array.
function Invoke-McpLine {
    param([AllowEmptyString()][string]$Line)
    if ($Line.Trim().Length -eq 0) { return $null }
    try {
        $parsed = ConvertFrom-Json -InputObject $Line
    } catch {
        return ConvertTo-McpLine (New-McpError -Id $null -Code -32700 -Message 'Parse error')
    }
    $trimmed = $Line.TrimStart()
    if ($trimmed.StartsWith('[')) {
        # (PowerShell 7 may hand a one-item array back as the item itself.)
        $items = ConvertTo-LauncherData $parsed
        if ($null -eq $items) { $items = @() }
        if ($items -isnot [array]) { $items = @(, $items) }
        if ($items.Count -eq 0) { return ConvertTo-McpLine (New-McpError -Id $null -Code -32600 -Message 'Invalid Request') }
        $answers = @()
        foreach ($item in $items) {
            $answer = Invoke-McpMessage -Message $item
            if ($null -ne $answer) { $answers += , $answer }
        }
        if ($answers.Count -eq 0) { return $null }
        return ConvertTo-McpLine -Data $answers
    }
    return ConvertTo-McpLine (Invoke-McpMessage -Message (ConvertTo-LauncherData $parsed))
}

function ConvertTo-McpLine {
    param($Data)
    if ($null -eq $Data) { return $null }
    # -Compress keeps one message on one line (a line break inside a string is written \n).
    return (ConvertTo-Json -InputObject $Data -Depth 32 -Compress)
}

# Reads messages from stdin and writes answers to stdout, UTF-8 without BOM, one per line,
# until stdin closes. Nothing else is written to stdout: logs go to stderr.
function Start-McpServer {
    $utf8 = New-Object System.Text.UTF8Encoding $false
    $reader = New-Object System.IO.StreamReader ([Console]::OpenStandardInput()), $utf8
    $writer = New-Object System.IO.StreamWriter ([Console]::OpenStandardOutput()), $utf8
    $writer.NewLine = "`n"
    $writer.AutoFlush = $true
    $null = Get-McpVersion
    [Console]::Error.WriteLine("$($script:McpServerName) $((Get-McpVersion)['text']): settings in $(Get-LauncherConfigPath)")
    while ($true) {
        $line = $reader.ReadLine()
        if ($null -eq $line) { break }
        $answer = $null
        try {
            $answer = Invoke-McpLine -Line $line
        } catch {
            [Console]::Error.WriteLine("error: $($_.Exception.Message)")
            $answer = ConvertTo-McpLine (New-McpError -Id $null -Code -32603 -Message $_.Exception.Message)
        }
        if ($null -ne $answer) { $writer.WriteLine($answer) }
    }
}
