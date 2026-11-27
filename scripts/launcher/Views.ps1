# The XAML of the window: the scene behind everything (sky, sun and its glow, sea, reflections,
# the dark fades and the night that fades out when the window opens) and the three views (Play,
# Preparing, More options). Texts come from the language files, sizes and colours from the
# theme. Every control that shows an option is listed in $Context.Controls with its option id,
# so the window can find it, set it and listen to it.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# The texts and the list of option controls one build of the window shares.
function New-LauncherViewContext {
    param([Parameter(Mandatory)][string]$Language)
    return @{ Language = $Language; Controls = (New-Object System.Collections.Generic.List[object]); Next = 0 }
}

function Get-LauncherViewText {
    param([Parameter(Mandatory)]$Context, [Parameter(Mandatory)][string]$Key, [object[]]$Arguments)
    return ConvertTo-LauncherXmlText (Get-LauncherText -Key $Key -Language $Context.Language -Arguments $Arguments)
}

# A name for an option's control, listed with what it shows.
function Add-LauncherViewControl {
    param([Parameter(Mandatory)]$Context, [Parameter(Mandatory)][string]$Kind, [Parameter(Mandatory)][string]$Id, [string]$Value, [string]$View)
    $Context.Next++
    $name = 'Option{0}' -f $Context.Next
    $Context.Controls.Add(@{ Name = $name; Kind = $Kind; Id = $Id; Value = $Value; View = $View })
    return $name
}

# An icon of the theme in its 24 x 24 box, drawn at a layout size.
function Get-LauncherIconXaml {
    param([Parameter(Mandatory)][string]$Icon, [Parameter(Mandatory)][string]$Size, [string]$Stroke, [string]$Fill, [string]$Thickness = 'Icon.stroke', [string]$Name, [string]$Margin = '0')
    $paint = ''
    if ($Fill) { $paint += ' Fill="{StaticResource ' + $Fill + '}"' }
    if ($Stroke) { $paint += ' Stroke="{StaticResource ' + $Stroke + '}" StrokeThickness="{StaticResource ' + $Thickness + '}" StrokeStartLineCap="Round" StrokeEndLineCap="Round" StrokeLineJoin="Round"' }
    $nameAttr = ''
    if ($Name) { $nameAttr = ' x:Name="' + $Name + '"' }
    return ('<Viewbox{0} Width="{{StaticResource Layout.{1}}}" Height="{{StaticResource Layout.{1}}}" VerticalAlignment="Center" Margin="{2}"><Canvas Width="24" Height="24"><Path Data="{{StaticResource Icon.{3}}}"{4}/></Canvas></Viewbox>' -f $nameAttr, $Size, $Margin, $Icon, $paint)
}

function Get-LauncherSpace {
    param([Parameter(Mandatory)][int]$Value)
    $space = @(Get-LauncherToken -Path 'space')
    if ($space -notcontains $Value) { throw "tokens.json has no space $Value" }
    return ConvertTo-LauncherXamlNumber $Value
}

