# Play from the window: Game.ps1's steps run on a background runspace so the window stays alive;
# their progress comes back through a queue that a dispatcher timer empties into the Preparing
# view. The window hides while the game runs and comes back when it exits. A failed step leaves
# the Preparing view with its message (and the command to run) and a Back button.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# What the background runspace runs: the core's Invoke-LauncherPlay, reporting into the queue.
$script:LauncherPlayScript = {
    param($LauncherFile, $Settings, $Sync, $Language)
    Set-StrictMode -Version Latest
    $ErrorActionPreference = 'Stop'
    $queue = $Sync['Queue']
    try {
        . $LauncherFile
        $report = { param($r) $queue.Enqueue($r) }.GetNewClosure()
        $result = Invoke-LauncherPlay -Settings $Settings -Progress $report -State $Sync -Language $Language
        $queue.Enqueue(@{ final = 'finished'; result = [string]$result })
    } catch {
        $queue.Enqueue(@{ final = 'failed'; message = $_.Exception.Message })
    }
}

function Get-LauncherUiBrush {
    param([Parameter(Mandatory)][string]$Name)
    return $script:LauncherUi.Root.FindResource("Brush.$Name")
}

function Get-LauncherUiElement {
    param([Parameter(Mandatory)][string]$Name)
    $element = $script:LauncherUi.Root.FindName($Name)
    if ($null -eq $element) { throw "the window has no element '$Name'" }
    return $element
}

# One row of the Preparing view in one state: waiting, running, done, skipped, failed, cancelled.
function Set-LauncherStepRow {
    param([Parameter(Mandatory)][int]$Index, [Parameter(Mandatory)][string]$State)
    $ui = $script:LauncherUi
    $icon = @{ waiting = 'Waiting'; running = 'Running'; done = 'Done'; skipped = 'Skipped'; failed = 'Failed'; cancelled = 'Waiting' }[$State]
    foreach ($part in 'Waiting', 'Running', 'Done', 'Skipped', 'Failed') {
        $visibility = 'Collapsed'
        if ($part -eq $icon) { $visibility = 'Visible' }
        (Get-LauncherUiElement "Step$Index$part").Visibility = $visibility
    }
    $label = Get-LauncherUiElement "Step${Index}Label"
    $stateText = Get-LauncherUiElement "Step${Index}State"
    $stateText.Text = Get-LauncherText -Key "prep.state.$State" -Language $ui.Language
    $label.FontWeight = 'Normal'
    $stateText.FontWeight = 'Normal'
    switch ($State) {
        'running' {
            $label.Foreground = Get-LauncherUiBrush 'text'; $label.FontWeight = 'SemiBold'
            $stateText.Foreground = Get-LauncherUiBrush 'text'; $stateText.FontWeight = 'SemiBold'
        }
        'failed' {
            $label.Foreground = Get-LauncherUiBrush 'text'; $label.FontWeight = 'SemiBold'
            $stateText.Foreground = Get-LauncherUiBrush 'text'; $stateText.FontWeight = 'SemiBold'
        }
        'waiting' { $label.Foreground = Get-LauncherUiBrush 'textMuted'; $stateText.Foreground = Get-LauncherUiBrush 'textMuted' }
        'cancelled' { $label.Foreground = Get-LauncherUiBrush 'textMuted'; $stateText.Foreground = Get-LauncherUiBrush 'textMuted' }
        default { $label.Foreground = Get-LauncherUiBrush 'textSoft'; $stateText.Foreground = Get-LauncherUiBrush 'textMuted' }
    }
}

# The Preparing view as Play starts: every step waiting, no progress, no error.
function Reset-LauncherPrepView {
    $ui = $script:LauncherUi
    $ui.StepStates = @{}
    for ($i = 0; $i -lt $script:LauncherSteps.Count; $i++) { Set-LauncherStepRow -Index $i -State 'waiting' }
    (Get-LauncherUiElement 'PrepProgress').Value = 0
    (Get-LauncherUiElement 'PrepLog').Visibility = 'Collapsed'
    (Get-LauncherUiElement 'PrepError').Visibility = 'Collapsed'
    (Get-LauncherUiElement 'PrepMessage').Text = Get-LauncherText -Key 'prep.opens_by_itself' -Language $ui.Language
    (Get-LauncherUiElement 'PrepCancel').Content = Get-LauncherText -Key 'prep.cancel' -Language $ui.Language
}

# One progress record of Invoke-LauncherPlay drawn into the Preparing view.
function Update-LauncherPrepView {
    param([Parameter(Mandatory)]$Report)
    $ui = $script:LauncherUi
    $index = [int]$Report['index'] - 1
    $state = [string]$Report['state']
    $step = [string]$Report['step']
    $firstReport = -not $ui.StepStates.ContainsKey($step)
    $ui.StepStates[$step] = $state
    Set-LauncherStepRow -Index $index -State $state
    (Get-LauncherUiElement 'PrepProgress').Value = [double]$Report['percent']
    $log = Get-LauncherUiElement 'PrepLog'
    switch ($state) {
        'running' {
            $doing = Get-LauncherText -Key "prep.step.$step.doing" -Language $ui.Language
            (Get-LauncherUiElement 'PrepMessage').Text = Get-LauncherText -Key 'prep.step_of' -Language $ui.Language -Arguments @($Report['index'], $Report['count'], $doing)
            # The first report of a step carries why it runs; the next ones carry its log's last line.
            if (-not $firstReport -and $Report['message']) {
                $log.Text = [string]$Report['message']
                $log.Visibility = 'Visible'
            } else {
                $log.Visibility = 'Collapsed'
            }
        }
        'failed' {
            # The step's command line goes under the message, to be run by hand and its output read.
            $message = [string]$Report['message']
            if ($null -ne $ui.PlayPlan) {
                foreach ($s in $ui.PlayPlan['steps']) {
                    if ($s['id'] -eq $step -and $s['command']) { $message += [Environment]::NewLine + $s['command'] }
                }
            }
            Show-LauncherPrepError -Message $message
        }
        default { $log.Visibility = 'Collapsed' }
    }
}

