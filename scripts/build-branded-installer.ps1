[CmdletBinding()]
param(
    [string]$Target = "",
    [string]$InnerInstaller = "",
    [string]$OutputDirectory = "",
    [switch]$SkipInnerBuild
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$repoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$cargoCommand = Get-Command cargo -ErrorAction SilentlyContinue
if (-not $cargoCommand) {
    throw "Rust Cargo is required to build the branded installer"
}
$rustcCommand = Get-Command rustc -ErrorAction SilentlyContinue
if (-not $rustcCommand) {
    throw "Rust rustc is required to detect the Windows build architecture"
}

$cargoToml = Get-Content -LiteralPath (Join-Path $repoRoot "Cargo.toml") -Raw
$versionMatch = [regex]::Match($cargoToml, '(?ms)\[workspace\.package\].*?^version\s*=\s*"([^"]+)"')
if (-not $versionMatch.Success) {
    throw "Unable to read [workspace.package].version from Cargo.toml"
}
$version = $versionMatch.Groups[1].Value

$frontendCheck = Join-Path $repoRoot "scripts\check-installer-frontend.mjs"
$nodeCommand = Get-Command node -ErrorAction SilentlyContinue
if (-not $nodeCommand) {
    throw "Node.js is required to validate the branded installer frontend"
}
& $nodeCommand.Source $frontendCheck
if ($LASTEXITCODE -ne 0) {
    throw "Branded installer frontend validation failed with exit code $LASTEXITCODE"
}

$architectureTarget = $Target
if (-not $architectureTarget) {
    $rustcVersion = & $rustcCommand.Source -vV
    if ($LASTEXITCODE -ne 0) {
        throw "Unable to query the native Rust host target (rustc exited with $LASTEXITCODE)"
    }
    $hostLine = @($rustcVersion | Where-Object { $_ -match '^host:\s*(\S+)\s*$' })
    if ($hostLine.Count -ne 1) {
        throw "Unable to read the native Rust host target from rustc -vV"
    }
    $architectureTarget = [regex]::Match($hostLine[0], '^host:\s*(\S+)\s*$').Groups[1].Value
}

$windowsTargetMatch = [regex]::Match(
    $architectureTarget,
    '^(x86_64|aarch64|i686|i586)-[^-]+-windows-(msvc|gnu|gnullvm)$'
)
if (-not $windowsTargetMatch.Success) {
    throw "The branded installer requires a Windows Rust target, got: $architectureTarget"
}

$targetRoot = Join-Path $repoRoot "target"
if ($Target) {
    $releaseRoot = Join-Path $targetRoot "$Target\release"
} else {
    $releaseRoot = Join-Path $targetRoot "release"
}

$targetArchitecture = $windowsTargetMatch.Groups[1].Value
$architecture = if ($targetArchitecture -eq "aarch64") {
    "arm64"
} elseif ($targetArchitecture -in @("i686", "i586")) {
    "x86"
} else {
    "x64"
}

if (-not $SkipInnerBuild) {
    $tuiBuildArgs = @("build", "--release", "-p", "r-code-tui", "--bin", "r-code-tui")
    if ($Target) {
        $tuiBuildArgs += @("--target", $Target)
    }
    Push-Location $repoRoot
    try {
        & $cargoCommand.Source @tuiBuildArgs
        if ($LASTEXITCODE -ne 0) {
            throw "r-code-tui sidecar build failed with exit code $LASTEXITCODE"
        }
    } finally {
        Pop-Location
    }
    $tuiSource = Join-Path $releaseRoot "r-code-tui.exe"
    if (-not (Test-Path -LiteralPath $tuiSource -PathType Leaf)) {
        throw "Expected r-code-tui sidecar not found: $tuiSource"
    }
    $tuiDestinationDir = Join-Path $repoRoot "src-tauri\binaries"
    [IO.Directory]::CreateDirectory($tuiDestinationDir) | Out-Null
    Copy-Item -LiteralPath $tuiSource -Destination (Join-Path $tuiDestinationDir "r-code-tui-$architectureTarget.exe") -Force

    # T38: sidecar set = TUI + shared daemon + two built-in harness plugins.
    $harnessSidecars = @(
        @{ Package = "r-code-runtime"; Bin = "r-code-service" },
        @{ Package = "r-code-harness-native"; Bin = "r-code-harness-native" },
        @{ Package = "r-code-harness-codex"; Bin = "r-code-harness-codex" }
    )
    foreach ($sidecar in $harnessSidecars) {
        & $cargoCommand.Source @("build", "--release", "-p", $sidecar.Package, "--bin", $sidecar.Bin)
        if ($LASTEXITCODE -ne 0) {
            throw "$($sidecar.Bin) sidecar build failed with exit code $LASTEXITCODE"
        }
        $source = Join-Path $releaseRoot "$($sidecar.Bin).exe"
        if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
            throw "Expected $($sidecar.Bin) sidecar not found: $source"
        }
        Copy-Item -LiteralPath $source -Destination (Join-Path $tuiDestinationDir "$($sidecar.Bin)-$architectureTarget.exe") -Force
    }

    # T38: built-in plugin package resources (manifest + bin) staged beside
    # the app; the daemon registers them through the normal immutable
    # registry at startup - no source-code engine switch.
    $builtinPackages = @(
        @{ Staged = "native"; Binary = "r-code-harness-native" },
        @{ Staged = "codex"; Binary = "r-code-harness-codex" }
    )
    foreach ($builtin in $builtinPackages) {
        $stagedDir = Join-Path $repoRoot "src-tauri\plugins\$($builtin.Staged)"
        [IO.Directory]::CreateDirectory((Join-Path $stagedDir "bin")) | Out-Null
        Copy-Item -LiteralPath (Join-Path $releaseRoot "$($builtin.Binary).exe") `
            -Destination (Join-Path $stagedDir "bin\$($builtin.Binary).exe") -Force
        if (-not (Test-Path -LiteralPath (Join-Path $stagedDir "harness.json") -PathType Leaf)) {
            throw "Built-in manifest missing: src-tauri/plugins/$($builtin.Staged)/harness.json"
        }
    }

    $previousPackagingMode = [Environment]::GetEnvironmentVariable("R_CODE_TAURI_PACKAGING", "Process")
    [Environment]::SetEnvironmentVariable("R_CODE_TAURI_PACKAGING", "1", "Process")
    Push-Location (Join-Path $repoRoot "src-tauri")
    try {
        $tauriArgs = @(
            "tauri", "build",
            "--bundles", "nsis",
            "--config", "tauri.local-package.conf.json"
        )
        if ($Target) {
            $tauriArgs += @("--target", $Target)
        }
        & $cargoCommand.Source @tauriArgs
        if ($LASTEXITCODE -ne 0) {
            throw "Tauri NSIS build failed with exit code $LASTEXITCODE"
        }
    } finally {
        Pop-Location
        [Environment]::SetEnvironmentVariable("R_CODE_TAURI_PACKAGING", $previousPackagingMode, "Process")
    }
}

if (-not $InnerInstaller) {
    $nsisDirectory = Join-Path $releaseRoot "bundle\nsis"
    $expectedName = "R-Code_${version}_${architecture}-setup.exe"
    $candidate = Join-Path $nsisDirectory $expectedName
    if (-not (Test-Path -LiteralPath $candidate -PathType Leaf)) {
        throw "Expected NSIS payload not found: $candidate"
    }
    $InnerInstaller = $candidate
}
$InnerInstaller = [IO.Path]::GetFullPath($InnerInstaller)
if (-not (Test-Path -LiteralPath $InnerInstaller -PathType Leaf)) {
    throw "NSIS payload not found: $InnerInstaller"
}

$buildArgs = @("build", "--release", "-p", "r-code-installer", "--bins")
if ($Target) {
    $targetArgs = @("--target", $Target)
} else {
    $targetArgs = @()
}

Push-Location $repoRoot
try {
    & $cargoCommand.Source @buildArgs @targetArgs
    if ($LASTEXITCODE -ne 0) {
        throw "Branded installer binaries build failed with exit code $LASTEXITCODE"
    }
} finally {
    Pop-Location
}

$outerExecutable = Join-Path $releaseRoot "r-code-installer.exe"
$packerExecutable = Join-Path $releaseRoot "r-code-installer-pack.exe"
if (-not (Test-Path -LiteralPath $outerExecutable -PathType Leaf)) {
    throw "Outer installer executable not found: $outerExecutable"
}
if (-not (Test-Path -LiteralPath $packerExecutable -PathType Leaf)) {
    throw "Installer packer executable not found: $packerExecutable"
}

if (-not $OutputDirectory) {
    $OutputDirectory = Join-Path $releaseRoot "bundle\branded"
} elseif (-not [IO.Path]::IsPathRooted($OutputDirectory)) {
    $OutputDirectory = Join-Path $repoRoot $OutputDirectory
}
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
[IO.Directory]::CreateDirectory($OutputDirectory) | Out-Null
$outputPath = Join-Path $OutputDirectory "R-Code_${version}_${architecture}-installer.exe"

& $packerExecutable $outerExecutable $InnerInstaller $outputPath
if ($LASTEXITCODE -ne 0) {
    throw "Installer composition failed with exit code $LASTEXITCODE"
}

$hashStream = [IO.File]::OpenRead($outputPath)
try {
    $sha256 = [Security.Cryptography.SHA256]::Create()
    try {
        $hashBytes = $sha256.ComputeHash($hashStream)
    } finally {
        $sha256.Dispose()
    }
} finally {
    $hashStream.Dispose()
}
$hash = [BitConverter]::ToString($hashBytes).Replace("-", "")
$result = [pscustomobject]@{
    Path = $outputPath
    SizeBytes = (Get-Item -LiteralPath $outputPath).Length
    SHA256 = $hash
    Payload = $InnerInstaller
}
$result | Format-List