# The sky, the sun and its glow, the sea, the reflections, the dark fades and the night.
function Get-LauncherSceneXaml {
    $w = [double](Get-LauncherToken 'size.windowWidth')
    $h = [double](Get-LauncherToken 'size.windowHeight')
    $sun = Get-LauncherToken 'scene.sun'
    $sunSize = [double]$sun['size']
    $sunLeft = [double]$sun['left']
    $sunTop = $h - [double]$sun['bottom'] - $sunSize
    $glowSize = $sunSize * [double](Get-LauncherToken 'layout.sunGlowScale')
    $glowLeft = $sunLeft + ($sunSize - $glowSize) / 2
    $glowTop = $sunTop + ($sunSize - $glowSize) / 2
    $seaHeight = [double](Get-LauncherToken 'gradient.sea.height')
    $n = { param($v) ConvertTo-LauncherXamlNumber $v }
    $x = New-Object System.Text.StringBuilder
    [void]$x.AppendLine(('<Canvas x:Name="Scene" Width="{0}" Height="{1}" IsHitTestVisible="False">' -f (& $n $w), (& $n $h)))
    [void]$x.AppendLine(('<Rectangle Width="{0}" Height="{1}" Fill="{{StaticResource Brush.sky}}"/>' -f (& $n $w), (& $n $h)))
    [void]$x.AppendLine(('<Ellipse x:Name="SunGlow" Canvas.Left="{0}" Canvas.Top="{1}" Width="{2}" Height="{2}" Fill="{{StaticResource Brush.sunGlow}}"/>' -f (& $n $glowLeft), (& $n $glowTop), (& $n $glowSize)))
    [void]$x.AppendLine(('<Ellipse Canvas.Left="{0}" Canvas.Top="{1}" Width="{2}" Height="{2}" Fill="{{StaticResource Brush.sun}}"/>' -f (& $n $sunLeft), (& $n $sunTop), (& $n $sunSize)))
    [void]$x.AppendLine(('<Rectangle Canvas.Top="{0}" Width="{1}" Height="{2}" Fill="{{StaticResource Brush.sea}}"/>' -f (& $n ($h - $seaHeight)), (& $n $w), (& $n $seaHeight)))
    foreach ($r in Get-LauncherToken 'scene.reflections') {
        $top = $h - [double]$r['bottom'] - [double]$r['height']
        [void]$x.AppendLine(('<Rectangle Canvas.Left="{0}" Canvas.Top="{1}" Width="{2}" Height="{3}" RadiusX="{4}" RadiusY="{4}" Opacity="{5}" Fill="{{StaticResource Brush.reflection}}"/>' -f (& $n $r['left']), (& $n $top), (& $n $r['width']), (& $n $r['height']), (& $n ([double]$r['height'] / 2)), (& $n $r['opacity'])))
    }
    [void]$x.AppendLine(('<Rectangle x:Name="FadeTop" Width="{0}" Height="{1}" Fill="{{StaticResource Brush.fadeTop}}"/>' -f (& $n $w), (& $n $h)))
    [void]$x.AppendLine(('<Rectangle x:Name="FadeSide" Width="{0}" Height="{1}" Fill="{{StaticResource Brush.fadeSide}}"/>' -f (& $n $w), (& $n $h)))
    [void]$x.AppendLine(('<Rectangle x:Name="Night" Width="{0}" Height="{1}" Opacity="0" Fill="{{StaticResource Brush.night}}"/>' -f (& $n $w), (& $n $h)))
    [void]$x.AppendLine('</Canvas>')
    return $x.ToString()
}

# A choice drawn as segmented buttons (the Play view's panel).
function Get-LauncherSegmentsXaml {
    param([Parameter(Mandatory)]$Context, [Parameter(Mandatory)]$Option)
    $id = $Option['id']
    $values = @(Get-LauncherOptionValues -Option $Option)
    $gap = Get-LauncherSpace 8
    $columns = @()
    $cells = @()
    for ($i = 0; $i -lt $values.Count; $i++) {
        if ($i -gt 0) { $columns += ('<ColumnDefinition Width="{0}"/>' -f $gap) }
        $columns += '<ColumnDefinition Width="*"/>'
        $v = $values[$i]
        $name = Add-LauncherViewControl -Context $Context -Kind 'segment' -Id $id -Value $v -View 'play'
        $text = Get-LauncherViewText -Context $Context -Key "option.$id.value.$v"
        $extra = ''
        if (-not (Test-LauncherValueAvailable -Option $Option -Value $v)) {
            $extra = ' IsEnabled="False" ToolTip="' + (Get-LauncherViewText -Context $Context -Key 'common.unavailable') + '"'
        }
        $cells += ('<ToggleButton x:Name="{0}" Grid.Column="{1}" Style="{{StaticResource Toggle.Segment}}" Content="{2}" AutomationProperties.Name="{2}"{3}/>' -f $name, ($i * 2), $text, $extra)
    }
    $caption = Format-LauncherDisplayText -Text (Get-LauncherText -Key "option.$id.label" -Language $Context.Language) -Language $Context.Language
    return @"
<StackPanel Margin="0,0,0,$(Get-LauncherSpace 18)">
  <TextBlock Style="{StaticResource Text.Caption}" Text="$(ConvertTo-LauncherXmlText $caption)" Margin="0,0,0,$gap"/>
  <Grid AutomationProperties.Name="$(Get-LauncherViewText -Context $Context -Key "option.$id.label")">
    <Grid.ColumnDefinitions>$($columns -join '')</Grid.ColumnDefinitions>
    $($cells -join "`n    ")
  </Grid>
</StackPanel>
"@
}

