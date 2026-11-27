# The launcher's self-check, with no window and nothing started: the data files are valid,
# every option round-trips through the saved file, the rules between options hold, the
# defaults resolve to exactly what scripts\play.ps1 sets with no parameter, and the language
# files have the same keys. It works on a settings file of its own (VICTORY_LAUNCHER_CONFIG
# when set, else a new temporary one), never on the user's.
#   powershell -File scripts\launcher\Test-Launcher.ps1     (or: scripts\launcher.ps1 -Check)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. (Join-Path $PSScriptRoot 'Launcher.ps1')

$script:Failures = 0
$script:Passes = 0

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

function Compare-LauncherMaps {
    param($Expected, $Actual, [string]$What)
    foreach ($k in $Expected.Keys) {
        if (-not $Actual.Contains($k)) { "$What lacks $k=$($Expected[$k])" }
        elseif ([string]$Actual[$k] -ne [string]$Expected[$k]) { "$What has $k=$($Actual[$k]), expected $($Expected[$k])" }
    }
    foreach ($k in $Actual.Keys) {
        if (-not $Expected.Contains($k)) { "$What has $k=$($Actual[$k]) too" }
    }
}

$savedOverride = [Environment]::GetEnvironmentVariable('VICTORY_LAUNCHER_CONFIG')
$ownDir = $null
if (-not $savedOverride) {
    $ownDir = Join-Path ([System.IO.Path]::GetTempPath()) ('launcher-check-' + [Guid]::NewGuid().ToString('N'))
    [Environment]::SetEnvironmentVariable('VICTORY_LAUNCHER_CONFIG', (Join-Path $ownDir 'launcher.json'))
}
$configPath = Get-LauncherConfigPath
Write-Output "PowerShell $($PSVersionTable.PSVersion) ($($PSVersionTable.PSEdition)); settings file $configPath"

