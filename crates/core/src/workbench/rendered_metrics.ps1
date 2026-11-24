$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes

$docType = New-Object Windows.Automation.PropertyCondition(
    [Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::Document)
$docName = New-Object Windows.Automation.PropertyCondition(
    [Windows.Automation.AutomationElement]::NameProperty, $env:MPIDER_RENDERED_TITLE)
$matchesArticle = New-Object Windows.Automation.AndCondition($docType, $docName)
$scopes = [Windows.Automation.TreeScope]::Descendants
$documents = @()
foreach ($window in [Windows.Automation.AutomationElement]::RootElement.FindAll(
    [Windows.Automation.TreeScope]::Children, [Windows.Automation.Condition]::TrueCondition)) {
    if ($window.Current.ClassName -ne 'Chrome_WidgetWin_0' -or $window.Current.IsOffscreen) { continue }
    $process = Get-Process -Id $window.Current.ProcessId -ErrorAction SilentlyContinue
    if (!$process -or $process.ProcessName -ne 'WeChatAppEx') { continue }
    $doc = $window.FindFirst($scopes, $matchesArticle)
    if ($doc -and !$doc.Current.IsOffscreen) { $documents += $doc }
}
if ($documents.Count -ne 1) { exit 0 }
$counts = @{}
$fields = @{
    'sns_praise_btn' = 'like_count'
    'sns_share_btn' = 'share_count'
    'sns_love_btn' = 'watching_count'
    'sns_sc_comment_btn' = 'comment_count'
}
foreach ($node in $documents[0].FindAll($scopes, [Windows.Automation.Condition]::TrueCondition)) {
    $info = $node.Current
    $classes = $info.ClassName -split '\s+'
    if ($info.ControlType -eq [Windows.Automation.ControlType]::Button) {
        foreach ($class in $classes) {
            if ($fields.ContainsKey($class) -and $info.Name -match '^[^0-9]+\s*([0-9]+)$') {
                $key = $fields[$class]
                $value = [long]$Matches[1]
                if (!$counts.ContainsKey($key)) { $counts[$key] = $value }
                elseif ($counts[$key] -ne $value) { $counts[$key] = $null }
            }
        }
    }
    if ($classes -contains 'js_media_tool_meta') {
        if ($info.Name -match '^\u9605\u8bfb\s*([0-9.,]+(?:\u4e07|\u4ebf)?\+?)$') {
            $reading = $Matches[1]
            if ($reading -match '^[0-9]+$') { $counts['read_count'] = [long]$reading }
            else { $counts['read_display'] = $reading }
        }
    }
}
[Console]::OutputEncoding = New-Object Text.UTF8Encoding($false)
$counts | ConvertTo-Json -Compress
