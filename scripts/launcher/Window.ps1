# The launcher's window: one 960 x 640 window with three views (Play, Preparing, More options)
# that fade into each other. Every change is saved at once through the core's functions; the
# window reads the saved file again when it changes on disk (the MCP server or the command line
# may change it). New-LauncherWindow builds the window without showing it (the -Snapshot mode
# draws it off screen); Show-LauncherWindow shows it and runs until it is closed.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

Add-Type -AssemblyName PresentationFramework, PresentationCore, WindowsBase
foreach ($part in 'Theme', 'Views', 'PlayRunner', 'Snapshot') { . (Join-Path $PSScriptRoot "$part.ps1") }

# The only page the launcher ever opens.
$script:LauncherGithubUrl = 'https://github.com/edsuuu/VI-BR-PORT'
$script:LauncherUi = $null

# Whether Windows wants animations (Settings > Accessibility > Animation effects).
function Test-LauncherMotion {
    return [bool][System.Windows.SystemParameters]::ClientAreaAnimation
}

# Runs a window action; what goes wrong is said in the window instead of being lost.
function Invoke-LauncherUiAction {
    param([Parameter(Mandatory)][scriptblock]$Action)
    try { & $Action } catch { Show-LauncherUiProblem -Message $_.Exception.Message }
}

function Show-LauncherUiProblem {
    param([Parameter(Mandatory)][AllowEmptyString()][string]$Message)
    $ui = $script:LauncherUi
    if ($null -eq $ui -or $null -eq $ui.Root) { return }
    if ($ui.View -eq 'preparing') { Show-LauncherPrepError -Message $Message; return }
    (Get-LauncherUiElement 'StatusText').Text = $Message
    (Get-LauncherUiElement 'StatusDot').Fill = Get-LauncherUiBrush 'rose'
}

# What a set of settings shows (options, executable, language), to compare what the window
# shows with what is saved.
function Get-LauncherSettingsFingerprint {
    param([Parameter(Mandatory)]$Settings)
    $shown = [ordered]@{}
    foreach ($k in 'options', 'game_exe', 'language') { $shown[$k] = $Settings[$k] }
    return (ConvertTo-LauncherJsonText -Data $shown)
}

# The saved file's last write and length: a cheap look to skip reading it when it did not move.
function Get-LauncherConfigStamp {
    $item = New-Object System.IO.FileInfo (Get-LauncherConfigPath)
    try {
        if (-not $item.Exists) { return 'none' }
        return ('{0}:{1}' -f $item.LastWriteTimeUtc.Ticks, $item.Length)
    } catch [System.IO.IOException] {
        # Being swapped in this moment: a stamp that matches nothing, so it is read.
        return [guid]::NewGuid().ToString()
    }
}

# Builds (or builds again, after the language changed) the window's content.
function Initialize-LauncherWindowContent {
    $ui = $script:LauncherUi
    $context = New-LauncherViewContext -Language $ui.Language
    $root = [System.Windows.Markup.XamlReader]::Parse((Get-LauncherWindowXaml -Context $context))
    $ui.Root = $root
    $ui.Context = $context
    $ui.Window.Content = $root
    $ui.Window.Title = Get-LauncherText -Key 'window.title' -Language $ui.Language
    $ui.Window.Background = $root.FindResource('Brush.night')
    foreach ($meta in $context.Controls) {
        $control = $root.FindName($meta.Name)
        $control.Tag = $meta
        switch ($meta.Kind) {
            'segment' { $control.Add_Click({ param($sender, $e) Invoke-LauncherUiAction { Set-LauncherUiOption -Id $sender.Tag.Id -Value $sender.Tag.Value } }) }
            'switch' { $control.Add_Click({ param($sender, $e) Invoke-LauncherUiAction { Set-LauncherUiOption -Id $sender.Tag.Id -Value ([bool]$sender.IsChecked) } }) }
            'combo' {
                $control.Add_SelectionChanged({
                        param($sender, $e)
                        if ($script:LauncherUi.Updating -or $null -eq $sender.SelectedItem) { return }
                        Invoke-LauncherUiAction { Set-LauncherUiOption -Id $sender.Tag.Id -Value ([string]$sender.SelectedItem.Tag) }
                    })
            }
        }
    }
    (Get-LauncherUiElement 'PlayButton').Add_Click({ Invoke-LauncherUiAction { Start-LauncherUiPlay } })
    (Get-LauncherUiElement 'ChooseButton').Add_Click({ Invoke-LauncherUiAction { Select-LauncherGameExe } })
    (Get-LauncherUiElement 'ChangeGameButton').Add_Click({ Invoke-LauncherUiAction { Select-LauncherGameExe } })
    (Get-LauncherUiElement 'MoreButton').Add_Click({ Invoke-LauncherUiAction { Set-LauncherView -Name 'more' } })
    (Get-LauncherUiElement 'MoreBack').Add_Click({ Invoke-LauncherUiAction { Set-LauncherView -Name 'play' } })
    (Get-LauncherUiElement 'MoreReset').Add_Click({ Invoke-LauncherUiAction { Update-LauncherUi -Settings (Reset-LauncherConfig) } })
    (Get-LauncherUiElement 'PrepCancel').Add_Click({ Invoke-LauncherUiAction { Stop-LauncherUiPlay } })
    (Get-LauncherUiElement 'GithubButton').Add_Click({ Invoke-LauncherUiAction { Start-Process $script:LauncherGithubUrl } })
    Update-LauncherUi -Settings (Get-LauncherSettings)
}

