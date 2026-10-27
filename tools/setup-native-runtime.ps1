# 为已有 Windows Python 自动准备隔离采集依赖，不修改全局 Python 包。
param([string]$PythonPath = 'runtime\python.exe')
$ErrorActionPreference = 'Stop'
if ($env:OS -ne 'Windows_NT') { throw '请在 Windows PowerShell 中运行' }
$resolvedPython = (Get-Command $PythonPath -ErrorAction Stop).Source
$runtimeDirectory = Split-Path $resolvedPython -Parent
$dependencyDirectory = Join-Path $runtimeDirectory 'session-deps'
$pipArchive = Join-Path $runtimeDirectory 'pip.pyz'
$requirements = Join-Path $PSScriptRoot '..\native\requirements.txt'
if (-not (Test-Path $pipArchive)) {
    Invoke-WebRequest -Uri 'https://bootstrap.pypa.io/pip/pip.pyz' -OutFile $pipArchive -UseBasicParsing
}
& $resolvedPython $pipArchive install --only-binary=:all: --upgrade --target $dependencyDirectory -r $requirements
if ($LASTEXITCODE -ne 0) { throw '依赖安装失败，请检查网络或 Python 版本（建议 Python 3.12 x64）' }
& $resolvedPython -c "import sys;sys.path.insert(0,sys.argv[1]);import frida;from cryptography.hazmat.primitives.ciphers.aead import AESGCM;print('采集环境已就绪')" $dependencyDirectory
if ($LASTEXITCODE -ne 0) { throw '采集依赖自检失败' }
Write-Host "完成。应用中的 Python 路径填写：$resolvedPython"
