param(
    [string]$PythonExe = "python",
    [switch]$Clean
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Assert-LastExitCode {
    param(
        [string]$StepName
    )

    if ($LASTEXITCODE -ne 0) {
        throw "$StepName 失敗，exit code: $LASTEXITCODE"
    }
}

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RepoRoot = Split-Path -Parent $ScriptDir
$SourceDir = Join-Path $RepoRoot "runtime\discord-bot-src"
$BuildRoot = Join-Path $RepoRoot "runtime\.build\discord-bot"
$VenvDir = Join-Path $BuildRoot ".venv"
$VenvPython = Join-Path $VenvDir "Scripts\python.exe"
$VenvConfig = Join-Path $VenvDir "pyvenv.cfg"
$WorkDir = Join-Path $BuildRoot "work"
$SpecDir = Join-Path $BuildRoot "spec"
$DistRoot = Join-Path $BuildRoot "dist"
$AppDistDir = Join-Path $DistRoot "xiaokui_bot"
$BundleDir = Join-Path $RepoRoot "src-tauri\resources\discord-bot"
$EntryScript = Join-Path $SourceDir "bot_xiaokui.py"
$RequirementsFile = Join-Path $SourceDir "requirements.txt"
$ConfigFile = Join-Path $SourceDir "config_xiaokui.json"
$EnvExampleFile = Join-Path $SourceDir ".env.xiaokui.example"

if (-not (Test-Path $EntryScript)) {
    throw "找不到 bot_xiaokui.py：$EntryScript"
}

if ($Clean) {
    Remove-Item $BuildRoot -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item $BundleDir -Recurse -Force -ErrorAction SilentlyContinue
}

New-Item -ItemType Directory -Force $BuildRoot | Out-Null
New-Item -ItemType Directory -Force $WorkDir | Out-Null
New-Item -ItemType Directory -Force $SpecDir | Out-Null
New-Item -ItemType Directory -Force $DistRoot | Out-Null

if ((Test-Path $VenvDir) -and (-not ((Test-Path $VenvPython) -and (Test-Path $VenvConfig)))) {
    Write-Host "偵測到損壞的 Python venv，重新建立中：$VenvDir" -ForegroundColor Yellow
    Remove-Item $VenvDir -Recurse -Force -ErrorAction SilentlyContinue
}

if (-not ((Test-Path $VenvPython) -and (Test-Path $VenvConfig))) {
    & $PythonExe -m venv $VenvDir
    Assert-LastExitCode "建立 Python venv"
}

& $VenvPython -m pip install --upgrade pip
Assert-LastExitCode "更新 pip"
& $VenvPython -m pip install -r $RequirementsFile pyinstaller
Assert-LastExitCode "安裝 Discord Bot 相依套件"

& $VenvPython -m PyInstaller `
    --noconfirm `
    --clean `
    --onedir `
    --name xiaokui_bot `
    --distpath $DistRoot `
    --workpath $WorkDir `
    --specpath $SpecDir `
    $EntryScript
Assert-LastExitCode "執行 PyInstaller"

if (-not (Test-Path $AppDistDir)) {
    throw "PyInstaller 輸出不存在：$AppDistDir"
}

Remove-Item $BundleDir -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $BundleDir | Out-Null

Copy-Item (Join-Path $AppDistDir "*") $BundleDir -Recurse -Force
Copy-Item $ConfigFile (Join-Path $BundleDir "config_xiaokui.json") -Force
Copy-Item $EnvExampleFile (Join-Path $BundleDir ".env.xiaokui.example") -Force

Write-Host "Discord Bot runtime 已輸出到：$BundleDir"
