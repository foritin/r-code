[CmdletBinding()]
param(
    [string]$Target = "",
    [string]$InnerInstaller = "",
    [string]$OutputDirectory = "",
    [switch]$SkipInnerBuild
)

$ErrorActionPreference = "Stop"
$repoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
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

if ($Target -and $Target -notmatch 'windows') {
    throw "The branded installer can only be built for a Windows target"
}

$targetRoot = Join-Path $repoRoot "target"
if ($Target) {
    $releaseRoot = Join-Path $targetRoot "$Target\release"
} else {
    $releaseRoot = Join-Path $targetRoot "release"
}

$architecture = if ($Target -match '^aarch64') {
    "arm64"
} elseif ($Target -match '^(i686|i586)') {
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
        & cargo @tauriArgs
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
        $matches = @(Get-ChildItem -LiteralPath $nsisDirectory -Filter "R-Code_${version}_*-setup.exe" -File -ErrorAction SilentlyContinue)
        if ($matches.Count -ne 1) {
            throw "Expected exactly one NSIS payload in $nsisDirectory, found $($matches.Count)"
        }
        $candidate = $matches[0].FullName
    }
    $InnerInstaller = $candidate
}
$InnerInstaller = [IO.Path]::GetFullPath($InnerInstaller)
if (-not (Test-Path -LiteralPath $InnerInstaller -PathType Leaf)) {
    throw "NSIS payload not found: $InnerInstaller"
}

$buildArgs = @("build", "--release", "-p", "r-code-installer", "--bin")
if ($Target) {
    $targetArgs = @("--target", $Target)
} else {
    $targetArgs = @()
}

Push-Location $repoRoot
try {
    & cargo @buildArgs "r-code-installer" @targetArgs
    if ($LASTEXITCODE -ne 0) {
        throw "Branded installer build failed with exit code $LASTEXITCODE"
    }
    & cargo @buildArgs "r-code-installer-pack" @targetArgs
    if ($LASTEXITCODE -ne 0) {
        throw "Installer packer build failed with exit code $LASTEXITCODE"
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
