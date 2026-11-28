# The launcher's MCP server: an MCP client (Claude Code, Claude Desktop, any other) reads and
# changes the launcher's saved options through it, the same file the window reads and writes.
# It speaks newline-delimited JSON-RPC 2.0 on stdin and stdout; its tools only change
# settings: none starts the game, builds or runs a command. See docs/launcher.md.
#   pwsh -NoProfile -File scripts\launcher-mcp.ps1
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\launcher-mcp.ps1
# VICTORY_LAUNCHER_CONFIG=<file> points it at another settings file (the tests do).
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
# Progress bars and the information stream would land on stdout, which is the protocol's.
$ProgressPreference = 'SilentlyContinue'
$InformationPreference = 'SilentlyContinue'

try {
    . (Join-Path $PSScriptRoot 'launcher\Launcher.ps1')
    . (Join-Path $PSScriptRoot 'launcher\McpTools.ps1')
    . (Join-Path $PSScriptRoot 'launcher\McpProtocol.ps1')
    Start-McpServer
} catch {
    [Console]::Error.WriteLine($_.Exception.Message)
    exit 1
}
exit 0
