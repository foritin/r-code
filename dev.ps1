[CmdletBinding()]
param(
    [switch]$BootstrapOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = $PSScriptRoot
$frontendDir = Join-Path $repoRoot "src-tauri\frontend"
$agentCoreManifest = Join-Path $repoRoot "vendor\agent-contracts\Cargo.toml"
$scriptExitCode = 0
$locationPushed = $false

function Write-Step {
    param([Parameter(Mandatory = $true)][string]$Message)
    Write-Host "[R-Code] $Message" -ForegroundColor Cyan
}

function Assert-Command {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][string]$InstallHint
    )

    if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
        throw "未找到命令 '$Name'。$InstallHint"
    }
}

function Get-ParentLockedAgentContractsCommit {
    # 优先读暂存区（索引）：`vendor/agent-core` 到 `vendor/agent-contracts` 的重命名
    # 处于已 staged、未 commit 状态时，HEAD 还看不到新路径，只有索引里有 gitlink。
    $indexEntry = ((& git ls-files -s -- "vendor/agent-contracts") | Out-String).Trim()
    if ($LASTEXITCODE -eq 0 -and $indexEntry) {
        $indexFields = $indexEntry -split '\s+'
        if ($indexFields.Count -ge 4) {
            return $indexFields[1]
        }
    }

    # 其次读 HEAD 的新路径，兼容已经提交重命名的父仓库。
    $treeEntry = ((& git ls-tree HEAD -- "vendor/agent-contracts") | Out-String).Trim()
    if ($LASTEXITCODE -eq 0 -and $treeEntry) {
        $treeFields = $treeEntry -split '\s+'
        if ($treeFields.Count -ge 3) {
            return $treeFields[2]
        }
    }

    # 最后回退到旧路径，兼容尚未重命名的历史提交。
    $legacyEntry = ((& git ls-tree HEAD -- "vendor/agent-core") | Out-String).Trim()
    if ($LASTEXITCODE -eq 0 -and $legacyEntry) {
        $legacyFields = $legacyEntry -split '\s+'
        if ($legacyFields.Count -ge 3) {
            return $legacyFields[2]
        }
    }

    return $null
}

function Sync-AgentContractsSubmodule {
    $expectedCommit = Get-ParentLockedAgentContractsCommit
    if (-not $expectedCommit) {
        throw "无法读取父仓库锁定的 agent-contracts 提交。"
    }

    if (-not (Test-Path -LiteralPath $agentCoreManifest)) {
        Write-Step "初始化 agent-contracts 子模块"
        & git submodule update --init --recursive --checkout -- "vendor/agent-contracts"
        if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $agentCoreManifest)) {
            throw "agent-contracts 子模块初始化失败。"
        }
    }

    $actualCommit = ((& git -C "vendor/agent-contracts" rev-parse HEAD) | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or -not $actualCommit) {
        throw "无法读取 agent-contracts 当前提交。"
    }

    if ($actualCommit -ne $expectedCommit) {
        $localChanges = @(& git -C "vendor/agent-contracts" status --porcelain --untracked-files=all)
        if ($LASTEXITCODE -ne 0) {
            throw "无法检查 agent-contracts 本地改动。"
        }
        if ($localChanges.Count -gt 0) {
            throw "agent-contracts 位于 $actualCommit，但父仓库锁定 $expectedCommit，且子模块含本地改动。请先提交或暂存这些改动，再运行 'git submodule update --init --recursive --checkout -- vendor/agent-contracts'。"
        }

        Write-Step "同步 agent-contracts 到父仓库锁定版本"
        & git submodule update --init --recursive --checkout -- "vendor/agent-contracts"
        if ($LASTEXITCODE -ne 0) {
            throw "agent-contracts 子模块同步失败。"
        }
        $actualCommit = ((& git -C "vendor/agent-contracts" rev-parse HEAD) | Out-String).Trim()
        if ($LASTEXITCODE -ne 0 -or $actualCommit -ne $expectedCommit) {
            throw "agent-contracts 同步后仍与父仓库锁定提交不一致。"
        }
    }

    $remainingChanges = @(& git -C "vendor/agent-contracts" status --porcelain --untracked-files=all)
    if ($LASTEXITCODE -ne 0) {
        throw "无法检查 agent-contracts 本地改动。"
    }
    if ($remainingChanges.Count -gt 0) {
        Write-Host "[R-Code] agent-contracts 含本地改动；将按当前工作树继续。" -ForegroundColor Yellow
    }
}

