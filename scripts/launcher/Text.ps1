# The texts the user sees, from lang\<language>.json. A key missing in one language falls back
# to English, then to the key itself.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$script:LauncherTextCache = @{}

function Get-LauncherLanguages {
    $dir = Join-Path $script:LauncherDir 'lang'
    return @(Get-ChildItem -LiteralPath $dir -Filter '*.json' | ForEach-Object { $_.BaseName } | Sort-Object)
}

# The saved language if there is a file for it, else Portuguese for a Portuguese Windows and
# English for any other.
function Get-LauncherLanguage {
    param($Config)
    $languages = Get-LauncherLanguages
    if ($null -ne $Config -and $Config.Contains('language') -and $Config['language'] -in $languages) { return $Config['language'] }
    $culture = [System.Globalization.CultureInfo]::CurrentUICulture.Name
    if ($culture -like 'pt*' -and 'pt-BR' -in $languages) { return 'pt-BR' }
    return 'en'
}

function Get-LauncherTextTable {
    param([Parameter(Mandatory)][string]$Language)
    if (-not $script:LauncherTextCache.ContainsKey($Language)) {
        $path = Join-Path $script:LauncherDir "lang\$Language.json"
        $table = [ordered]@{}
        if (Test-Path -LiteralPath $path) { $table = Read-LauncherJson -Path $path }
        $script:LauncherTextCache[$Language] = $table
    }
    return $script:LauncherTextCache[$Language]
}

function Get-LauncherText {
    param([Parameter(Mandatory)][string]$Key, [string]$Language, [object[]]$Arguments)
    if (-not $Language) {
        $Language = 'en'
        try { $Language = Get-LauncherLanguage -Config (Get-LauncherConfig) } catch { }
    }
    $text = $null
    foreach ($l in @($Language, 'en')) {
        $table = Get-LauncherTextTable -Language $l
        if ($table.Contains($Key)) { $text = [string]$table[$Key]; break }
    }
    if ($null -eq $text) { $text = $Key }
    if ($null -ne $Arguments -and $Arguments.Count -gt 0) { $text = [string]::Format($text, $Arguments) }
    return $text
}