# A toggle drawn as a row with its label, its hint (or the 'not on this version' text) and a switch.
function Get-LauncherSwitchXaml {
    param([Parameter(Mandatory)]$Context, [Parameter(Mandatory)]$Option, [Parameter(Mandatory)][string]$View, [string]$Height = 'Layout.switchRowHeight')
    $id = $Option['id']
    $name = Add-LauncherViewControl -Context $Context -Kind 'switch' -Id $id -View $View
    $label = Get-LauncherViewText -Context $Context -Key "option.$id.label"
    $hint = Get-LauncherViewText -Context $Context -Key "option.$id.hint"
    $extra = ''
    if (-not $Option['available']) {
        $extra = ' IsEnabled="False" ToolTip="' + $hint + '"'
        $hint = Get-LauncherViewText -Context $Context -Key 'common.unavailable'
    }
    return @"
<ToggleButton x:Name="$name" Style="{StaticResource Toggle.Switch}" MinHeight="{StaticResource $Height}" AutomationProperties.Name="$label" AutomationProperties.HelpText="$hint"$extra>
  <StackPanel>
    <TextBlock Style="{StaticResource Text.Label}" Text="$label"/>
    <TextBlock Style="{StaticResource Text.Hint}" Text="$hint"/>
  </StackPanel>
</ToggleButton>
"@
}

# A choice drawn as a row with its label and a drop-down (More options).
function Get-LauncherComboXaml {
    param([Parameter(Mandatory)]$Context, [Parameter(Mandatory)]$Option)
    $id = $Option['id']
    $name = Add-LauncherViewControl -Context $Context -Kind 'combo' -Id $id -View 'more'
    $label = Get-LauncherViewText -Context $Context -Key "option.$id.label"
    $hint = Get-LauncherViewText -Context $Context -Key "option.$id.hint"
    $unavailable = Get-LauncherViewText -Context $Context -Key 'common.unavailable'
    $items = foreach ($v in Get-LauncherOptionValues -Option $Option) {
        $text = Get-LauncherViewText -Context $Context -Key "option.$id.value.$v"
        $extra = ''
        if (-not (Test-LauncherValueAvailable -Option $Option -Value $v)) { $extra = ' IsEnabled="False" ToolTip="' + $unavailable + '"' }
        '<ComboBoxItem Content="{0}" Tag="{1}"{2}/>' -f $text, (ConvertTo-LauncherXmlText $v), $extra
    }
    $enabled = ''
    if (-not $Option['available']) { $enabled = ' IsEnabled="False"'; $hint = $unavailable }
    return @"
<Border Style="{StaticResource Border.Row}">
  <Grid ToolTip="$hint">
    <Grid.ColumnDefinitions><ColumnDefinition Width="*"/><ColumnDefinition Width="Auto"/></Grid.ColumnDefinitions>
    <TextBlock Style="{StaticResource Text.Label}" VerticalAlignment="Center" Margin="0,0,$(Get-LauncherSpace 12),0" Text="$label"/>
    <ComboBox x:Name="$name" Grid.Column="1" VerticalAlignment="Center" Style="{StaticResource Combo.Dark}" AutomationProperties.Name="$label" AutomationProperties.HelpText="$hint"$enabled>
      $($items -join "`n      ")
    </ComboBox>
  </Grid>
</Border>
"@
}