function Show-LauncherPrepError {
    param([Parameter(Mandatory)][AllowEmptyString()][string]$Message)
    $ui = $script:LauncherUi
    $ui.PlayFailed = $true
    $box = Get-LauncherUiElement 'PrepError'
    $box.Text = $Message
    $box.Visibility = 'Visible'
    (Get-LauncherUiElement 'PrepLog').Visibility = 'Collapsed'
    (Get-LauncherUiElement 'PrepMessage').Text = ''
    (Get-LauncherUiElement 'PrepCancel').Content = Get-LauncherText -Key 'prep.back' -Language $ui.Language
}

# Play: the Preparing view, then the steps on a background runspace.
function Start-LauncherUiPlay {
    $ui = $script:LauncherUi
    if ($null -ne $ui.Running) { return }
    $settings = Get-LauncherSettings
    $ui.PlayPlan = Get-LauncherPlayPlan -Settings $settings -Language $ui.Language
    $ui.PlayFailed = $false
    Reset-LauncherPrepView
    Set-LauncherView -Name 'preparing'
    $sync = [hashtable]::Synchronized(@{ Cancel = $false; Queue = (New-Object 'System.Collections.Concurrent.ConcurrentQueue[object]') })
    $runspace = [System.Management.Automation.Runspaces.RunspaceFactory]::CreateRunspace()
    $runspace.Open()
    $shell = [System.Management.Automation.PowerShell]::Create()
    $shell.Runspace = $runspace
    [void]$shell.AddScript($script:LauncherPlayScript.ToString())
    [void]$shell.AddArgument((Join-Path $script:LauncherDir 'Launcher.ps1'))
    [void]$shell.AddArgument($settings)
    [void]$shell.AddArgument($sync)
    [void]$shell.AddArgument($ui.Language)
    $handle = $shell.BeginInvoke()
    $timer = New-Object System.Windows.Threading.DispatcherTimer
    $timer.Interval = [TimeSpan]::FromMilliseconds(100)
    $ui.Running = @{ Shell = $shell; Runspace = $runspace; Handle = $handle; Sync = $sync; Timer = $timer; Hidden = $false }
    $timer.Add_Tick({ Invoke-LauncherUiAction { Receive-LauncherPlayProgress } })
    $timer.Start()
}

# Empties the queue of the running Play into the window.
function Receive-LauncherPlayProgress {
    $ui = $script:LauncherUi
    $running = $ui.Running
    if ($null -eq $running) { return }
    $item = $null
    while ($running.Sync['Queue'].TryDequeue([ref]$item)) {
        if ($item.Contains('final')) {
            Complete-LauncherUiPlay -Final $item
            return
        }
        Update-LauncherPrepView -Report $item
        if ($item['step'] -eq 'open' -and $item['state'] -eq 'running' -and -not $running.Hidden) {
            $running.Hidden = $true
            $ui.Window.Hide()
        }
        if ($item['step'] -eq 'open' -and $item['state'] -ne 'running' -and $running.Hidden) {
            Show-LauncherWindowAgain
        }
    }
}

function Show-LauncherWindowAgain {
    $ui = $script:LauncherUi
    if ($null -ne $ui.Running) { $ui.Running.Hidden = $false }
    $ui.Window.Show()
    [void]$ui.Window.Activate()
}

# The end of Play: back to the Play view, or the error left on the Preparing view.
function Complete-LauncherUiPlay {
    param([Parameter(Mandatory)]$Final)
    $ui = $script:LauncherUi
    $running = $ui.Running
    $running.Timer.Stop()
    if ($running.Hidden) { Show-LauncherWindowAgain }
    try { [void]$running.Shell.EndInvoke($running.Handle) } catch { }
    $running.Shell.Dispose()
    $running.Runspace.Dispose()
    $ui.Running = $null
    if ($Final['final'] -eq 'failed') {
        if (-not $ui.PlayFailed) { Show-LauncherPrepError -Message ([string]$Final['message']) }
        return
    }
    Update-LauncherUi -Settings (Get-LauncherSettings)
    Set-LauncherView -Name 'play'
}

# Cancel while preparing; Back after a failure.
function Stop-LauncherUiPlay {
    param([int]$WaitMs = 0)
    $ui = $script:LauncherUi
    if ($null -eq $ui.Running) {
        Set-LauncherView -Name 'play'
        return
    }
    $ui.Running.Sync['Cancel'] = $true
    if ($WaitMs -gt 0) { [void]$ui.Running.Handle.AsyncWaitHandle.WaitOne($WaitMs) }
}
