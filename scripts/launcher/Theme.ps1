# The window's theme, made from tokens.json only: brushes, sizes, fonts, icons, text styles and
# the control templates (segmented buttons, switches, drop-downs, outline and gradient buttons,
# the progress bar, the gold focus rectangle). Get-LauncherThemeXaml returns the entries of a
# WPF resource dictionary as XAML text; the window puts them in its root's resources, so its
# own XAML refers to them as {StaticResource ...}. No colour or size is written in the window.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$script:LauncherTokensCache = $null

function Get-LauncherTokens {
    if ($null -eq $script:LauncherTokensCache) {
        $script:LauncherTokensCache = Read-LauncherJson -Path (Join-Path $script:LauncherDir 'tokens.json')
    }
    return $script:LauncherTokensCache
}

# A token by its dotted path, for example 'layout.rowHeight' or 'motion.viewFadeMs'.
function Get-LauncherToken {
    param([Parameter(Mandatory)][string]$Path)
    $node = Get-LauncherTokens
    foreach ($part in $Path.Split('.')) {
        if ($node -isnot [System.Collections.IDictionary] -or -not $node.Contains($part)) { throw "tokens.json has no '$Path'" }
        $node = $node[$part]
    }
    return $node
}

# A number as XAML writes it, whatever the culture of this Windows.
function ConvertTo-LauncherXamlNumber {
    param([Parameter(Mandatory)]$Value)
    return ([double]$Value).ToString('0.####', [System.Globalization.CultureInfo]::InvariantCulture)
}

# A token number as XAML text.
function Get-LauncherTokenText {
    param([Parameter(Mandatory)][string]$Path)
    return ConvertTo-LauncherXamlNumber (Get-LauncherToken -Path $Path)
}

# '#RRGGBB', or { colour, opacity }, as WPF's '#AARRGGBB'.
function ConvertTo-LauncherColourText {
    param([Parameter(Mandatory)]$Value)
    $opacity = 1.0
    if ($Value -is [System.Collections.IDictionary]) {
        $opacity = [double]$Value['opacity']
        $Value = $Value['colour']
    }
    $hex = ([string]$Value).TrimStart('#')
    if ($hex -notmatch '^[0-9A-Fa-f]{6}$') { throw "tokens.json: '$Value' is not a #RRGGBB colour" }
    $alpha = [int][Math]::Round([Math]::Max(0.0, [Math]::Min(1.0, $opacity)) * 255)
    return '#' + $alpha.ToString('X2') + $hex.ToUpperInvariant()
}

# A font token as WPF takes it: a family list, a stretch and a weight. A family named
# '<name> Condensed' (Bahnschrift Condensed) is asked for as '<name>' with the condensed stretch:
# WPF does not see the condensed face under its own name.
function Get-LauncherThemeFont {
    param([Parameter(Mandatory)][string]$Which)
    $font = Get-LauncherToken -Path "type.$Which"
    $stretch = 'Normal'
    $families = @()
    foreach ($name in ([string]$font['family']).Split(',')) {
        $name = $name.Trim()
        if ($name -like '* Condensed') {
            $name = $name.Substring(0, $name.Length - ' Condensed'.Length)
            $stretch = 'Condensed'
        }
        if ($name) { $families += $name }
    }
    $weight = 'Normal'
    if ($font.Contains('weight')) { $weight = [string]$font['weight'] }
    return @{ Family = ($families -join ', '); Stretch = $stretch; Weight = $weight }
}

# Upper case for the display and caption styles (WPF has no text-transform).
function Format-LauncherDisplayText {
    param([AllowEmptyString()][string]$Text, [string]$Language)
    $culture = [System.Globalization.CultureInfo]::InvariantCulture
    if ($Language) { try { $culture = [System.Globalization.CultureInfo]::GetCultureInfo($Language) } catch { } }
    return $Text.ToUpper($culture)
}

