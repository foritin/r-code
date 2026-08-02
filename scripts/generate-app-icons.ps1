[CmdletBinding()]
param(
    [switch]$WindowsOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$iconsRoot = Join-Path $repoRoot "icons"
$fullbleedSource = Join-Path $iconsRoot "source\master\r-code-icon-bright-1024-fullbleed.png"
$insetSource = Join-Path $iconsRoot "source\master\r-code-icon-bright-1024.png"

$requiredSources = @($fullbleedSource)
if (-not $WindowsOnly) {
    $requiredSources += $insetSource
}
foreach ($source in $requiredSources) {
    if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
        throw "Icon source not found: $source"
    }
}

$cargo = Get-Command cargo -ErrorAction SilentlyContinue
if (-not $cargo) {
    throw "cargo is required to generate the Tauri icon assets"
}

$node = Get-Command node -ErrorAction SilentlyContinue
if (-not $node) {
    throw "node is required to render the pixel-aligned small icon assets"
}

$tempRoot = Join-Path ([IO.Path]::GetTempPath()) ("r-code-icons-" + [Guid]::NewGuid().ToString("N"))
$pngOutput = Join-Path $tempRoot "png"
$platformOutput = Join-Path $tempRoot "platform"
[IO.Directory]::CreateDirectory($pngOutput) | Out-Null
[IO.Directory]::CreateDirectory($platformOutput) | Out-Null

function Invoke-TauriIcon {
    param([string[]]$Arguments)

    & $cargo.Source @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "cargo tauri icon failed with exit code $LASTEXITCODE"
    }
}

function Write-PngIco {
    param(
        [string]$OutputPath,
        [string]$InputDirectory,
        [int[]]$Sizes
    )

    $frames = foreach ($size in $Sizes) {
        $path = Join-Path $InputDirectory ("{0}x{0}.png" -f $size)
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            throw "ICO frame not found: $path"
        }
        [pscustomobject]@{
            Size = $size
            Bytes = [IO.File]::ReadAllBytes($path)
        }
    }

    $stream = [IO.File]::Open($OutputPath, [IO.FileMode]::Create, [IO.FileAccess]::Write)
    $writer = [IO.BinaryWriter]::new($stream)
    try {
        $writer.Write([uint16]0)
        $writer.Write([uint16]1)
        $writer.Write([uint16]$frames.Count)

        $offset = 6 + (16 * $frames.Count)
        foreach ($frame in $frames) {
            $dimension = if ($frame.Size -eq 256) { 0 } else { $frame.Size }
            $writer.Write([byte]$dimension)
            $writer.Write([byte]$dimension)
            $writer.Write([byte]0)
            $writer.Write([byte]0)
            $writer.Write([uint16]1)
            $writer.Write([uint16]32)
            $writer.Write([uint32]$frame.Bytes.Length)
            $writer.Write([uint32]$offset)
            $offset += $frame.Bytes.Length
        }

        foreach ($frame in $frames) {
            $writer.Write([byte[]]$frame.Bytes)
        }
    } finally {
        $writer.Dispose()
        $stream.Dispose()
    }
}

function Invoke-SmallIconRenderer {
    param([string]$OutputDirectory)

    $renderer = Join-Path $repoRoot "scripts\render-small-app-icons.mjs"
    & $node.Source $renderer --output $OutputDirectory --sizes "16,20,24,32"
    if ($LASTEXITCODE -ne 0) {
        throw "small icon renderer failed with exit code $LASTEXITCODE"
    }
}

try {
    $sizes = @(16, 20, 24, 32, 40, 48, 64, 96, 128, 256, 512)
    Invoke-TauriIcon -Arguments @(
        "tauri", "icon", $fullbleedSource,
        "--output", $pngOutput,
        "--png", ($sizes -join ",")
    )
    if (-not $WindowsOnly) {
        Invoke-TauriIcon -Arguments @(
            "tauri", "icon", $insetSource,
            "--output", $platformOutput
        )
    }
    Invoke-SmallIconRenderer -OutputDirectory $pngOutput

    Copy-Item -LiteralPath (Join-Path $pngOutput "32x32.png") -Destination (Join-Path $iconsRoot "32x32.png") -Force
    Copy-Item -LiteralPath (Join-Path $pngOutput "128x128.png") -Destination (Join-Path $iconsRoot "128x128.png") -Force
    Copy-Item -LiteralPath (Join-Path $pngOutput "256x256.png") -Destination (Join-Path $iconsRoot "128x128@2x.png") -Force
    Copy-Item -LiteralPath (Join-Path $pngOutput "512x512.png") -Destination (Join-Path $iconsRoot "512x512.png") -Force
    Copy-Item -LiteralPath $fullbleedSource -Destination (Join-Path $iconsRoot "icon.png") -Force
    if (-not $WindowsOnly) {
        Copy-Item -LiteralPath (Join-Path $platformOutput "icon.icns") -Destination (Join-Path $iconsRoot "icon.icns") -Force
    }
    Copy-Item -LiteralPath (Join-Path $pngOutput "512x512.png") -Destination (Join-Path $repoRoot "installer\frontend\icon.png") -Force

    Write-PngIco `
        -OutputPath (Join-Path $iconsRoot "icon.ico") `
        -InputDirectory $pngOutput `
        -Sizes @(16, 20, 24, 32, 40, 48, 64, 96, 128, 256)

    $scope = if ($WindowsOnly) { "Windows/Linux" } else { "all platforms" }
    Write-Host "R-Code $scope icons regenerated with pixel-aligned Windows small frames."
} finally {
    $resolvedTemp = [IO.Path]::GetFullPath($tempRoot)
    $systemTemp = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
    if ($resolvedTemp.StartsWith($systemTemp, [StringComparison]::OrdinalIgnoreCase) -and
        (Test-Path -LiteralPath $resolvedTemp)) {
        Remove-Item -LiteralPath $resolvedTemp -Recurse -Force
    }
}
