param(
    [string]$Executable = (Join-Path $PSScriptRoot '../build-mingw/ScreenCastSendCpp.exe'),
    [int[]]$Sizes = @(512, 640),
    [string]$ResultsDirectory = (Join-Path $PSScriptRoot ('results/' + (Get-Date -Format 'yyyyMMdd-HHmmss'))),
    [switch]$ComparePrecision
)
$ErrorActionPreference = 'Stop'
$exe = (Resolve-Path -LiteralPath $Executable).Path
New-Item -ItemType Directory -Force -Path $ResultsDirectory | Out-Null
$results = (Resolve-Path -LiteralPath $ResultsDirectory).Path
$precisionModes = if ($ComparePrecision) { @($false, $true) } else { @($false) }
# Keep one sender/receiver pair at a time to avoid contaminating comparisons.
foreach ($size in $Sizes) {
    foreach ($format in @('nv12', 'bgrx')) {
        foreach ($precision in $precisionModes) {
            $label = "${size}-${format}-precision$([int]$precision)"
            $run = Join-Path $results $label
            New-Item -ItemType Directory -Force -Path $run | Out-Null
            $arguments = @('--benchmark', '240', 'locked', 'DXGI', 'fast', 'motion', $format, "size=$size")
            if ($precision) { $arguments += 'precision' }
            Write-Output "Running $label (keep the Windows desktop unlocked)"
            $process = Start-Process -FilePath $exe -WorkingDirectory $run -ArgumentList $arguments -PassThru
            if (!$process.WaitForExit(90000)) {
                $process.Kill()
                throw "Benchmark timed out: $label"
            }
            $log = Join-Path $run 'native-test.txt'
            if (!(Test-Path -LiteralPath $log)) { throw "Missing result: $label" }
            Get-Content -LiteralPath $log
            if ($process.ExitCode -ne 0) { throw "Benchmark failed: $label; see $log" }
        }
    }
}
Write-Output "Results saved to $results"
Write-Output 'These are same-machine sender/receiver measurements; they do not measure the AI machine or cross-machine one-way latency.'
