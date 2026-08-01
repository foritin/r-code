[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$iconsRoot = Join-Path $repoRoot "icons"
$fullbleedSource = Join-Path $iconsRoot "source\master\r-code-icon-bright-1024-fullbleed.png"
$insetSource = Join-Path $iconsRoot "source\master\r-code-icon-bright-1024.png"

foreach ($source in @($fullbleedSource, $insetSource)) {
    if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
        throw "Icon source not found: $source"
    }
}

$cargo = Get-Command cargo -ErrorAction SilentlyContinue
if (-not $cargo) {
    throw "cargo is required to generate the Tauri icon assets"
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

try {
    $sizes = @(16, 20, 24, 32, 40, 48, 64, 96, 128, 256, 512)
    Invoke-TauriIcon -Arguments @(
        "tauri", "icon", $fullbleedSource,
        "--output", $pngOutput,
        "--png", ($sizes -join ",")
    )
    Invoke-TauriIcon -Arguments @(
        "tauri", "icon", $insetSource,
        "--output", $platformOutput
    )

    Copy-Item -LiteralPath (Join-Path $pngOutput "32x32.png") -Destination (Join-Path $iconsRoot "32x32.png") -Force
    Copy-Item -LiteralPath (Join-Path $pngOutput "128x128.png") -Destination (Join-Path $iconsRoot "128x128.png") -Force
    Copy-Item -LiteralPath (Join-Path $pngOutput "256x256.png") -Destination (Join-Path $iconsRoot "128x128@2x.png") -Force
    Copy-Item -LiteralPath (Join-Path $pngOutput "512x512.png") -Destination (Join-Path $iconsRoot "512x512.png") -Force
    Copy-Item -LiteralPath $fullbleedSource -Destination (Join-Path $iconsRoot "icon.png") -Force
    Copy-Item -LiteralPath (Join-Path $platformOutput "icon.icns") -Destination (Join-Path $iconsRoot "icon.icns") -Force
    Copy-Item -LiteralPath (Join-Path $pngOutput "512x512.png") -Destination (Join-Path $repoRoot "installer\frontend\icon.png") -Force

    Write-PngIco `
        -OutputPath (Join-Path $iconsRoot "icon.ico") `
        -InputDirectory $pngOutput `
        -Sizes @(16, 20, 24, 32, 40, 48, 64, 96, 128, 256)

    Write-Host "R-Code application icons regenerated from the bright masters."
} finally {
    $resolvedTemp = [IO.Path]::GetFullPath($tempRoot)
    $systemTemp = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
    if ($resolvedTemp.StartsWith($systemTemp, [StringComparison]::OrdinalIgnoreCase) -and
        (Test-Path -LiteralPath $resolvedTemp)) {
        Remove-Item -LiteralPath $resolvedTemp -Recurse -Force
    }
}