# Saves one option (with the rules between options) and shows what changed.
function Set-LauncherUiOption {
    param([Parameter(Mandatory)][string]$Id, [Parameter(Mandatory)]$Value)
    if ($script:LauncherUi.Updating) { return }
    try {
        $settings = Set-LauncherOption -Id $Id -Value $Value
    } catch {
        # A refused value: the controls go back to what is saved, and the reason is shown.
        Update-LauncherUi -Settings (Get-LauncherSettings)
        throw
    }
    Update-LauncherUi -Settings $settings
}

# Every control set from the settings; the Play button or "Choose the executable"; the status.
function Update-LauncherUi {
    param([Parameter(Mandatory)]$Settings)
    $ui = $script:LauncherUi
    $ui.Updating = $true
    try {
        foreach ($meta in $ui.Context.Controls) {
            $control = $ui.Root.FindName($meta.Name)
            $value = $Settings['options'][$meta.Id]
            switch ($meta.Kind) {
                'segment' { $control.IsChecked = ([string]$value -eq $meta.Value) }
                # An option not on this version is drawn off: it is never sent.
                'switch' { $control.IsChecked = ([bool]$value -and $control.IsEnabled) }
                'combo' {
                    foreach ($item in $control.Items) {
                        if ([string]$item.Tag -eq [string]$value) { $control.SelectedItem = $item }
                    }
                }
            }
        }
        $hasGame = [bool]$Settings['game_exe']
        $shown = 'Collapsed'
        $hidden = 'Visible'
        if ($hasGame) { $shown = 'Visible'; $hidden = 'Collapsed' }
        (Get-LauncherUiElement 'PlayButton').Visibility = $shown
        (Get-LauncherUiElement 'ChangeGameButton').Visibility = $shown
        (Get-LauncherUiElement 'ChooseButton').Visibility = $hidden
        Update-LauncherUiStatus -Settings $Settings
    } finally {
        $ui.Updating = $false
    }
    $ui.Settings = $Settings
    # What is shown, not the file's stamp now: a save another process makes while this ran
    # must still be seen by Sync-LauncherUiFromDisk.
    $ui.Shown = Get-LauncherSettingsFingerprint -Settings $Settings
}

# The line under the Play button: ready, what the first launch does, or what stops Play.
function Update-LauncherUiStatus {
    param([Parameter(Mandatory)]$Settings)
    $ui = $script:LauncherUi
    $dot = 'gold'
    if (-not $Settings['game_exe']) {
        $text = Get-LauncherText -Key 'play.status.no_game' -Language $ui.Language
    } else {
        $plan = Get-LauncherPlayPlan -Settings $Settings -Language $ui.Language
        if ($plan['problem']) {
            $text = $plan['problem']
            $dot = 'rose'
        } else {
            $shaders = @($plan['steps'] | Where-Object { $_['id'] -eq 'shaders' })[0]
            $key = 'play.status.ready'
            if ($shaders['needed']) { $key = 'play.status.prepare' }
            $text = Get-LauncherText -Key $key -Language $ui.Language
        }
    }
    (Get-LauncherUiElement 'StatusText').Text = $text
    (Get-LauncherUiElement 'StatusDot').Fill = Get-LauncherUiBrush $dot
}

# The file picker for the game's executable, saved at once.
function Select-LauncherGameExe {
    $ui = $script:LauncherUi
    $dialog = New-Object Microsoft.Win32.OpenFileDialog
    $dialog.Title = Get-LauncherText -Key 'play.choose_game' -Language $ui.Language
    $dialog.Filter = Get-LauncherText -Key 'play.choose_game.filter' -Language $ui.Language
    $dialog.CheckFileExists = $true
    if ($ui.Settings -and $ui.Settings['game_exe']) {
        $dir = Split-Path -Parent $ui.Settings['game_exe']
        if ($dir -and (Test-Path -LiteralPath $dir)) { $dialog.InitialDirectory = $dir }
    }
    if ($dialog.ShowDialog($ui.Window)) {
        Update-LauncherUi -Settings (Set-LauncherSetting -Name 'game_exe' -Value $dialog.FileName)
    }
}