try {
    Push-Location $repoRoot
    $locationPushed = $true

    Write-Step "检查基础开发工具"
    Assert-Command "git" "请先安装 Git for Windows，并重新打开终端。"
    Assert-Command "cargo" "请先通过 rustup 安装 Rust MSVC 工具链，并重新打开终端。"
    Assert-Command "rustc" "请先通过 rustup 安装 Rust MSVC 工具链，并重新打开终端。"
    Assert-Command "node" "请先安装 Node.js，并重新打开终端。"
    Assert-Command "npm" "请先安装包含 npm 的 Node.js，并重新打开终端。"

    $tauriVersion = ""
    if (Get-Command "cargo-tauri" -ErrorAction SilentlyContinue) {
        $tauriVersion = ((& cargo tauri --version 2>$null) | Out-String).Trim()
    }

    if ($tauriVersion -notmatch '^tauri-cli 2\.') {
        Write-Step "安装 Tauri 2 CLI（首次安装可能需要几分钟）"
        $installArgs = @("install", "tauri-cli", "--version", "^2.0.0", "--locked")
        if ($tauriVersion) {
            $installArgs += "--force"
        }
        & cargo @installArgs
        if ($LASTEXITCODE -ne 0) {
            throw "Tauri 2 CLI 安装失败。"
        }

        $tauriVersion = ((& cargo tauri --version 2>$null) | Out-String).Trim()
        if ($tauriVersion -notmatch '^tauri-cli 2\.') {
            throw "Tauri CLI 已安装，但当前终端仍无法找到 2.x 版本。请重新打开终端后再试。"
        }
    }
    Write-Host "[R-Code] $tauriVersion" -ForegroundColor DarkGray

    Write-Step "检查 agent-contracts 子模块版本"
    Sync-AgentContractsSubmodule

    Write-Step "检查前端依赖"
    $viteCommand = Join-Path $frontendDir "node_modules\.bin\vite.cmd"
    $rootLock = Join-Path $frontendDir "package-lock.json"
    $installedLock = Join-Path $frontendDir "node_modules\.package-lock.json"
    $npmReady = Test-Path -LiteralPath $viteCommand

    if ($npmReady -and (Test-Path -LiteralPath $rootLock)) {
        $npmReady = (Test-Path -LiteralPath $installedLock) -and
            ((Get-Item -LiteralPath $installedLock).LastWriteTimeUtc -ge
                (Get-Item -LiteralPath $rootLock).LastWriteTimeUtc)
    }

    if ($npmReady) {
        Push-Location $frontendDir
        try {
            & npm ls --depth=0 --silent *> $null
            $npmReady = $LASTEXITCODE -eq 0
        }
        finally {
            Pop-Location
        }
    }

    if (-not $npmReady) {
        Write-Step "安装前端依赖"
        Push-Location $frontendDir
        try {
            if (Test-Path -LiteralPath $rootLock) {
                & npm ci
            }
            else {
                & npm install
            }
            if ($LASTEXITCODE -ne 0) {
                throw "前端依赖安装失败。"
            }
        }
        finally {
            Pop-Location
        }
    }

    Write-Step "验证 Cargo workspace"
    & cargo metadata --no-deps --format-version 1 *> $null
    if ($LASTEXITCODE -ne 0) {
        throw "Cargo workspace 验证失败。"
    }

    Write-Step "验证本地 TCP 回环"
    Push-Location $frontendDir
    try {
        & node scripts/check-loopback.mjs
        if ($LASTEXITCODE -ne 0) {
            throw "本地 TCP 回环不可用，请根据上方提示调整 VPN/TUN 设置。"
        }
    }
    finally {
        Pop-Location
    }

    if ($BootstrapOnly) {
        Write-Host "[R-Code] 依赖已就绪。" -ForegroundColor Green
    }
    else {
        Write-Step "启动隔离的 R-Code Dev"
        & cargo tauri dev --config "src-tauri/tauri.dev.conf.json"
        $scriptExitCode = $LASTEXITCODE
    }
}
catch {
    Write-Host "[R-Code] $($_.Exception.Message)" -ForegroundColor Red
    $scriptExitCode = 1
}
finally {
    if ($locationPushed) {
        Pop-Location
    }
}

exit $scriptExitCode