try {
    Test-Case 'options.json is valid' {
        $doc = Read-LauncherJson -Path (Join-Path $script:LauncherDir 'options.json')
        Test-LauncherOptionsDocument -Document $doc
        if (@($doc['options']).Count -lt 10) { 'fewer than ten options' }
    }

    Test-Case 'tokens.json has every token of the design' {
        $tokens = Read-LauncherJson -Path (Join-Path $script:LauncherDir 'tokens.json')
        foreach ($group in 'colour', 'gradient', 'scene', 'glow', 'type', 'size', 'radius', 'space', 'motion') {
            if (-not $tokens.Contains($group)) { "no '$group'" }
        }
        $colours = 'night', 'night2', 'violet', 'magenta', 'rose', 'coral', 'gold', 'sunTop', 'sunMid', 'sunLow',
        'accent', 'onAccent', 'text', 'textSoft', 'textMuted', 'field', 'switchOff', 'switchKnobOff', 'focus',
        'panel', 'panelStrong', 'stroke', 'strokeStrong', 'rowLine'
        foreach ($c in $colours) {
            if (-not $tokens['colour'].Contains($c)) { "no colour '$c'"; continue }
            $v = $tokens['colour'][$c]
            if ($v -is [System.Collections.IDictionary]) { $v = $v['colour'] }
            if ([string]$v -notmatch '^#[0-9A-Fa-f]{6}$') { "colour '$c' is not #RRGGBB" }
        }
        foreach ($g in 'sky', 'sea', 'sun', 'play', 'fadeTop', 'fadeSide') {
            if (-not $tokens['gradient'].Contains($g)) { "no gradient '$g'" }
        }
        foreach ($k in 'backgroundFadeMs', 'viewFadeMs', 'glowBreathMs') {
            if (-not $tokens['motion'].Contains($k)) { "no motion '$k'" }
        }
        if ($tokens['size']['windowWidth'] -ne 960 -or $tokens['size']['windowHeight'] -ne 640) { 'the window is not 960 x 640' }
    }

    Test-Case 'the language files have the same keys' {
        $languages = Get-LauncherLanguages
        if ('en' -notin $languages -or 'pt-BR' -notin $languages) { 'en and pt-BR are not both there' }
        $en = Get-LauncherTextTable -Language 'en'
        foreach ($l in $languages) {
            $table = Get-LauncherTextTable -Language $l
            foreach ($k in $en.Keys) { if (-not $table.Contains($k)) { "$l lacks '$k'" } }
            foreach ($k in $table.Keys) { if (-not $en.Contains($k)) { "$l has '$k', which en has not" } }
            foreach ($k in $table.Keys) { if (-not ([string]$table[$k]).Trim()) { "$l has an empty '$k'" } }
        }
    }

    Test-Case 'every option, value, group and step has its texts' {
        $en = Get-LauncherTextTable -Language 'en'
        $keys = @()
        foreach ($o in Get-LauncherOptions) {
            $keys += "option.$($o['id']).label", "option.$($o['id']).hint", "group.$($o['group'])"
            if ($o['kind'] -eq 'choice') { foreach ($v in Get-LauncherOptionValues -Option $o) { $keys += "option.$($o['id']).value.$v" } }
        }
        foreach ($s in $script:LauncherSteps) { $keys += "prep.step.$s", "prep.step.$s.doing" }
        foreach ($k in $keys | Select-Object -Unique) { if (-not $en.Contains($k)) { "no text '$k'" } }
    }

    Test-Case 'the launcher scripts are plain ASCII (Windows PowerShell reads them without a BOM)' {
        $files = @(Get-ChildItem -LiteralPath $script:LauncherDir -Filter '*.ps1') + @(Get-Item -LiteralPath (Join-Path $script:LauncherRoot 'scripts\launcher.ps1'))
        $mcp = Join-Path $script:LauncherRoot 'scripts\launcher-mcp.ps1'
        if (Test-Path -LiteralPath $mcp) { $files += Get-Item -LiteralPath $mcp }
        foreach ($f in $files) {
            $bytes = [System.IO.File]::ReadAllBytes($f.FullName)
            foreach ($b in $bytes) { if ($b -gt 127) { "$($f.Name) has a byte over 127"; break } }
        }
    }

    Test-Case 'the translation switches are the ones the shader cache is keyed on' {
        $source = Join-Path $script:LauncherRoot 'crates\victory-gpu\src\shader_cache.rs'
        if (-not (Test-Path -LiteralPath $source)) { return 'no crates\victory-gpu\src\shader_cache.rs' }
        $text = [System.IO.File]::ReadAllText($source)
        $block = [regex]::Match($text, 'for name in \[(.*?)\]', 'Singleline')
        if (-not $block.Success) { return 'shader_cache.rs has no list of switches' }
        $inSource = @([regex]::Matches($block.Groups[1].Value, '"(VICTORY_[A-Z0-9_]+)"') | ForEach-Object { $_.Groups[1].Value })
        $listed = Get-LauncherTranslationSwitches
        foreach ($n in $inSource) { if ($n -notin $listed) { "options.json lacks $n" } }
        foreach ($n in $listed) { if ($n -notin $inSource) { "options.json has $n, which shader_cache.rs has not" } }
    }

    Test-Case 'with no saved file the settings are the defaults' {
        if (Test-Path -LiteralPath $configPath) { Remove-Item -LiteralPath $configPath -Force }
        $settings = Get-LauncherSettings
        foreach ($o in Get-LauncherOptions) {
            if ([string]$settings['options'][$o['id']] -ne [string]$o['default']) { "$($o['id']) is $($settings['options'][$o['id']]), not its default" }
        }
    }

    Test-Case 'the defaults resolve to exactly what scripts\play.ps1 sets with no parameter' {
        $settings = Reset-LauncherConfig
        $run = Resolve-LauncherRun -Settings $settings
        Compare-LauncherMaps -Expected (Get-PlayScriptSettings) -Actual $run['Environment'] -What 'the launcher'
        if ($run['Console'] -ne 'series-s') { "console $($run['Console'])" }
        if ($run['RayTracing']) { 'ray tracing on' }
    }

    Test-Case 'ray tracing resolves to what scripts\play.ps1 -RayTracing sets' {
        $settings = Set-LauncherOption -Id 'ray_tracing' -Value 'on'
        $run = Resolve-LauncherRun -Settings $settings
        Compare-LauncherMaps -Expected (Get-PlayScriptSettings -RayTracing) -Actual $run['Environment'] -What 'the launcher'
        if (-not $run['RayTracing']) { 'play.ps1 is not given -RayTracing' }
        $null = Reset-LauncherConfig
    }

    Test-Case 'every available value of every option round-trips through the saved file' {
        foreach ($o in Get-LauncherOptions) {
            if (-not $o['available']) { continue }
            foreach ($v in Get-LauncherOptionValues -Option $o) {
                if (-not (Test-LauncherValueAvailable -Option $o -Value $v)) { continue }
                $null = Reset-LauncherConfig
                $text = [string]$v
                if ($v -is [bool]) { $text = 'off'; if ($v) { $text = 'on' } }
                $back = Set-LauncherOption -Id $o['id'] -Value $text
                if ([string]$back['options'][$o['id']] -ne [string]$v) { "$($o['id'])=$text came back as $($back['options'][$o['id']])" }
                $reread = (Get-LauncherSettings)['options'][$o['id']]
                if ([string]$reread -ne [string]$v) { "$($o['id'])=$text read back from the file as $reread" }
                $onDisk = (Read-LauncherJson -Path $configPath)['options'][$o['id']]
                if ([string]$onDisk -ne [string]$v) { "$($o['id'])=$text is $onDisk in the file" }
            }
        }
        $null = Reset-LauncherConfig
    }

    Test-Case 'options and values that are not on this branch are refused and never sent' {
        foreach ($o in Get-LauncherOptions) {
            if (-not $o['available']) {
                $refused = $false
                try { $null = Set-LauncherOption -Id $o['id'] -Value ([string]$o['default']) } catch { $refused = $true }
                if (-not $refused) { "$($o['id']) is not available but was set" }
                $effect = Get-LauncherEffect -Option $o -Value $true
                if ($effect.Contains('env')) {
                    $run = Resolve-LauncherRun
                    foreach ($k in $effect['env'].Keys) { if ($run['More'].Contains($k)) { "$($o['id']) sent $k" } }
                }
            }
            if ($o['kind'] -eq 'choice') {
                foreach ($v in Get-LauncherOptionValues -Option $o) {
                    if ($o['available'] -and -not (Test-LauncherValueAvailable -Option $o -Value $v)) {
                        $refused = $false
                        try { $null = Set-LauncherOption -Id $o['id'] -Value $v } catch { $refused = $true }
                        if (-not $refused) { "$($o['id'])=$v is not available but was set" }
                    }
                }
            }
        }
        $refused = $false
        try { $null = Set-LauncherOption -Id 'no_such_option' -Value 'on' } catch { $refused = $true }
        if (-not $refused) { 'an unknown option was set' }
        $refused = $false
        try { $null = Set-LauncherOption -Id 'card_memory' -Value '4096' } catch { $refused = $true }
        if (-not $refused) { 'card_memory=4096 was set' }
    }

    Test-Case 'a toggle that needs another value brings it, and loses it' {
        $null = Reset-LauncherConfig
        $s = Set-LauncherOption -Id 'capture_frames' -Value 'on'
        if ($s['options']['build'] -ne 'diag') { 'capture_frames on did not set build=diag' }
        if ('build' -notin $s['changed']) { 'build is not named as changed' }
        $s = Set-LauncherOption -Id 'build' -Value 'release'
        if ($s['options']['capture_frames']) { 'build=release left capture_frames on' }
        $null = Reset-LauncherConfig
    }

    Test-Case 'keys the launcher does not know are kept' {
        $data = [ordered]@{ format = 1; options = [ordered]@{ thin_lists = $false; from_a_newer_version = 'x' }; someone_else = [ordered]@{ kept = $true } }
        Write-LauncherJson -Path $configPath -Data $data
        $null = Set-LauncherOption -Id 'water_light' -Value 'off'
        $back = Read-LauncherJson -Path $configPath
        if (-not $back.Contains('someone_else') -or -not $back['someone_else']['kept']) { 'the unknown top-level key went' }
        if ($back['options']['from_a_newer_version'] -ne 'x') { 'the unknown option went' }
        if ($back['options']['thin_lists'] -ne $false) { 'thin_lists changed' }
        $null = Reset-LauncherConfig
    }

    Test-Case 'the saved file is UTF-8 without a BOM and no temporary file is left' {
        # A folder name with accents (written as code points: this file stays ASCII).
        $folder = 'pasta com acentua' + [char]0x00E7 + [char]0x00E3 + 'o'
        $null = Set-LauncherSetting -Name 'game_exe' -Value (Join-Path ([System.IO.Path]::GetTempPath()) "$folder\title.exe")
        $bytes = [System.IO.File]::ReadAllBytes($configPath)
        if ($bytes.Length -ge 3 -and $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF) { 'the file starts with a BOM' }
        if ((Get-LauncherSettings)['game_exe'] -notlike '*acentua*o\title.exe') { 'the path did not come back' }
        $left = @(Get-ChildItem -LiteralPath (Split-Path -Parent $configPath) -Filter '*.tmp')
        if ($left.Count -gt 0) { "left $($left[0].Name)" }
        $null = Set-LauncherSetting -Name 'game_exe' -Value ''
    }

    Test-Case 'a broken settings file is set aside and the defaults are used' {
        [System.IO.File]::WriteAllText($configPath, '{ "options": { this is not json')
        $settings = Get-LauncherSettings
        if (-not (Test-Path -LiteralPath "$configPath.bad")) { 'no launcher.json.bad' }
        if (Test-Path -LiteralPath $configPath) { 'the broken file is still in place' }
        if ($settings['options']['thin_lists'] -ne $true) { 'not the defaults' }
        Remove-Item -LiteralPath "$configPath.bad" -Force
    }

    Test-Case 'the language comes from the saved setting' {
        $null = Set-LauncherSetting -Name 'language' -Value 'en'
        if ((Get-LauncherText -Key 'play.button') -ne 'PLAY') { 'en is not used' }
        $null = Set-LauncherSetting -Name 'language' -Value 'pt-BR'
        if ((Get-LauncherText -Key 'play.button') -ne 'JOGAR') { 'pt-BR is not used' }
        $refused = $false
        try { $null = Set-LauncherSetting -Name 'language' -Value 'xx' } catch { $refused = $true }
        if (-not $refused) { 'an unknown language was saved' }
        $null = Set-LauncherSetting -Name 'language' -Value ''
    }

    Test-Case 'the version reads v<version> (<commit>)' {
        $v = Get-LauncherVersion -Language 'en'
        if ($v['text'] -notmatch '^v\d+\.\d+\.\d+( \([0-9a-f]{4,}\))?$') { "it reads '$($v['text'])'" }
        if ($v['version'] -ne (Get-LauncherWorkspaceVersion)) { 'not the workspace version' }
    }

    Test-Case 'the dry run returns the steps and starts nothing' {
        $null = Reset-LauncherConfig
        $plan = Get-LauncherPlayPlan -Language 'en'
        $ids = @($plan['steps'] | ForEach-Object { $_['id'] })
        if (($ids -join ',') -ne ($script:LauncherSteps -join ',')) { "steps $($ids -join ',')" }
        $open = $plan['steps'][3]
        if ($open['command'] -notlike '*play.ps1*-Console*series-s*') { "open runs $($open['command'])" }
        $lines = @(Invoke-LauncherPlay -DryRun -Language 'en')
        if ($lines.Count -lt 1) { 'the dry run returned no command line' }
        if (Test-Path -LiteralPath (Get-LauncherStampPath)) { 'the dry run wrote the stamp' }
    }
} finally {
    if ($ownDir) {
        Remove-Item -LiteralPath $ownDir -Recurse -Force -ErrorAction SilentlyContinue
        [Environment]::SetEnvironmentVariable('VICTORY_LAUNCHER_CONFIG', $null)
    }
}

Write-Output ("{0} passed, {1} failed" -f $script:Passes, $script:Failures)
if ($script:Failures -gt 0) { exit 1 }
exit 0