# The saved file read again when it changed on disk; a new language builds the content again.
function Sync-LauncherUiFromDisk {
    $ui = $script:LauncherUi
    if ($null -ne $ui.Running) { return }
    # The stamp is taken before the file is read: a save that lands after the read moves it
    # again, and the next look reads once more.
    $stamp = Get-LauncherConfigStamp
    if ($stamp -eq $ui.ConfigStamp) { return }
    $ui.ConfigStamp = $stamp
    $settings = Get-LauncherSettings
    if ((Get-LauncherSettingsFingerprint -Settings $settings) -eq $ui.Shown) { return }
    if (-not $ui.LanguageFixed -and $settings['language'] -ne $ui.Language) {
        $view = $ui.View
        $ui.Language = $settings['language']
        Initialize-LauncherWindowContent
        Set-LauncherView -Name $view -Instant
        return
    }
    Update-LauncherUi -Settings $settings
}

function Get-LauncherViewElementName {
    param([Parameter(Mandatory)][string]$Name)
    switch ($Name.ToLowerInvariant()) {
        'play' { return 'PlayView' }
        'preparing' { return 'PrepView' }
        'more' { return 'MoreView' }
        'options' { return 'MoreView' }
    }
    throw "there is no view '$Name' (play, preparing, more)"
}

# Shows one view; the others fade out (200 ms, none when Windows turned animations off).
function Set-LauncherView {
    param([Parameter(Mandatory)][string]$Name, [switch]$Instant)
    $ui = $script:LauncherUi
    $target = Get-LauncherViewElementName -Name $Name
    $previous = $ui.View
    $ui.View = @{ PlayView = 'play'; PrepView = 'preparing'; MoreView = 'more' }[$target]
    $animate = (-not $Instant) -and (Test-LauncherMotion) -and $ui.Window.IsVisible
    $duration = New-Object System.Windows.Duration ([TimeSpan]::FromMilliseconds([double](Get-LauncherToken 'motion.viewFadeMs')))
    foreach ($viewName in 'PlayView', 'PrepView', 'MoreView') {
        $view = Get-LauncherUiElement $viewName
        if ($viewName -eq $target) {
            $view.Visibility = 'Visible'
            if ($animate) {
                $fade = New-Object System.Windows.Media.Animation.DoubleAnimation (0.0, 1.0, $duration)
                $view.BeginAnimation([System.Windows.UIElement]::OpacityProperty, $fade)
            } else {
                $view.BeginAnimation([System.Windows.UIElement]::OpacityProperty, $null)
                $view.Opacity = 1.0
            }
        } elseif ($view.Visibility -eq 'Visible') {
            if ($animate) {
                $fade = New-Object System.Windows.Media.Animation.DoubleAnimation (1.0, 0.0, $duration)
                $fade.Add_Completed({
                        foreach ($n in 'PlayView', 'PrepView', 'MoreView') {
                            $v = $script:LauncherUi.Root.FindName($n)
                            if ($n -ne (Get-LauncherViewElementName -Name $script:LauncherUi.View)) { $v.Visibility = 'Collapsed' }
                        }
                    })
                $view.BeginAnimation([System.Windows.UIElement]::OpacityProperty, $fade)
                # Not reachable by Tab or the mouse while it fades out.
                $view.IsHitTestVisible = $false
            } else {
                $view.Visibility = 'Collapsed'
            }
        }
        if ($viewName -eq $target) { $view.IsHitTestVisible = $true }
    }
    if ($ui.Window.IsVisible) { Set-LauncherViewFocus -Previous $previous }
}

# Keyboard focus where the user expects it after a view changed.
function Set-LauncherViewFocus {
    param([string]$Previous)
    $ui = $script:LauncherUi
    $focus = $null
    switch ($ui.View) {
        'more' { $focus = @($ui.Context.Controls | Where-Object { $_.View -eq 'more' -and $ui.Root.FindName($_.Name).IsEnabled } | Select-Object -First 1) | ForEach-Object { $ui.Root.FindName($_.Name) } }
        'preparing' { $focus = Get-LauncherUiElement 'PrepCancel' }
        'play' {
            if ($Previous -eq 'more') { $focus = Get-LauncherUiElement 'MoreButton' }
            elseif ((Get-LauncherUiElement 'PlayButton').Visibility -eq 'Visible') { $focus = Get-LauncherUiElement 'PlayButton' }
            else { $focus = Get-LauncherUiElement 'ChooseButton' }
        }
    }
    if ($null -ne $focus) { [void]$focus.Focus() }
}

