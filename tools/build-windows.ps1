# 在 Windows 本机构建桌面程序，可附带固定 Python/Frida 生成完整便携发行包。
param(
    [switch]$Release,
    [switch]$Portable,
    [string]$PythonPath = 'python',
    [string]$SourceRevision = 'source-archive'
)
$ErrorActionPreference = 'Stop'
if ($env:OS -ne 'Windows_NT') { throw '请在 Windows PowerShell 中运行本脚本' }
if ($Portable -and -not $Release) { throw '便携发行包须同时指定 -Release' }
Set-Location (Split-Path $PSScriptRoot -Parent)
foreach ($tool in @('pnpm', 'cargo')) {
    if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) {
        throw "缺少 $tool，请先按 docs/windows-standalone.md 安装构建环境并重新打开终端"
    }
}
if ($Portable -and -not (Get-Command $PythonPath -ErrorAction SilentlyContinue)) {
    throw '组装便携包需要构建机 Python 3.9+；可通过 -PythonPath 指定现有 Python'
}
& pnpm install --frozen-lockfile
if ($LASTEXITCODE -ne 0) { throw '前端依赖安装失败' }
if ($Release) {
    & pnpm tauri build --no-bundle
    $buildProfile = 'release'
} else {
    # 专机内存较小时减少调试信息占用，仍使用真实 Windows 本机编译。
    $env:CARGO_PROFILE_DEV_DEBUG = '0'
    & pnpm tauri build --debug --no-bundle
    $buildProfile = 'debug'
}
if ($LASTEXITCODE -ne 0) { throw 'Windows 桌面程序构建失败' }
$targetDirectory = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { 'target' }
$executable = Join-Path $targetDirectory "$buildProfile\mpider-tauri.exe"
$destination = Join-Path (Get-Location) 'dist-windows'
New-Item -ItemType Directory -Force $destination | Out-Null
if ($Portable) {
    & $PythonPath tools/package-windows-release.py --exe $executable --output $destination --revision $SourceRevision
    if ($LASTEXITCODE -ne 0) { throw '便携包组装失败' }
    $version = (Get-Content package.json -Raw -Encoding UTF8 | ConvertFrom-Json).version
    $stage = Join-Path $destination "MPider-v$version-windows-x64"
    & $PythonPath tools/verify-windows-release.py --directory $stage
    if ($LASTEXITCODE -ne 0) { throw '便携包离线自检失败' }
} else {
    Copy-Item $executable (Join-Path $destination 'MPider.exe') -Force
    Write-Host "构建完成：$destination\MPider.exe"
    Write-Host '这是开发用单文件；运行需另备 Python/Frida。完整发行包请加 -Release -Portable。'
}