# The Play view: the Play button (or "Choose the executable") with its status on the left, the
# options panel on the right, with "More options", the GitHub link and the version.
function Get-LauncherPlayViewXaml {
    param([Parameter(Mandatory)]$Context)
    $panel = New-Object System.Text.StringBuilder
    $toggles = New-Object System.Collections.Generic.List[string]
    $flush = {
        if ($toggles.Count -gt 0) {
            [void]$panel.AppendLine('<StackPanel Margin="0,0,0,' + (Get-LauncherSpace 18) + '">' + ($toggles -join "`n") + '</StackPanel>')
            $toggles.Clear()
        }
    }
    foreach ($o in Get-LauncherOptions) {
        if (-not ($o.Contains('main') -and $o['main'])) { continue }
        if ($o['kind'] -eq 'toggle') { $toggles.Add((Get-LauncherSwitchXaml -Context $Context -Option $o -View 'play')) }
        else {
            & $flush
            [void]$panel.AppendLine((Get-LauncherSegmentsXaml -Context $Context -Option $o))
        }
    }
    & $flush
    $title = ConvertTo-LauncherXmlText (Format-LauncherDisplayText -Text (Get-LauncherText -Key 'panel.title' -Language $Context.Language) -Language $Context.Language)
    $play = ConvertTo-LauncherXmlText (Format-LauncherDisplayText -Text (Get-LauncherText -Key 'play.button' -Language $Context.Language) -Language $Context.Language)
    $choose = ConvertTo-LauncherXmlText (Format-LauncherDisplayText -Text (Get-LauncherText -Key 'play.choose_game' -Language $Context.Language) -Language $Context.Language)
    $version = Get-LauncherVersion -Language $Context.Language
    $s8 = Get-LauncherSpace 8
    $s14 = Get-LauncherSpace 14
    $s18 = Get-LauncherSpace 18
    $s32 = Get-LauncherSpace 32
    return @"
<Grid x:Name="PlayView" Margin="{StaticResource Thickness.view}">
  <Grid.ColumnDefinitions><ColumnDefinition Width="*"/><ColumnDefinition Width="{StaticResource Column.panel}"/></Grid.ColumnDefinitions>
  <StackPanel VerticalAlignment="Bottom" HorizontalAlignment="Left" Margin="0,0,$s32,0">
    <Button x:Name="PlayButton" Style="{StaticResource Button.Play}" HorizontalAlignment="Left" AutomationProperties.Name="$(Get-LauncherViewText -Context $Context -Key 'play.button.name')">
      <StackPanel Orientation="Horizontal">
        $(Get-LauncherIconXaml -Icon 'play' -Size 'iconPlay' -Fill 'Brush.onAccent' -Margin "0,0,$(Get-LauncherSpace 14),0")
        <TextBlock Style="{StaticResource Text.Display}" FontSize="{StaticResource FontSize.play}" Foreground="{StaticResource Brush.onAccent}" VerticalAlignment="Center" Text="$play"/>
      </StackPanel>
    </Button>
    <Button x:Name="ChooseButton" Style="{StaticResource Button.Play}" HorizontalAlignment="Left" Visibility="Collapsed" AutomationProperties.Name="$(Get-LauncherViewText -Context $Context -Key 'play.choose_game')">
      <TextBlock Style="{StaticResource Text.Display}" FontSize="{StaticResource FontSize.titleSmall}" Foreground="{StaticResource Brush.onAccent}" Text="$choose"/>
    </Button>
    <Grid Margin="0,$s14,0,0" MaxWidth="{StaticResource Layout.statusWidth}" HorizontalAlignment="Left">
      <Grid.ColumnDefinitions><ColumnDefinition Width="Auto"/><ColumnDefinition Width="*"/></Grid.ColumnDefinitions>
      <Ellipse x:Name="StatusDot" Width="{StaticResource Layout.statusDot}" Height="{StaticResource Layout.statusDot}" VerticalAlignment="Top" Margin="0,$(ConvertTo-LauncherXamlNumber (([double](Get-LauncherToken 'type.size.small') * 1.33 - [double](Get-LauncherToken 'layout.statusDot')) / 2)),$(Get-LauncherSpace 12),0" Fill="{StaticResource Brush.gold}"/>
      <TextBox x:Name="StatusText" Grid.Column="1" Style="{StaticResource Text.Selectable}"/>
    </Grid>
    <Button x:Name="ChangeGameButton" Style="{StaticResource Button.Link}" HorizontalAlignment="Left" Margin="0,$s8,0,0" Foreground="{StaticResource Brush.textMuted}" Content="$(Get-LauncherViewText -Context $Context -Key 'play.change_game')"/>
  </StackPanel>
  <Border Grid.Column="1" Style="{StaticResource Border.Panel}" Padding="{StaticResource Thickness.panel}">
    <DockPanel LastChildFill="False">
      <TextBlock DockPanel.Dock="Top" Style="{StaticResource Text.Display}" FontSize="{StaticResource FontSize.titleSmall}" Margin="0,0,0,$s18" Text="$title"/>
      <StackPanel DockPanel.Dock="Top">
        $($panel.ToString())
      </StackPanel>
      <DockPanel DockPanel.Dock="Bottom" MinHeight="{StaticResource Layout.linkRowHeight}">
        <Button x:Name="GithubButton" DockPanel.Dock="Left" Style="{StaticResource Button.Link}" AutomationProperties.Name="$(Get-LauncherViewText -Context $Context -Key 'panel.github.name')" ToolTip="https://github.com/edsuuu/VI-BR-PORT">
          <StackPanel Orientation="Horizontal">
            $(Get-LauncherIconXaml -Icon 'link' -Size 'iconLink' -Stroke 'Brush.textSoft' -Margin "0,0,$s8,0")
            <TextBlock VerticalAlignment="Center" Text="$(Get-LauncherViewText -Context $Context -Key 'panel.github')"/>
          </StackPanel>
        </Button>
        <TextBlock x:Name="VersionText" HorizontalAlignment="Right" VerticalAlignment="Center" FontSize="{StaticResource FontSize.caption}" Foreground="{StaticResource Brush.textMuted}" Text="$(ConvertTo-LauncherXmlText $version['text'])" AutomationProperties.Name="$(Get-LauncherViewText -Context $Context -Key 'panel.version.name' -Arguments @($version['text']))"/>
      </DockPanel>
      <Button x:Name="MoreButton" DockPanel.Dock="Bottom" Style="{StaticResource Button.Outline}" Padding="$s14,0" Margin="0,0,0,$s18" FontSize="{StaticResource FontSize.body}" HorizontalContentAlignment="Stretch">
        <DockPanel>
          $((Get-LauncherIconXaml -Icon 'chevron' -Size 'iconChevron' -Stroke 'Brush.text').Replace('<Viewbox ', '<Viewbox DockPanel.Dock="Right" '))
          <TextBlock VerticalAlignment="Center" Text="$(Get-LauncherViewText -Context $Context -Key 'panel.more')"/>
        </DockPanel>
      </Button>
    </DockPanel>
  </Border>
</Grid>
"@
}