# The background: the sunset fades in from the night colour, and the sun's glow breathes.
function Start-LauncherWindowMotion {
    $ui = $script:LauncherUi
    $night = Get-LauncherUiElement 'Night'
    $glow = Get-LauncherUiElement 'SunGlow'
    if (-not (Test-LauncherMotion)) {
        $night.Opacity = 0.0
        $glow.Opacity = [double](Get-LauncherToken 'motion.glowMax')
        return
    }
    $fadeIn = New-Object System.Windows.Media.Animation.DoubleAnimation (1.0, 0.0, (New-Object System.Windows.Duration ([TimeSpan]::FromMilliseconds([double](Get-LauncherToken 'motion.backgroundFadeMs')))))
    $fadeIn.EasingFunction = New-Object System.Windows.Media.Animation.QuadraticEase
    $night.BeginAnimation([System.Windows.UIElement]::OpacityProperty, $fadeIn)
    # Low to high in half the breath, and back.
    $half = [double](Get-LauncherToken 'motion.glowBreathMs') / 2
    $breath = New-Object System.Windows.Media.Animation.DoubleAnimation ([double](Get-LauncherToken 'motion.glowMin'), [double](Get-LauncherToken 'motion.glowMax'), (New-Object System.Windows.Duration ([TimeSpan]::FromMilliseconds($half))))
    $breath.AutoReverse = $true
    $breath.RepeatBehavior = [System.Windows.Media.Animation.RepeatBehavior]::Forever
    $breath.EasingFunction = New-Object System.Windows.Media.Animation.SineEase
    $glow.BeginAnimation([System.Windows.UIElement]::OpacityProperty, $breath)
}

# The window, built and filled with the saved settings, not shown. -Language draws it in a
# language other than the saved one (nothing is saved).
function New-LauncherWindow {
    param([ValidateSet('play', 'preparing', 'more', 'options')][string]$View = 'play', [string]$Language)
    $fixed = [bool]$Language
    if (-not $Language) { $Language = Get-LauncherLanguage -Config (Get-LauncherConfig) }
    if ($Language -notin (Get-LauncherLanguages)) {
        throw (Get-LauncherText -Key 'error.bad_value' -Language 'en' -Arguments @('language', $Language, ((Get-LauncherLanguages) -join ', ')))
    }
    $window = New-Object System.Windows.Window
    $window.SizeToContent = 'WidthAndHeight'
    $window.ResizeMode = 'CanMinimize'
    $window.WindowStartupLocation = 'CenterScreen'
    $window.UseLayoutRounding = $true
    $script:LauncherUi = @{
        Window = $window; Root = $null; Context = $null; Language = $Language; LanguageFixed = $fixed
        Settings = $null; Updating = $false; View = 'play'; ConfigStamp = $null; Shown = $null; Running = $null
        StepStates = @{}; PlayFailed = $false; PlayPlan = $null
    }
    Initialize-LauncherWindowContent
    Set-LauncherView -Name $View -Instant
    return $window
}

# Shows the window and runs until it is closed.
function Show-LauncherWindow {
    $window = New-LauncherWindow
    $ui = $script:LauncherUi
    if (Test-LauncherMotion) { (Get-LauncherUiElement 'Night').Opacity = 1.0 }
    $window.Add_ContentRendered({ Invoke-LauncherUiAction { Start-LauncherWindowMotion } })
    $window.Add_KeyDown({
            param($sender, $e)
            if ($e.Key -eq 'Escape' -and $script:LauncherUi.View -eq 'more') {
                $e.Handled = $true
                Invoke-LauncherUiAction { Set-LauncherView -Name 'play' }
            }
        })
    # The saved file is looked at every second: the MCP server or the command line may change it.
    $poll = New-Object System.Windows.Threading.DispatcherTimer
    $poll.Interval = [TimeSpan]::FromSeconds(1)
    $poll.Add_Tick({ Invoke-LauncherUiAction { Sync-LauncherUiFromDisk } })
    $window.Add_Closing({
            param($sender, $e)
            # Preparing steps are stopped with their process tree; a running game is not touched
            # (the window is hidden while it runs and cannot be closed then).
            if ($null -ne $script:LauncherUi.Running) { Stop-LauncherUiPlay -WaitMs 3000 }
        })
    $frame = New-Object System.Windows.Threading.DispatcherFrame
    $window.Add_Closed({ $poll.Stop(); $frame.Continue = $false }.GetNewClosure())
    $poll.Start()
    $window.Show()
    Set-LauncherViewFocus
    [System.Windows.Threading.Dispatcher]::PushFrame($frame)
}
