[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [ValidatePattern('^[A-Za-z0-9](?:[A-Za-z0-9._-]*[A-Za-z0-9])?$')]
  [string]$BundleId,

  [string]$RoamingBase = [Environment]::GetFolderPath('ApplicationData'),
  [string]$LocalBase = [Environment]::GetFolderPath('LocalApplicationData'),

  [ValidateRange(1, 30)]
  [int]$RetrySeconds = 8,

  [switch]$SkipCodexRegistration
)

$ErrorActionPreference = 'SilentlyContinue'

function Get-NormalizedPath {
  param([Parameter(Mandatory = $true)][string]$Path)

  return [IO.Path]::GetFullPath($Path)
}

function Get-ManagedRoot {
  param([Parameter(Mandatory = $true)][string]$BasePath)

  if ([string]::IsNullOrWhiteSpace($BasePath)) {
    throw 'The application-data base path is empty.'
  }

  return Get-NormalizedPath (Join-Path $BasePath $BundleId)
}

$roamingRoot = Get-ManagedRoot $RoamingBase
$localRoot = Get-ManagedRoot $LocalBase
$dataRoot = Get-NormalizedPath (Join-Path $roamingRoot 'r-code')
$hostRoot = Get-NormalizedPath (Join-Path $dataRoot 'mcp-host')
$hostPrefix = $hostRoot.TrimEnd([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar

function Test-ManagedHostPath {
  param([AllowNull()][string]$Path)

  if ([string]::IsNullOrWhiteSpace($Path)) {
    return $false
  }

  try {
    $fullPath = Get-NormalizedPath $Path
    $fileName = [IO.Path]::GetFileName($fullPath)
    return $fullPath.StartsWith($hostPrefix, [StringComparison]::OrdinalIgnoreCase) -and
      $fileName -match '^r-code-mcp-host-[a-f0-9]{64}\.exe$'
  } catch {
    return $false
  }
}

function Remove-OwnedCodexRegistration {
  if ($SkipCodexRegistration) {
    return
  }

  try {
    $codex = Get-Command codex -ErrorAction SilentlyContinue
    if (-not $codex -or [string]::IsNullOrWhiteSpace([string]$codex.Source)) {
      return
    }

    $registration = (& $codex.Source mcp get r-code --json 2>$null | Out-String | ConvertFrom-Json)
    $registeredArgs = @($registration.transport.args)
    $ownsRegistration =
      [string]::Equals([string]$registration.name, 'r-code', [StringComparison]::OrdinalIgnoreCase) -and
      [string]::Equals([string]$registration.transport.type, 'stdio', [StringComparison]::OrdinalIgnoreCase) -and
      (Test-ManagedHostPath ([string]$registration.transport.command)) -and
      $registeredArgs.Count -eq 3 -and
      [string]::Equals([string]$registeredArgs[0], 'mcp-server', [StringComparison]::OrdinalIgnoreCase) -and
      [string]::Equals([string]$registeredArgs[1], '--data-dir', [StringComparison]::OrdinalIgnoreCase) -and
      [string]::Equals((Get-NormalizedPath ([string]$registeredArgs[2])), $dataRoot, [StringComparison]::OrdinalIgnoreCase)

    if ($ownsRegistration) {
      & $codex.Source mcp remove r-code *> $null
    }
  } catch {
    # An absent or incompatible Codex CLI must not block local-data cleanup.
  }
}

function Get-ManagedHostProcesses {
  return @(Get-CimInstance Win32_Process -ErrorAction SilentlyContinue | Where-Object {
      Test-ManagedHostPath ([string]$_.ExecutablePath)
    })
}

function Remove-ManagedRoots {
  foreach ($root in @($roamingRoot, $localRoot)) {
    if (Test-Path -LiteralPath $root) {
      Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
    }
  }
}

function Test-ManagedRootsRemoved {
  return -not (Test-Path -LiteralPath $roamingRoot) -and
    -not (Test-Path -LiteralPath $localRoot)
}

Remove-OwnedCodexRegistration

$stoppedProcessIds = [Collections.Generic.HashSet[int]]::new()
$deadline = [DateTime]::UtcNow.AddSeconds($RetrySeconds)
do {
  $managedHosts = Get-ManagedHostProcesses
  foreach ($managedHost in $managedHosts) {
    [void]$stoppedProcessIds.Add([int]$managedHost.ProcessId)
    Stop-Process -Id $managedHost.ProcessId -Force -ErrorAction SilentlyContinue
  }

  if ($managedHosts.Count -gt 0) {
    Start-Sleep -Milliseconds 250
  }

  Remove-ManagedRoots
  if (Test-ManagedRootsRemoved) {
    Write-Output "removed=1 stopped=$($stoppedProcessIds.Count)"
    exit 0
  }

  Start-Sleep -Milliseconds 350
} while ([DateTime]::UtcNow -lt $deadline)

Write-Output "removed=0 stopped=$($stoppedProcessIds.Count)"
exit 20
