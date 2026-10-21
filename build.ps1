param([string]$Zig = 'zig', [string]$OutputDirectory = (Join-Path $PSScriptRoot 'build-zig'))
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$resource = Join-Path $OutputDirectory 'app.res'
& $Zig rc /I (Join-Path $PSScriptRoot 'resources') /fo $resource (Join-Path $PSScriptRoot 'resources\app.rc')
if ($LASTEXITCODE -ne 0) { throw 'Resource compilation failed.' }
$sources = @((Join-Path $PSScriptRoot 'src\main.cpp'), (Join-Path $PSScriptRoot 'src\selftest.cpp'))
$libyuv = Join-Path $PSScriptRoot 'third_party\libyuv'
$yuvSources = @(Get-ChildItem (Join-Path $libyuv 'source') -Filter '*.cc' | Where-Object { $_.Name -notmatch '_(neon|neon64|sve|sme|lsx|lasx)' } | ForEach-Object { $_.FullName })
& $Zig c++ -target x86_64-windows-gnu -std=c++17 -O3 -DNDEBUG -static -municode '-Wl,--subsystem,windows' -I (Join-Path $libyuv 'include') -Wall -Wextra -Wno-missing-field-initializers $sources $yuvSources $resource -o (Join-Path $OutputDirectory 'ScreenCastSendCpp.exe') -ld3d11 -ldxgi -luuid -lgdi32 -luser32 -lcomctl32 -lshell32 -lole32 -lwinmm
if ($LASTEXITCODE -ne 0) { throw 'Native C++ build failed.' }
foreach ($name in @('Processing.NDI.Lib.x64.dll', 'NDI License Agreement.pdf', 'Processing.NDI.Lib.Licenses.txt')) {
    if (([IO.Path]::GetFullPath($OutputDirectory)).TrimEnd('\') -ne $PSScriptRoot.TrimEnd('\')) {
        Copy-Item -LiteralPath (Join-Path $PSScriptRoot $name) -Destination $OutputDirectory -Force
    }
}
Write-Output (Join-Path $OutputDirectory 'ScreenCastSendCpp.exe')
