# JSON files the launcher reads and writes, the same on Windows PowerShell 5.1 and PowerShell 7:
# objects come back as ordered dictionaries (no -AsHashtable on 5.1), files are written as UTF-8
# without a byte-order mark, through a temporary file moved into place.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function ConvertTo-LauncherData {
    param($InputObject)
    if ($null -eq $InputObject) { return $null }
    if ($InputObject -is [System.Management.Automation.PSCustomObject]) {
        $out = [ordered]@{}
        foreach ($p in $InputObject.PSObject.Properties) { $out[$p.Name] = ConvertTo-LauncherData $p.Value }
        return $out
    }
    if ($InputObject -is [System.Collections.IDictionary]) {
        $out = [ordered]@{}
        foreach ($k in $InputObject.Keys) { $out[[string]$k] = ConvertTo-LauncherData $InputObject[$k] }
        return $out
    }
    if (($InputObject -is [System.Collections.IEnumerable]) -and ($InputObject -isnot [string])) {
        $list = New-Object System.Collections.Generic.List[object]
        foreach ($item in $InputObject) { $list.Add((ConvertTo-LauncherData $item)) }
        return , $list.ToArray()
    }
    return $InputObject
}

function ConvertFrom-LauncherJsonText {
    param([Parameter(Mandatory)][AllowEmptyString()][string]$Text)
    if ($Text.Trim().Length -eq 0) { throw 'empty JSON text' }
    return ConvertTo-LauncherData ($Text | ConvertFrom-Json)
}

function Read-LauncherJson {
    param([Parameter(Mandatory)][string]$Path)
    $text = [System.IO.File]::ReadAllText($Path, [System.Text.Encoding]::UTF8)
    return ConvertFrom-LauncherJsonText -Text $text
}

function ConvertTo-LauncherJsonText {
    param([Parameter(Mandatory)]$Data)
    return (ConvertTo-Json -InputObject $Data -Depth 32)
}

function Write-LauncherJson {
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)]$Data)
    $dir = Split-Path -Parent $Path
    if ($dir -and -not (Test-Path -LiteralPath $dir)) { New-Item -ItemType Directory -Path $dir -Force | Out-Null }
    $text = (ConvertTo-LauncherJsonText -Data $Data) + "`n"
    $temp = '{0}.{1}.tmp' -f $Path, [System.Diagnostics.Process]::GetCurrentProcess().Id
    [System.IO.File]::WriteAllText($temp, $text, (New-Object System.Text.UTF8Encoding $false))
    # Another process (the window, the MCP server) may be reading the file this moment.
    for ($try = 1; ; $try++) {
        try {
            if ([System.IO.File]::Exists($Path)) { [System.IO.File]::Replace($temp, $Path, [NullString]::Value) }
            else { [System.IO.File]::Move($temp, $Path) }
            return
        } catch [System.IO.IOException], [System.UnauthorizedAccessException] {
            if ($try -ge 20) {
                Remove-Item -LiteralPath $temp -Force -ErrorAction SilentlyContinue
                throw
            }
            Start-Sleep -Milliseconds 50
        }
    }
}