# The Preparing view: the card with the four steps, the progress bar, the message and Cancel.
function Get-LauncherPrepViewXaml {
    param([Parameter(Mandatory)]$Context)
    $rows = foreach ($i in 0..($script:LauncherSteps.Count - 1)) {
        $step = $script:LauncherSteps[$i]
        $ringSize = '{StaticResource Layout.iconStep}'
        @"
<Grid x:Name="Step$i" MinHeight="{StaticResource Layout.stepRowHeight}" Margin="0,0,0,$(Get-LauncherSpace 4)">
  <Grid.ColumnDefinitions><ColumnDefinition Width="Auto"/><ColumnDefinition Width="*"/><ColumnDefinition Width="Auto"/></Grid.ColumnDefinitions>
  <Grid Width="$ringSize" Height="$ringSize" VerticalAlignment="Center" Margin="0,0,$(Get-LauncherSpace 12),0">
    $(Get-LauncherIconXaml -Icon 'check' -Size 'iconStep' -Stroke 'Brush.gold' -Thickness 'Icon.strokeBold' -Name "Step${i}Done")
    $(Get-LauncherIconXaml -Icon 'check' -Size 'iconStep' -Stroke 'Brush.textMuted' -Thickness 'Icon.strokeBold' -Name "Step${i}Skipped")
    $(Get-LauncherIconXaml -Icon 'cross' -Size 'iconStep' -Stroke 'Brush.rose' -Thickness 'Icon.strokeBold' -Name "Step${i}Failed")
    <Ellipse x:Name="Step${i}Running" Stroke="{StaticResource Brush.accent}" StrokeThickness="{StaticResource Icon.ringRunning}"/>
    <Ellipse x:Name="Step${i}Waiting" Stroke="{StaticResource Brush.stepWaiting}" StrokeThickness="{StaticResource Icon.ringWaiting}"/>
  </Grid>
  <TextBlock x:Name="Step${i}Label" Grid.Column="1" VerticalAlignment="Center" Text="$(Get-LauncherViewText -Context $Context -Key "prep.step.$step")"/>
  <TextBlock x:Name="Step${i}State" Grid.Column="2" VerticalAlignment="Center" FontSize="{StaticResource FontSize.small}"/>
</Grid>
"@
    }
    $title = ConvertTo-LauncherXmlText (Format-LauncherDisplayText -Text (Get-LauncherText -Key 'prep.title' -Language $Context.Language) -Language $Context.Language)
    $s8 = Get-LauncherSpace 8
    $s20 = Get-LauncherSpace 20
    return @"
<Grid x:Name="PrepView" Visibility="Collapsed" Margin="{StaticResource Thickness.view}">
  <Border Style="{StaticResource Border.Card}" Width="{StaticResource Layout.cardWidth}" HorizontalAlignment="Center" VerticalAlignment="Center" Padding="{StaticResource Thickness.card}">
    <StackPanel>
      <TextBlock Style="{StaticResource Text.Display}" FontSize="{StaticResource FontSize.titleLarge}" Text="$title"/>
      <TextBlock Style="{StaticResource Text.Soft}" FontSize="{StaticResource FontSize.body}" LineHeight="$(ConvertTo-LauncherXamlNumber ([double](Get-LauncherToken 'type.size.body') * 1.5))" Margin="0,$s8,0,$s20" Text="$(Get-LauncherViewText -Context $Context -Key 'prep.subtitle')"/>
      <StackPanel Margin="0,0,0,$(Get-LauncherSpace 18)">
        $($rows -join "`n")
      </StackPanel>
      <ProgressBar x:Name="PrepProgress" Style="{StaticResource Progress.Prep}" AutomationProperties.Name="$(Get-LauncherViewText -Context $Context -Key 'prep.progress.name')"/>
      <TextBlock x:Name="PrepLog" Style="{StaticResource Text.Hint}" Margin="0,$s8,0,0" Visibility="Collapsed"/>
      <TextBox x:Name="PrepError" Style="{StaticResource Text.Selectable}" Foreground="{StaticResource Brush.text}" Margin="0,$(Get-LauncherSpace 14),0,0" Visibility="Collapsed"/>
      <Grid Margin="0,$s20,0,0">
        <Grid.ColumnDefinitions><ColumnDefinition Width="*"/><ColumnDefinition Width="Auto"/></Grid.ColumnDefinitions>
        <TextBlock x:Name="PrepMessage" Style="{StaticResource Text.Soft}" VerticalAlignment="Center" Margin="0,0,$(Get-LauncherSpace 18),0"/>
        <Button x:Name="PrepCancel" Grid.Column="1" Style="{StaticResource Button.Outline}" Content="$(Get-LauncherViewText -Context $Context -Key 'prep.cancel')"/>
      </Grid>
    </StackPanel>
  </Border>
</Grid>
"@
}