function ConvertTo-LauncherXmlText {
    param([AllowEmptyString()][AllowNull()][string]$Text)
    if ($null -eq $Text) { return '' }
    return [System.Security.SecurityElement]::Escape($Text)
}

function Get-LauncherXamlNamespaces {
    return 'xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation" xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml" xmlns:sys="clr-namespace:System;assembly=mscorlib"'
}

function Get-LauncherGradientXaml {
    param([Parameter(Mandatory)][string]$Key, [Parameter(Mandatory)]$Gradient)
    $end = '0,1'
    if ($Gradient['direction'] -eq 'horizontal') { $end = '1,0' }
    $stops = foreach ($s in $Gradient['stops']) {
        '<GradientStop Color="{0}" Offset="{1}"/>' -f (ConvertTo-LauncherColourText $s['colour']), (ConvertTo-LauncherXamlNumber $s['offset'])
    }
    return ('<LinearGradientBrush x:Key="{0}" StartPoint="0,0" EndPoint="{1}">{2}</LinearGradientBrush>' -f $Key, $end, ($stops -join ''))
}

# The icons, in a 24 x 24 box as the design draws them.
function Get-LauncherIconData {
    return [ordered]@{
        'Icon.play'    = 'M7,4.5 V19.5 A1,1 0 0 0 8.5,20.36 L21,12.86 A1,1 0 0 0 21,11.14 L8.5,3.64 A1,1 0 0 0 7,4.5 Z'
        'Icon.check'   = 'M5,12 L10,17 L19,7'
        'Icon.cross'   = 'M7,7 L17,17 M17,7 L7,17'
        'Icon.chevron' = 'M9,5 L16,12 L9,19'
        'Icon.link'    = 'M14,5 H19 V10 M19,5 L11,13 M11,6 H6 A1,1 0 0 0 5,7 V18 A1,1 0 0 0 6,19 H17 A1,1 0 0 0 18,18 V13'
        'Icon.down'    = 'M6,9 L12,15 L18,9'
    }
}

