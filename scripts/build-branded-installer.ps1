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
