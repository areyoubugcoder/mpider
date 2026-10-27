# 为 Cargo 测试程序嵌入 Common Controls v6 清单，解决 Tauri 单元测试的 Windows 加载问题。
param(
    [Parameter(Mandatory=$true, Position=0)][string]$Executable,
    [Parameter(Position=1, ValueFromRemainingArguments=$true)][string[]]$TestArguments
)
$ErrorActionPreference='Stop'
$executablePath=(Resolve-Path -LiteralPath $Executable).Path
$manifest=Join-Path (Split-Path $PSScriptRoot -Parent) 'src-tauri\windows-test.manifest'
$kits=(Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows Kits\Installed Roots').KitsRoot10
$mt=Get-ChildItem -Path (Join-Path $kits 'bin\*\x64\mt.exe') | Sort-Object FullName -Descending | Select-Object -First 1
if(-not $mt){throw '缺少 Windows SDK mt.exe，无法为测试程序嵌入启动清单'}
& $mt.FullName -nologo -manifest $manifest "-outputresource:$executablePath;#1"
if($LASTEXITCODE -ne 0){throw '测试程序启动清单嵌入失败'}
& $executablePath @TestArguments
exit $LASTEXITCODE
