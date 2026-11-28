# What scripts/play.ps1 sets, read from the script itself so that the launcher never keeps a
# second copy of its settings: the hashtable it starts from, then the branches of its switch
# parameters (-Safe, -RayTracing) taken as asked. The script is parsed, never run. A change of
# play.ps1 this reader does not understand stops it with an error instead of a wrong answer.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-PlayScriptPath {
    return (Join-Path $script:LauncherRoot 'scripts\play.ps1')
}

function Test-PlayScriptTouchesSettings {
    param($Ast)
    $found = $Ast.Find({
            param($a)
            ($a -is [System.Management.Automation.Language.VariableExpressionAst]) -and ($a.VariablePath.UserPath -eq 'settings')
        }, $true)
    return ($null -ne $found)
}

function Get-PlayScriptConstant {
    param($Ast, [string]$What)
    $expr = $Ast
    if ($expr -is [System.Management.Automation.Language.PipelineAst]) { $expr = $expr.GetPureExpression() }
    if ($expr -is [System.Management.Automation.Language.CommandExpressionAst]) { $expr = $expr.Expression }
    if ($expr -isnot [System.Management.Automation.Language.ConstantExpressionAst]) {
        throw "scripts/play.ps1: $What is not a constant ($($Ast.Extent.Text))"
    }
    return [string]$expr.SafeGetValue()
}

function Invoke-PlayScriptStatements {
    param($Statements, [hashtable]$Switches, [System.Collections.IDictionary]$Settings)
    foreach ($s in $Statements) {
        if ($s -is [System.Management.Automation.Language.AssignmentStatementAst]) {
            $left = $s.Left
            if (($left -is [System.Management.Automation.Language.VariableExpressionAst]) -and ($left.VariablePath.UserPath -eq 'settings')) {
                $table = $s.Right.Expression
                if ($table -isnot [System.Management.Automation.Language.HashtableAst]) { throw 'scripts/play.ps1: $settings is not set from a hashtable' }
                $Settings.Clear()
                foreach ($pair in $table.KeyValuePairs) {
                    $Settings[(Get-PlayScriptConstant -Ast $pair.Item1 -What 'a key')] = Get-PlayScriptConstant -Ast $pair.Item2 -What 'a value'
                }
            } elseif (($left -is [System.Management.Automation.Language.MemberExpressionAst]) -and
                ($left.Expression -is [System.Management.Automation.Language.VariableExpressionAst]) -and
                ($left.Expression.VariablePath.UserPath -eq 'settings')) {
                $Settings[(Get-PlayScriptConstant -Ast $left.Member -What 'a switch name')] = Get-PlayScriptConstant -Ast $s.Right -What 'a switch value'
            } elseif (Test-PlayScriptTouchesSettings -Ast $left) {
                throw "scripts/play.ps1: an assignment this reader does not know ($($s.Extent.Text))"
            }
            continue
        }
        if ($s -is [System.Management.Automation.Language.IfStatementAst]) {
            $taken = $false
            foreach ($clause in $s.Clauses) {
                $condition = $clause.Item1.GetPureExpression()
                if (($condition -isnot [System.Management.Automation.Language.VariableExpressionAst]) -or
                    (-not $Switches.ContainsKey($condition.VariablePath.UserPath))) {
                    if (Test-PlayScriptTouchesSettings -Ast $clause.Item2) {
                        throw "scripts/play.ps1: a condition this reader does not know ($($clause.Item1.Extent.Text))"
                    }
                    continue
                }
                if ($Switches[$condition.VariablePath.UserPath]) {
                    Invoke-PlayScriptStatements -Statements $clause.Item2.Statements -Switches $Switches -Settings $Settings
                    $taken = $true
                    break
                }
            }
            if (-not $taken -and $null -ne $s.ElseClause) {
                Invoke-PlayScriptStatements -Statements $s.ElseClause.Statements -Switches $Switches -Settings $Settings
            }
        }
        # Anything else (the -More loop, starting the title) sets nothing of its own.
    }
}

# Every switch scripts/play.ps1 sets for these parameters, before its -More.
function Get-PlayScriptSettings {
    param([switch]$RayTracing, [switch]$Safe)
    $tokens = $null
    $errors = $null
    $ast = [System.Management.Automation.Language.Parser]::ParseFile((Get-PlayScriptPath), [ref]$tokens, [ref]$errors)
    if ($errors.Count -gt 0) { throw "scripts/play.ps1 does not parse: $($errors[0].Message)" }
    $settings = [ordered]@{}
    $switches = @{ RayTracing = [bool]$RayTracing; Safe = [bool]$Safe }
    Invoke-PlayScriptStatements -Statements $ast.EndBlock.Statements -Switches $switches -Settings $settings
    if ($settings.Count -eq 0) { throw 'scripts/play.ps1: no $settings found' }
    return $settings
}
