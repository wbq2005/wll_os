param(
    [Parameter(Mandatory = $true)]
    [ValidateSet("riscv64", "loongarch64")]
    [string]$Arch,

    [Parameter(Mandatory = $true)]
    [string]$Image,

    [int]$TimeoutSeconds = 6250,
    [string]$Repo = (Split-Path -Parent $PSScriptRoot),
    [string]$PythonExe = "python.exe",
    [string]$BuildFeatures = ""
)

$ErrorActionPreference = "Stop"
$tmp = Join-Path $Repo "_tmp"
$stdout = Join-Path $tmp "buildstorm-$Arch-supervisor.out"
$stderr = Join-Path $tmp "buildstorm-$Arch-supervisor.err"
$exitFile = Join-Path $tmp "buildstorm-$Arch-supervisor.exit"
$startedFile = Join-Path $tmp "buildstorm-$Arch-supervisor.started"

Set-Content -LiteralPath $startedFile -Value ([DateTimeOffset]::Now.ToString("o")) -Encoding ascii
Remove-Item -LiteralPath $exitFile -Force -ErrorAction SilentlyContinue

$arguments = @(
    "scripts/run_buildstorm.py",
    "--arch", $Arch,
    "--image", $Image,
    "--stage", "complete",
    "--timeout", $TimeoutSeconds
)
if ($BuildFeatures) {
    $arguments += @("--build-features", $BuildFeatures)
}

Push-Location $Repo
try {
    & $PythonExe @arguments 1> $stdout 2> $stderr
    $exitCode = $LASTEXITCODE
} finally {
    Pop-Location
}

Set-Content -LiteralPath $exitFile -Value $exitCode -Encoding ascii