# More options: every option but the profile, by group, in two columns; Restore the defaults and
# Save and go back at the bottom.
function Get-LauncherMoreViewXaml {
    param([Parameter(Mandatory)]$Context)
    $groups = [ordered]@{}
    foreach ($o in Get-LauncherOptions) {
        if ($o['group'] -eq 'profile') { continue }
        if (-not $groups.Contains($o['group'])) { $groups[$o['group']] = New-Object System.Collections.Generic.List[object] }
        $groups[$o['group']].Add($o)
    }
    # Fill the left column first, up to half of the rows.
    $total = 0
    foreach ($g in $groups.Keys) { $total += $groups[$g].Count }
    $columns = @((New-Object System.Text.StringBuilder), (New-Object System.Text.StringBuilder))
    $count = 0
    $sectionGap = Get-LauncherTokenText 'layout.sectionGap'
    foreach ($g in $groups.Keys) {
        $column = 0
        if ($count -ge [Math]::Ceiling($total / 2.0)) { $column = 1 }
        $count += $groups[$g].Count
        $caption = ConvertTo-LauncherXmlText (Format-LauncherDisplayText -Text (Get-LauncherText -Key "group.$g" -Language $Context.Language) -Language $Context.Language)
        $rows = foreach ($o in $groups[$g]) {
            if ($o['kind'] -eq 'toggle') {
                '<Border Style="{StaticResource Border.Row}">' + (Get-LauncherSwitchXaml -Context $Context -Option $o -View 'more' -Height 'Layout.rowHeight') + '</Border>'
            } else {
                Get-LauncherComboXaml -Context $Context -Option $o
            }
        }
        [void]$columns[$column].AppendLine(@"
<StackPanel Margin="0,0,0,$sectionGap">
  <TextBlock Style="{StaticResource Text.Caption}" Margin="0,0,0,$(Get-LauncherSpace 4)" Text="$caption"/>
  $($rows -join "`n")
</StackPanel>
"@)
    }
    $title = ConvertTo-LauncherXmlText (Format-LauncherDisplayText -Text (Get-LauncherText -Key 'more.title' -Language $Context.Language) -Language $Context.Language)
    return @"
<Grid x:Name="MoreView" Visibility="Collapsed" Margin="{StaticResource Thickness.more}">
  <Border Style="{StaticResource Border.Card}" Padding="{StaticResource Thickness.morePadding}">
    <DockPanel>
      <TextBlock DockPanel.Dock="Top" Style="{StaticResource Text.Display}" FontSize="{StaticResource FontSize.title}" Margin="0,0,0,$(Get-LauncherSpace 20)" Text="$title"/>
      <Grid DockPanel.Dock="Bottom" Margin="0,$(Get-LauncherSpace 8),0,0">
        <Button x:Name="MoreReset" Style="{StaticResource Button.Outline}" HorizontalAlignment="Left" Padding="$(Get-LauncherSpace 18),0" Content="$(Get-LauncherViewText -Context $Context -Key 'more.reset')"/>
        <Button x:Name="MoreBack" Style="{StaticResource Button.Accent}" HorizontalAlignment="Right" Content="$(Get-LauncherViewText -Context $Context -Key 'more.back')"/>
      </Grid>
      <Grid>
        <Grid.ColumnDefinitions><ColumnDefinition Width="*"/><ColumnDefinition Width="{StaticResource Column.gap}"/><ColumnDefinition Width="*"/></Grid.ColumnDefinitions>
        <StackPanel Grid.Column="0">$($columns[0].ToString())</StackPanel>
        <StackPanel Grid.Column="2">$($columns[1].ToString())</StackPanel>
      </Grid>
    </DockPanel>
  </Border>
</Grid>
"@
}

# The whole window's content: the theme, the scene and the three views.
function Get-LauncherWindowXaml {
    param([Parameter(Mandatory)]$Context)
    $p = { param($path) Get-LauncherTokenText -Path $path }
    $thickness = @"
<Thickness x:Key="Thickness.view">$(& $p 'layout.viewPadding')</Thickness>
<Thickness x:Key="Thickness.panel">$(& $p 'layout.panelPadding')</Thickness>
<Thickness x:Key="Thickness.card">$(& $p 'layout.cardPadding')</Thickness>
<Thickness x:Key="Thickness.more">$(& $p 'layout.moreMargin')</Thickness>
<Thickness x:Key="Thickness.morePadding">$(& $p 'layout.morePaddingX'),$(& $p 'layout.morePaddingY')</Thickness>
<GridLength x:Key="Column.panel">$(& $p 'size.panelWidth')</GridLength>
<GridLength x:Key="Column.gap">$(& $p 'layout.columnGap')</GridLength>
"@
    return @"
<Grid x:Name="Root" $(Get-LauncherXamlNamespaces) Width="$(& $p 'size.windowWidth')" Height="$(& $p 'size.windowHeight')" ClipToBounds="True" UseLayoutRounding="True" SnapsToDevicePixels="True">
<Grid.Resources>
$(Get-LauncherThemeXaml)
$thickness
</Grid.Resources>
<Grid TextElement.FontFamily="{StaticResource Font.body}" TextElement.FontSize="{StaticResource FontSize.body}" TextElement.Foreground="{StaticResource Brush.text}" Background="{StaticResource Brush.night}">
$(Get-LauncherSceneXaml)
<Grid x:Name="Views" KeyboardNavigation.TabNavigation="Cycle">
$(Get-LauncherPlayViewXaml -Context $Context)
$(Get-LauncherPrepViewXaml -Context $Context)
$(Get-LauncherMoreViewXaml -Context $Context)
</Grid>
</Grid>
</Grid>
"@
}
