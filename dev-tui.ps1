[CmdletBinding()]
param(
    [switch]$Print,
    [string]$Message = "",
    [switch]$Json,
    [switch]$SkipBuild
)

# R-Code TUI（r-code-tui）开发启动脚本。
#
# 与 dev.ps1 的区别：TUI 是独立 Cargo 二进制（ratatui + crossterm），不启动
# WebView/Tauri，因此不需要前端 node_modules、Tauri CLI、本地回环检查。
# 只需 Rust 工具链 + agent-contracts 子模块就位。
#
# 默认启动交互终端（--mode tui）；`-Print` / `-Json` 走非交互单轮（脚本/管道用）。
# 默认 data-dir 指向 dev GUI 的同一 AppData（com.r-code.app.dev\r-code），
# 让 TUI 复用 dev.ps1 里配好的 provider 与密钥。

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = $PSScriptRoot
$agentCoreManifest = Join-Path $repoRoot "vendor\agent-contracts\Cargo.toml"
$scriptExitCode = 0
$locationPushed = $false

function Write-Step {
    param([Parameter(Mandatory = $true)][string]$Message)
    Write-Host "[R-Code TUI] $Message" -ForegroundColor Cyan
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
    # 与 dev.ps1 同一解析：优先暂存区、其次 HEAD、最后回退旧路径。
    $indexEntry = ((& git ls-files -s -- "vendor/agent-contracts") | Out-String).Trim()
    if ($LASTEXITCODE -eq 0 -and $indexEntry) {
        $indexFields = $indexEntry -split '\s+'
        if ($indexFields.Count -ge 4) { return $indexFields[1] }
    }
    $treeEntry = ((& git ls-tree HEAD -- "vendor/agent-contracts") | Out-String).Trim()
    if ($LASTEXITCODE -eq 0 -and $treeEntry) {
        $treeFields = $treeEntry -split '\s+'
        if ($treeFields.Count -ge 3) { return $treeFields[2] }
    }
    $legacyEntry = ((& git ls-tree HEAD -- "vendor/agent-core") | Out-String).Trim()
    if ($LASTEXITCODE -eq 0 -and $legacyEntry) {
        $legacyFields = $legacyEntry -split '\s+'
        if ($legacyFields.Count -ge 3) { return $legacyFields[2] }
    }
    return $null
}

function Sync-AgentContractsSubmodule {
    $expectedCommit = Get-ParentLockedAgentContractsCommit
    if (-not $expectedCommit) { throw "无法读取父仓库锁定的 agent-contracts 提交。" }

    if (-not (Test-Path -LiteralPath $agentCoreManifest)) {
        Write-Step "初始化 agent-contracts 子模块"
        & git submodule update --init --recursive --checkout -- "vendor/agent-contracts"
        if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $agentCoreManifest)) {
            throw "agent-contracts 子模块初始化失败。"
        }
    }

    $actualCommit = ((& git -C "vendor/agent-contracts" rev-parse HEAD) | Out-String).Trim()
    if (-not $actualCommit) { throw "无法读取 agent-contracts 当前提交。" }

    if ($actualCommit -ne $expectedCommit) {
        $localChanges = @(& git -C "vendor/agent-contracts" status --porcelain --untracked-files=all)
        if ($localChanges.Count -gt 0) {
            throw "agent-contracts 位于 $actualCommit，但父仓库锁定 $expectedCommit，且子模块含本地改动。请先提交或暂存，再运行 'git submodule update --init --recursive --checkout -- vendor/agent-contracts'。"
        }
        Write-Step "同步 agent-contracts 到父仓库锁定版本"
        & git submodule update --init --recursive --checkout -- "vendor/agent-contracts"
        if ($LASTEXITCODE -ne 0) { throw "agent-contracts 子模块同步失败。" }
    }
}

try {
    Push-Location $repoRoot
    $locationPushed = $true

    # r-code-tui 输出 UTF-8（Windows 下已自设代码页 65001）。让 PowerShell
    # 也按 UTF-8 解码子进程 stdout，否则 -Print 模式下中文经重定向会乱码。
    [Console]::OutputEncoding = [System.Text.Encoding]::UTF8
    $OutputEncoding = [System.Text.Encoding]::UTF8

    Write-Step "检查基础开发工具"
    Assert-Command "git" "请先安装 Git for Windows，并重新打开终端。"
    Assert-Command "cargo" "请先通过 rustup 安装 Rust MSVC 工具链，并重新打开终端。"
    Assert-Command "rustc" "请先通过 rustup 安装 Rust MSVC 工具链，并重新打开终端。"

    Write-Step "检查 agent-contracts 子模块版本"
    Sync-AgentContractsSubmodule

    if (-not $SkipBuild) {
        Write-Step "编译 r-code-tui"
        & cargo build -p r-code-tui --bin r-code-tui
        if ($LASTEXITCODE -ne 0) { throw "r-code-tui 编译失败。" }
    }

    # dev GUI 的同一 AppData（Roaming），让 TUI 复用 dev 里配好的 provider。
    $devDataDir = Join-Path $env:APPDATA "com.r-code.app.dev\r-code"

    $tuiArgs = @()
    if ($Print) {
        $tuiArgs += "--mode", "print"
        if (-not $Message) { throw "-Print 需要配合 -Message '<文本>'。" }
        $tuiArgs += "--message", $Message
    }
    elseif ($Json) {
        $tuiArgs += "--mode", "json"
        if (-not $Message) { throw "-Json 需要配合 -Message '<文本>'。" }
        $tuiArgs += "--message", $Message
    }
    else {
        $tuiArgs += "--mode", "tui"
    }
    $tuiArgs += "--data-dir", $devDataDir

    Write-Step "启动 r-code-tui"
    & cargo run -q -p r-code-tui --bin r-code-tui -- @tuiArgs
    $scriptExitCode = $LASTEXITCODE}
catch {
    Write-Host "[R-Code TUI] $($_.Exception.Message)" -ForegroundColor Red
    $scriptExitCode = 1
}
finally {
    if ($locationPushed) { Pop-Location }
}

exit $scriptExitCode