# Every resource entry of the theme, as XAML text (no enclosing element).
function Get-LauncherThemeXaml {
    $t = Get-LauncherTokens
    $x =New-Object System.Text.StringBuilder
    $add = { param($line) [void]$x.AppendLine($line) }

    foreach ($name in $t['colour'].Keys) {
        $c = ConvertTo-LauncherColourText $t['colour'][$name]
        & $add ('<Color x:Key="Color.{0}">{1}</Color>' -f $name, $c)
        & $add ('<SolidColorBrush x:Key="Brush.{0}" Color="{1}"/>' -f $name, $c)
    }
    foreach ($name in $t['gradient'].Keys) { & $add (Get-LauncherGradientXaml -Key "Brush.$name" -Gradient $t['gradient'][$name]) }
    # The sun's glow: its middle colour fading out around it.
    $glow = @{ colour = $t['colour']['sunMid']; opacity = (Get-LauncherToken 'layout.sunGlowOpacity') }
    $clear = @{ colour = $t['colour']['sunMid']; opacity = 0 }
    & $add ('<RadialGradientBrush x:Key="Brush.sunGlow"><GradientStop Color="{0}" Offset="0.3"/><GradientStop Color="{1}" Offset="1"/></RadialGradientBrush>' -f (ConvertTo-LauncherColourText $glow), (ConvertTo-LauncherColourText $clear))

    foreach ($group in 'size', 'layout') {
        foreach ($name in $t[$group].Keys) {
            if ($name -eq 'comment') { continue }
            & $add ('<sys:Double x:Key="{0}.{1}">{2}</sys:Double>' -f ($group.Substring(0, 1).ToUpperInvariant() + $group.Substring(1)), $name, (ConvertTo-LauncherXamlNumber $t[$group][$name]))
        }
    }
    foreach ($name in $t['type']['size'].Keys) {
        & $add ('<sys:Double x:Key="FontSize.{0}">{1}</sys:Double>' -f $name, (ConvertTo-LauncherXamlNumber $t['type']['size'][$name]))
    }
    foreach ($name in $t['radius'].Keys) {
        & $add ('<CornerRadius x:Key="Radius.{0}">{1}</CornerRadius>' -f $name, (ConvertTo-LauncherXamlNumber $t['radius'][$name]))
    }
    $display = Get-LauncherThemeFont -Which 'display'
    $body = Get-LauncherThemeFont -Which 'body'
    $caption = Get-LauncherToken 'type.caption'
    & $add ('<FontFamily x:Key="Font.display">{0}</FontFamily>' -f (ConvertTo-LauncherXmlText $display.Family))
    & $add ('<FontFamily x:Key="Font.body">{0}</FontFamily>' -f (ConvertTo-LauncherXmlText $body.Family))
    foreach ($icon in (Get-LauncherIconData).GetEnumerator()) {
        & $add ('<Geometry x:Key="{0}">{1}</Geometry>' -f $icon.Key, $icon.Value)
    }
    # Stroke widths of the icons in their 24 x 24 box, and of the step rings, as the design draws them.
    & $add '<sys:Double x:Key="Icon.stroke">2.4</sys:Double>'
    & $add '<sys:Double x:Key="Icon.strokeBold">3</sys:Double>'
    & $add '<sys:Double x:Key="Icon.ringRunning">3</sys:Double>'
    & $add '<sys:Double x:Key="Icon.ringWaiting">2</sys:Double>'

    $switchW = [double](Get-LauncherToken 'size.switchWidth')
    $switchH = [double](Get-LauncherToken 'size.switchHeight')
    $knob = [double](Get-LauncherToken 'size.knob')
    $knobInset = ConvertTo-LauncherXamlNumber (($switchH - $knob) / 2)
    $trackRadius = ConvertTo-LauncherXamlNumber ($switchH / 2)
    $focusOut = ConvertTo-LauncherXamlNumber (-([double](Get-LauncherToken 'size.focusOffset') + [double](Get-LauncherToken 'size.focusThickness')))
    $focusRadius = ConvertTo-LauncherXamlNumber ([double](Get-LauncherToken 'radius.control') + [double](Get-LauncherToken 'size.focusOffset'))
    $space = @($t['space'] | ForEach-Object { ConvertTo-LauncherXamlNumber $_ })
    # space: 4, 8, 12, 14, 18, 20, 22, 32
    $s4 = $space[0]; $s8 = $space[1]; $s12 = $space[2]; $s14 = $space[3]; $s18 = $space[4]; $s22 = $space[6]

    & $add @"
<Style x:Key="FocusVisual">
  <Setter Property="Control.Template"><Setter.Value><ControlTemplate>
    <Rectangle Margin="$focusOut" Stroke="{StaticResource Brush.focus}" StrokeThickness="{StaticResource Size.focusThickness}" RadiusX="$focusRadius" RadiusY="$focusRadius" SnapsToDevicePixels="True"/>
  </ControlTemplate></Setter.Value></Setter>
</Style>

<Style x:Key="Text.Display" TargetType="TextBlock">
  <Setter Property="FontFamily" Value="{StaticResource Font.display}"/>
  <Setter Property="FontStretch" Value="$($display.Stretch)"/>
  <Setter Property="FontWeight" Value="$($display.Weight)"/>
  <Setter Property="Foreground" Value="{StaticResource Brush.text}"/>
  <Setter Property="LineHeight" Value="{Binding FontSize, RelativeSource={RelativeSource Self}}"/>
  <Setter Property="LineStackingStrategy" Value="BlockLineHeight"/>
</Style>
<Style x:Key="Text.Caption" TargetType="TextBlock">
  <Setter Property="FontSize" Value="{StaticResource FontSize.caption}"/>
  <Setter Property="FontWeight" Value="$($caption['weight'])"/>
  <Setter Property="Foreground" Value="{StaticResource Brush.textMuted}"/>
</Style>
<Style x:Key="Text.Label" TargetType="TextBlock">
  <Setter Property="FontSize" Value="{StaticResource FontSize.body}"/>
  <Setter Property="FontWeight" Value="SemiBold"/>
  <Setter Property="TextTrimming" Value="CharacterEllipsis"/>
</Style>
<Style x:Key="Text.Hint" TargetType="TextBlock">
  <Setter Property="FontSize" Value="{StaticResource FontSize.caption}"/>
  <Setter Property="Foreground" Value="{StaticResource Brush.textMuted}"/>
  <Setter Property="TextTrimming" Value="CharacterEllipsis"/>
  <Setter Property="Margin" Value="0,2,0,0"/>
</Style>
<Style x:Key="Text.Soft" TargetType="TextBlock">
  <Setter Property="FontSize" Value="{StaticResource FontSize.small}"/>
  <Setter Property="Foreground" Value="{StaticResource Brush.textSoft}"/>
  <Setter Property="TextWrapping" Value="Wrap"/>
</Style>
<Style x:Key="Text.Selectable" TargetType="TextBox">
  <Setter Property="IsReadOnly" Value="True"/>
  <Setter Property="IsReadOnlyCaretVisible" Value="False"/>
  <Setter Property="TextWrapping" Value="Wrap"/>
  <Setter Property="Background" Value="Transparent"/>
  <Setter Property="BorderThickness" Value="0"/>
  <Setter Property="Padding" Value="0"/>
  <Setter Property="FontSize" Value="{StaticResource FontSize.small}"/>
  <Setter Property="Foreground" Value="{StaticResource Brush.textSoft}"/>
  <Setter Property="SelectionBrush" Value="{StaticResource Brush.accent}"/>
  <Setter Property="FocusVisualStyle" Value="{StaticResource FocusVisual}"/>
  <Setter Property="Template"><Setter.Value><ControlTemplate TargetType="TextBox">
    <ScrollViewer x:Name="PART_ContentHost" Focusable="False" HorizontalScrollBarVisibility="Hidden" VerticalScrollBarVisibility="Hidden"/>
  </ControlTemplate></Setter.Value></Setter>
</Style>

<Style TargetType="ToolTip">
  <Setter Property="Background" Value="{StaticResource Brush.night2}"/>
  <Setter Property="Foreground" Value="{StaticResource Brush.text}"/>
  <Setter Property="FontFamily" Value="{StaticResource Font.body}"/>
  <Setter Property="FontSize" Value="{StaticResource FontSize.caption}"/>
  <Setter Property="Template"><Setter.Value><ControlTemplate TargetType="ToolTip">
    <Border Background="{TemplateBinding Background}" BorderBrush="{StaticResource Brush.strokeStrong}" BorderThickness="1" CornerRadius="{StaticResource Radius.control}" Padding="$s12,$s8">
      <ContentPresenter/>
    </Border>
  </ControlTemplate></Setter.Value></Setter>
</Style>

<Style x:Key="Button.Base" TargetType="ButtonBase">
  <Setter Property="FocusVisualStyle" Value="{StaticResource FocusVisual}"/>
  <Setter Property="Foreground" Value="{StaticResource Brush.text}"/>
  <Setter Property="Cursor" Value="Hand"/>
  <Setter Property="ToolTipService.ShowOnDisabled" Value="True"/>
  <Style.Triggers>
    <Trigger Property="IsEnabled" Value="False">
      <Setter Property="Opacity" Value="{StaticResource Layout.disabledOpacity}"/>
      <Setter Property="Cursor" Value="Arrow"/>
    </Trigger>
  </Style.Triggers>
</Style>

<Style x:Key="Button.Play" TargetType="Button" BasedOn="{StaticResource Button.Base}">
  <Setter Property="Width" Value="{StaticResource Size.playWidth}"/>
  <Setter Property="Height" Value="{StaticResource Size.playHeight}"/>
  <Setter Property="Foreground" Value="{StaticResource Brush.onAccent}"/>
  <Setter Property="Template"><Setter.Value><ControlTemplate TargetType="Button">
    <Border x:Name="Face" Background="{StaticResource Brush.play}" CornerRadius="{StaticResource Radius.play}" RenderTransformOrigin="0.5,0.5">
      <Border.Effect><DropShadowEffect Color="{StaticResource Color.accent}" Opacity="$(Get-LauncherTokenText 'glow.play.opacity')" BlurRadius="$(Get-LauncherTokenText 'glow.play.blur')" ShadowDepth="$(Get-LauncherTokenText 'glow.play.offsetY')" Direction="270"/></Border.Effect>
      <ContentPresenter HorizontalAlignment="Center" VerticalAlignment="Center"/>
    </Border>
    <ControlTemplate.Triggers>
      <Trigger Property="IsMouseOver" Value="True"><Setter TargetName="Face" Property="Opacity" Value="0.94"/></Trigger>
      <Trigger Property="IsPressed" Value="True">
        <Setter TargetName="Face" Property="RenderTransform"><Setter.Value><ScaleTransform ScaleX="0.98" ScaleY="0.98"/></Setter.Value></Setter>
      </Trigger>
    </ControlTemplate.Triggers>
  </ControlTemplate></Setter.Value></Setter>
</Style>

<Style x:Key="Button.Outline" TargetType="Button" BasedOn="{StaticResource Button.Base}">
  <Setter Property="Height" Value="{StaticResource Size.controlHeight}"/>
  <Setter Property="Padding" Value="$s18,0"/>
  <Setter Property="FontSize" Value="{StaticResource FontSize.small}"/>
  <Setter Property="FontWeight" Value="SemiBold"/>
  <Setter Property="HorizontalContentAlignment" Value="Center"/>
  <Setter Property="BorderBrush" Value="{StaticResource Brush.strokeStrong}"/>
  <Setter Property="Template"><Setter.Value><ControlTemplate TargetType="Button">
    <Border x:Name="Face" Background="Transparent" BorderBrush="{TemplateBinding BorderBrush}" BorderThickness="1" CornerRadius="{StaticResource Radius.control}" Padding="{TemplateBinding Padding}">
      <ContentPresenter HorizontalAlignment="{TemplateBinding HorizontalContentAlignment}" VerticalAlignment="Center"/>
    </Border>
    <ControlTemplate.Triggers>
      <Trigger Property="IsMouseOver" Value="True"><Setter TargetName="Face" Property="BorderBrush" Value="{StaticResource Brush.textMuted}"/></Trigger>
      <Trigger Property="IsPressed" Value="True"><Setter TargetName="Face" Property="Background" Value="{StaticResource Brush.rowLine}"/></Trigger>
    </ControlTemplate.Triggers>
  </ControlTemplate></Setter.Value></Setter>
</Style>

<Style x:Key="Button.Accent" TargetType="Button" BasedOn="{StaticResource Button.Base}">
  <Setter Property="Height" Value="{StaticResource Size.controlHeight}"/>
  <Setter Property="Padding" Value="$s22,0"/>
  <Setter Property="FontSize" Value="{StaticResource FontSize.body}"/>
  <Setter Property="FontWeight" Value="SemiBold"/>
  <Setter Property="Foreground" Value="{StaticResource Brush.onAccent}"/>
  <Setter Property="Template"><Setter.Value><ControlTemplate TargetType="Button">
    <Border x:Name="Face" Background="{StaticResource Brush.play}" CornerRadius="{StaticResource Radius.control}" Padding="{TemplateBinding Padding}">
      <ContentPresenter HorizontalAlignment="Center" VerticalAlignment="Center"/>
    </Border>
    <ControlTemplate.Triggers>
      <Trigger Property="IsMouseOver" Value="True"><Setter TargetName="Face" Property="Opacity" Value="0.94"/></Trigger>
    </ControlTemplate.Triggers>
  </ControlTemplate></Setter.Value></Setter>
</Style>

<Style x:Key="Button.Link" TargetType="Button" BasedOn="{StaticResource Button.Base}">
  <Setter Property="MinHeight" Value="{StaticResource Layout.linkRowHeight}"/>
  <Setter Property="FontSize" Value="{StaticResource FontSize.caption}"/>
  <Setter Property="FontWeight" Value="SemiBold"/>
  <Setter Property="Foreground" Value="{StaticResource Brush.textSoft}"/>
  <Setter Property="Template"><Setter.Value><ControlTemplate TargetType="Button">
    <Border Background="Transparent"><ContentPresenter VerticalAlignment="Center"/></Border>
  </ControlTemplate></Setter.Value></Setter>
  <Style.Triggers>
    <Trigger Property="IsMouseOver" Value="True"><Setter Property="Foreground" Value="{StaticResource Brush.sunTop}"/></Trigger>
  </Style.Triggers>
</Style>

<Style x:Key="Toggle.Segment" TargetType="ToggleButton" BasedOn="{StaticResource Button.Base}">
  <Setter Property="Height" Value="{StaticResource Size.controlHeight}"/>
  <Setter Property="FontSize" Value="{StaticResource FontSize.body}"/>
  <Setter Property="FontWeight" Value="SemiBold"/>
  <Setter Property="Background" Value="Transparent"/>
  <Setter Property="BorderBrush" Value="{StaticResource Brush.strokeStrong}"/>
  <Setter Property="Template"><Setter.Value><ControlTemplate TargetType="ToggleButton">
    <Border Background="{TemplateBinding Background}" BorderBrush="{TemplateBinding BorderBrush}" BorderThickness="1" CornerRadius="{StaticResource Radius.control}">
      <ContentPresenter HorizontalAlignment="Center" VerticalAlignment="Center"/>
    </Border>
  </ControlTemplate></Setter.Value></Setter>
  <Style.Triggers>
    <MultiTrigger>
      <MultiTrigger.Conditions><Condition Property="IsMouseOver" Value="True"/><Condition Property="IsChecked" Value="False"/></MultiTrigger.Conditions>
      <Setter Property="BorderBrush" Value="{StaticResource Brush.textMuted}"/>
    </MultiTrigger>
    <Trigger Property="IsChecked" Value="True">
      <Setter Property="Background" Value="{StaticResource Brush.text}"/>
      <Setter Property="BorderBrush" Value="{StaticResource Brush.text}"/>
      <Setter Property="Foreground" Value="{StaticResource Brush.onAccent}"/>
    </Trigger>
  </Style.Triggers>
</Style>

<Style x:Key="Toggle.Switch" TargetType="ToggleButton" BasedOn="{StaticResource Button.Base}">
  <Setter Property="HorizontalContentAlignment" Value="Stretch"/>
  <Setter Property="Template"><Setter.Value><ControlTemplate TargetType="ToggleButton">
    <Grid Background="Transparent">
      <Grid.ColumnDefinitions><ColumnDefinition Width="*"/><ColumnDefinition Width="Auto"/></Grid.ColumnDefinitions>
      <ContentPresenter VerticalAlignment="Center" Margin="0,0,$s12,0"/>
      <Border x:Name="Track" Grid.Column="1" VerticalAlignment="Center" Width="$(ConvertTo-LauncherXamlNumber $switchW)" Height="$(ConvertTo-LauncherXamlNumber $switchH)" CornerRadius="$trackRadius" Background="{StaticResource Brush.switchOff}">
        <Ellipse x:Name="Knob" Width="{StaticResource Size.knob}" Height="{StaticResource Size.knob}" HorizontalAlignment="Left" VerticalAlignment="Center" Margin="$knobInset,0,$knobInset,0" Fill="{StaticResource Brush.switchKnobOff}"/>
      </Border>
    </Grid>
    <ControlTemplate.Triggers>
      <Trigger Property="IsChecked" Value="True">
        <Setter TargetName="Track" Property="Background" Value="{StaticResource Brush.accent}"/>
        <Setter TargetName="Knob" Property="Fill" Value="{StaticResource Brush.text}"/>
        <Setter TargetName="Knob" Property="HorizontalAlignment" Value="Right"/>
      </Trigger>
      <MultiTrigger>
        <MultiTrigger.Conditions><Condition Property="IsMouseOver" Value="True"/><Condition Property="IsChecked" Value="False"/></MultiTrigger.Conditions>
        <Setter TargetName="Knob" Property="Fill" Value="{StaticResource Brush.text}"/>
      </MultiTrigger>
    </ControlTemplate.Triggers>
  </ControlTemplate></Setter.Value></Setter>
</Style>

<Style x:Key="ComboItem.Dark" TargetType="ComboBoxItem">
  <Setter Property="Foreground" Value="{StaticResource Brush.text}"/>
  <Setter Property="FontSize" Value="{StaticResource FontSize.small}"/>
  <Setter Property="ToolTipService.ShowOnDisabled" Value="True"/>
  <Setter Property="Template"><Setter.Value><ControlTemplate TargetType="ComboBoxItem">
    <Border x:Name="Row" Background="Transparent" Padding="$s12,$s8" CornerRadius="$s4">
      <ContentPresenter/>
    </Border>
    <ControlTemplate.Triggers>
      <Trigger Property="IsHighlighted" Value="True"><Setter TargetName="Row" Property="Background" Value="{StaticResource Brush.violet}"/></Trigger>
      <Trigger Property="IsSelected" Value="True"><Setter TargetName="Row" Property="Background" Value="{StaticResource Brush.switchOff}"/></Trigger>
      <Trigger Property="IsEnabled" Value="False"><Setter Property="Opacity" Value="{StaticResource Layout.disabledOpacity}"/></Trigger>
    </ControlTemplate.Triggers>
  </ControlTemplate></Setter.Value></Setter>
</Style>

<Style x:Key="Combo.Dark" TargetType="ComboBox">
  <Setter Property="Width" Value="{StaticResource Layout.selectWidth}"/>
  <Setter Property="Height" Value="{StaticResource Layout.selectHeight}"/>
  <Setter Property="FontSize" Value="{StaticResource FontSize.small}"/>
  <Setter Property="Foreground" Value="{StaticResource Brush.text}"/>
  <Setter Property="FocusVisualStyle" Value="{StaticResource FocusVisual}"/>
  <Setter Property="ItemContainerStyle" Value="{StaticResource ComboItem.Dark}"/>
  <Setter Property="ToolTipService.ShowOnDisabled" Value="True"/>
  <Setter Property="Cursor" Value="Hand"/>
  <Setter Property="Template"><Setter.Value><ControlTemplate TargetType="ComboBox">
    <Grid>
      <ToggleButton Focusable="False" ClickMode="Press" IsChecked="{Binding IsDropDownOpen, Mode=TwoWay, RelativeSource={RelativeSource TemplatedParent}}">
        <ToggleButton.Template><ControlTemplate TargetType="ToggleButton">
          <Border x:Name="Field" Background="{StaticResource Brush.field}" BorderBrush="{StaticResource Brush.strokeStrong}" BorderThickness="1" CornerRadius="{StaticResource Radius.control}">
            <Viewbox Width="{StaticResource Layout.iconChevron}" Height="{StaticResource Layout.iconChevron}" HorizontalAlignment="Right" Margin="0,0,$s12,0">
              <Canvas Width="24" Height="24"><Path Data="{StaticResource Icon.down}" Stroke="{StaticResource Brush.textSoft}" StrokeThickness="{StaticResource Icon.stroke}" StrokeStartLineCap="Round" StrokeEndLineCap="Round" StrokeLineJoin="Round"/></Canvas>
            </Viewbox>
          </Border>
          <ControlTemplate.Triggers>
            <Trigger Property="IsMouseOver" Value="True"><Setter TargetName="Field" Property="BorderBrush" Value="{StaticResource Brush.textMuted}"/></Trigger>
          </ControlTemplate.Triggers>
        </ControlTemplate></ToggleButton.Template>
      </ToggleButton>
      <ContentPresenter IsHitTestVisible="False" Margin="$s12,0,$(ConvertTo-LauncherXamlNumber ([double]$s12 + [double](Get-LauncherToken 'layout.iconChevron') + [double]$s8)),0" VerticalAlignment="Center" HorizontalAlignment="Left" Content="{TemplateBinding SelectionBoxItem}" ContentTemplate="{TemplateBinding SelectionBoxItemTemplate}"/>
      <Popup IsOpen="{TemplateBinding IsDropDownOpen}" Placement="Bottom" AllowsTransparency="True" Focusable="False" PopupAnimation="None">
        <Border MinWidth="{TemplateBinding ActualWidth}" Margin="0,$s4,0,0" Padding="$s4" Background="{StaticResource Brush.field}" BorderBrush="{StaticResource Brush.strokeStrong}" BorderThickness="1" CornerRadius="{StaticResource Radius.control}">
          <ScrollViewer VerticalScrollBarVisibility="Auto"><ItemsPresenter KeyboardNavigation.DirectionalNavigation="Contained"/></ScrollViewer>
        </Border>
      </Popup>
    </Grid>
    <ControlTemplate.Triggers>
      <Trigger Property="IsEnabled" Value="False"><Setter Property="Opacity" Value="{StaticResource Layout.disabledOpacity}"/></Trigger>
    </ControlTemplate.Triggers>
  </ControlTemplate></Setter.Value></Setter>
</Style>

<Style x:Key="Progress.Prep" TargetType="ProgressBar">
  <Setter Property="Height" Value="{StaticResource Layout.progressHeight}"/>
  <Setter Property="Minimum" Value="0"/>
  <Setter Property="Maximum" Value="100"/>
  <Setter Property="Template"><Setter.Value><ControlTemplate TargetType="ProgressBar">
    <Grid>
      <Border x:Name="PART_Track" CornerRadius="$(ConvertTo-LauncherXamlNumber ([double](Get-LauncherToken 'layout.progressHeight') / 2))" Background="{StaticResource Brush.switchOff}"/>
      <Border x:Name="PART_Indicator" HorizontalAlignment="Left" CornerRadius="$(ConvertTo-LauncherXamlNumber ([double](Get-LauncherToken 'layout.progressHeight') / 2))" Background="{StaticResource Brush.accent}"/>
    </Grid>
  </ControlTemplate></Setter.Value></Setter>
</Style>

<Style x:Key="Border.Panel" TargetType="Border">
  <Setter Property="Background" Value="{StaticResource Brush.panel}"/>
  <Setter Property="BorderBrush" Value="{StaticResource Brush.stroke}"/>
  <Setter Property="BorderThickness" Value="1"/>
  <Setter Property="CornerRadius" Value="{StaticResource Radius.panel}"/>
</Style>
<Style x:Key="Border.Card" TargetType="Border">
  <Setter Property="Background" Value="{StaticResource Brush.panelStrong}"/>
  <Setter Property="BorderBrush" Value="{StaticResource Brush.stroke}"/>
  <Setter Property="BorderThickness" Value="1"/>
  <Setter Property="CornerRadius" Value="{StaticResource Radius.card}"/>
</Style>
<Style x:Key="Border.Row" TargetType="Border">
  <Setter Property="MinHeight" Value="{StaticResource Layout.rowHeight}"/>
  <Setter Property="BorderBrush" Value="{StaticResource Brush.rowLine}"/>
  <Setter Property="BorderThickness" Value="0,0,0,1"/>
</Style>
"@
    return $x.ToString()
}
