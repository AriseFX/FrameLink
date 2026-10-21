#Requires -Version 5.1
[CmdletBinding()]
param(
    [string]$Executable = $(
        if (Test-Path -LiteralPath (Join-Path $PSScriptRoot '../FrameLink.exe')) {
            Join-Path $PSScriptRoot '../FrameLink.exe'
        } else {
            Join-Path $PSScriptRoot '../dist/FrameLink-3.1.1-dev-Windows-x64/FrameLink.exe'
        }
    ),
    [ValidateRange(3, 60)][int]$TimeoutSeconds = 15,
    [ValidateRange(2, 16)][int]$ConcurrentLaunches = 6
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
    throw 'This verification requires an interactive Windows desktop.'
}

if ($null -eq ('FrameLink.SingleInstanceVerification.Native' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
namespace FrameLink.SingleInstanceVerification {
    public static class Native {
        private const string Marker = "FrameLink.SingleInstance.Window.7D57CFC8-11A1-4E8C-91B7-8EE8F5F98D45";
        private delegate bool WindowCallback(IntPtr window, IntPtr state);
        [DllImport("user32.dll")] private static extern bool EnumWindows(WindowCallback callback, IntPtr state);
        [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr window, out uint processId);
        [DllImport("user32.dll", CharSet = CharSet.Unicode, EntryPoint = "GetPropW")]
        private static extern IntPtr GetProp(IntPtr window, string property);
        [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr window);
        [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr window);
        [DllImport("user32.dll")] public static extern bool ShowWindowAsync(IntPtr window, int command);
        [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr window, uint message, IntPtr wParam, IntPtr lParam);
        [DllImport("user32.dll")] private static extern IntPtr GetForegroundWindow();
        [DllImport("user32.dll")] private static extern IntPtr GetAncestor(IntPtr window, uint flags);
        public static IntPtr FindMarkedWindow(uint processId) {
            IntPtr result = IntPtr.Zero;
            EnumWindows(delegate(IntPtr window, IntPtr state) {
                uint owner;
                GetWindowThreadProcessId(window, out owner);
                if ((processId == 0 || owner == processId) && GetProp(window, Marker) != IntPtr.Zero) {
                    result = window;
                    return false;
                }
                return true;
            }, IntPtr.Zero);
            return result;
        }
        public static bool IsForegroundRoot(IntPtr window) {
            IntPtr foreground = GetForegroundWindow();
            return foreground != IntPtr.Zero && GetAncestor(foreground, 2) == window;
        }
    }
}
'@
}

$exe = (Resolve-Path -LiteralPath $Executable).ProviderPath
$processName = [IO.Path]::GetFileNameWithoutExtension($exe)
$script:ownedProcesses = [Collections.Generic.List[Diagnostics.Process]]::new()
$settings = Join-Path $env:LOCALAPPDATA 'FrameLink/settings.toml'

function Assert-Condition([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

function Get-TargetProcesses {
    foreach ($candidate in @(Get-Process -Name $processName -ErrorAction SilentlyContinue)) {
        try {
            $candidate.Refresh()
            if ($candidate.HasExited) { continue }
            $candidatePath = $candidate.Path
            if ([string]::IsNullOrEmpty($candidatePath)) {
                throw "Cannot inspect executable path for PID $($candidate.Id)."
            }
            if ([string]::Equals($candidatePath, $exe, [StringComparison]::OrdinalIgnoreCase)) {
                $candidate
            }
        } catch {
            $candidate.Refresh()
            if (-not $candidate.HasExited) { throw }
        }
    }
}

function Start-OwnedProcess {
    $launched = Start-Process -FilePath $exe -PassThru
    $script:ownedProcesses.Add($launched)
    # Keep a native process handle before a short-lived duplicate exits.
    # Windows PowerShell otherwise may lose its exit code during Refresh().
    $null = $launched.Handle
    return $launched
}

function Test-Running([Diagnostics.Process]$Process) {
    $Process.Refresh()
    return -not $Process.HasExited
}

function Wait-Condition([scriptblock]$Condition, [string]$Message) {
    $timer = [Diagnostics.Stopwatch]::StartNew()
    while ($timer.Elapsed.TotalSeconds -lt $TimeoutSeconds) {
        if (& $Condition) { return }
        Start-Sleep -Milliseconds 100
    }
    throw $Message
}

function Wait-VisibleWindow([Diagnostics.Process]$Process) {
    Wait-Condition {
        Assert-Condition (Test-Running $Process) "Primary process $($Process.Id) exited before its window was ready."
        $handle = [FrameLink.SingleInstanceVerification.Native]::FindMarkedWindow([uint32]$Process.Id)
        return $handle -ne [IntPtr]::Zero -and [FrameLink.SingleInstanceVerification.Native]::IsWindowVisible($handle)
    } 'The marked FrameLink root window did not become visible.'
    return [FrameLink.SingleInstanceVerification.Native]::FindMarkedWindow([uint32]$Process.Id)
}

function Assert-OneOriginal([Diagnostics.Process]$Original) {
    Assert-Condition (Test-Running $Original) 'The original process exited during activation.'
    $running = @(Get-TargetProcesses)
    Assert-Condition ($running.Count -eq 1 -and $running[0].Id -eq $Original.Id) 'Expected exactly one persistent process with the original PID.'
}

function Wait-Restored([IntPtr]$Window) {
    Wait-Condition {
        return [FrameLink.SingleInstanceVerification.Native]::IsWindowVisible($Window) -and
            -not [FrameLink.SingleInstanceVerification.Native]::IsIconic($Window) -and
            [FrameLink.SingleInstanceVerification.Native]::IsForegroundRoot($Window)
    } 'The original window was not restored to the foreground. Do not interact with other windows during this test.'
}

function Wait-DuplicateExit([Diagnostics.Process]$Duplicate) {
    Assert-Condition ($Duplicate.WaitForExit($TimeoutSeconds * 1000)) "Duplicate PID $($Duplicate.Id) did not exit."
    Assert-Condition ($Duplicate.ExitCode -eq 0) "Duplicate PID $($Duplicate.Id) exited with code $($Duplicate.ExitCode)."
}

function Invoke-DuplicateActivation([Diagnostics.Process]$Original, [IntPtr]$Window) {
    $duplicate = Start-OwnedProcess
    Wait-DuplicateExit $duplicate
    Wait-Restored $Window
    Assert-OneOriginal $Original
    Assert-Condition ([FrameLink.SingleInstanceVerification.Native]::FindMarkedWindow([uint32]$Original.Id) -eq $Window) 'Activation replaced the root window.'
}

function Get-SettingsDigest {
    if (Test-Path -LiteralPath $settings) {
        return (Get-FileHash -LiteralPath $settings -Algorithm SHA256).Hash
    }
    return '<absent>'
}

function Get-CloseToTray {
    $candidates = @($settings, (Join-Path $env:LOCALAPPDATA 'ScreenCastSendRust/settings.toml'))
    foreach ($candidate in $candidates) {
        if (Test-Path -LiteralPath $candidate) {
            $text = [IO.File]::ReadAllText($candidate)
            $match = [regex]::Match($text, '(?m)^\s*close_to_tray\s*=\s*(true|false)\s*(?:#.*)?$')
            if ($match.Success) { return $match.Groups[1].Value -eq 'true' }
            return $true
        }
    }
    return $true
}

Assert-Condition (@(Get-TargetProcesses).Count -eq 0) 'Exit the existing same-path FrameLink process before running this verification.'
Assert-Condition ([FrameLink.SingleInstanceVerification.Native]::FindMarkedWindow(0) -eq [IntPtr]::Zero) 'An existing FrameLink window is running. Exit it first; this script will not alter another session.'
$existingMutex = $null
if ([Threading.Mutex]::TryOpenExisting('Global\FrameLink.Instance.7D57CFC8-11A1-4E8C-91B7-8EE8F5F98D45', [ref]$existingMutex)) {
    $existingMutex.Dispose()
    throw 'An existing machine-wide FrameLink instance is present, possibly on another desktop. Exit it first.'
}
Write-Output 'Starting verification. Do not interact with the desktop or launch FrameLink until it finishes.'
Write-Output 'This script does not edit settings or start streaming. It cleans up only processes that it creates.'

try {
    $primary = Start-OwnedProcess
    $window = Wait-VisibleWindow $primary
    Start-Sleep -Milliseconds 1000
    $baselineSettings = Get-SettingsDigest
    Assert-OneOriginal $primary

    Invoke-DuplicateActivation $primary $window
    Write-Output 'PASS: duplicate launch activates the same original window and exits.'

    [void][FrameLink.SingleInstanceVerification.Native]::ShowWindowAsync($window, 6)
    Wait-Condition { [FrameLink.SingleInstanceVerification.Native]::IsIconic($window) } 'The window did not minimize.'
    Invoke-DuplicateActivation $primary $window
    Write-Output 'PASS: duplicate launch restores the minimized original window.'

    if (Get-CloseToTray) {
        Assert-Condition ([FrameLink.SingleInstanceVerification.Native]::PostMessage($window, 0x10, [IntPtr]::Zero, [IntPtr]::Zero)) 'Unable to post WM_CLOSE.'
        Wait-Condition {
            Assert-Condition (Test-Running $primary) 'Closing exited the process despite close_to_tray=true; check tray availability and application errors.'
            return -not [FrameLink.SingleInstanceVerification.Native]::IsWindowVisible($window)
        } 'Closing the window did not hide it to the tray.'
        Invoke-DuplicateActivation $primary $window
        Write-Output 'PASS: duplicate launch restores the same hidden tray window.'
    } else {
        Write-Output 'SKIP: close-to-tray test; the effective user setting is close_to_tray=false. No settings were edited by this script.'
    }

    [void][FrameLink.SingleInstanceVerification.Native]::ShowWindowAsync($window, 6)
    Wait-Condition { [FrameLink.SingleInstanceVerification.Native]::IsIconic($window) } 'The window did not minimize before the launch burst.'
    $duplicates = @(for ($index = 0; $index -lt $ConcurrentLaunches; $index++) { Start-OwnedProcess })
    foreach ($duplicate in $duplicates) { Wait-DuplicateExit $duplicate }
    Wait-Restored $window
    Assert-OneOriginal $primary
    Assert-Condition ((Get-SettingsDigest) -eq $baselineSettings) 'Settings changed during duplicate activation.'
    Write-Output "PASS: $ConcurrentLaunches near-simultaneous duplicate launches retain only the original PID and preserve saved settings."

    $primary.Kill()
    Assert-Condition ($primary.WaitForExit($TimeoutSeconds * 1000)) 'The test-owned primary process did not terminate.'
    Wait-Condition { @(Get-TargetProcesses).Count -eq 0 } 'The terminated process did not disappear.'

    $coldStarts = @(for ($index = 0; $index -lt $ConcurrentLaunches; $index++) { Start-OwnedProcess })
    Wait-Condition {
        $survivors = @($coldStarts | Where-Object { Test-Running $_ })
        if ($survivors.Count -ne 1) { return $false }
        $handle = [FrameLink.SingleInstanceVerification.Native]::FindMarkedWindow([uint32]$survivors[0].Id)
        return $handle -ne [IntPtr]::Zero -and [FrameLink.SingleInstanceVerification.Native]::IsWindowVisible($handle)
    } 'Concurrent restart after forced termination did not leave one visible instance.'
    $replacement = @($coldStarts | Where-Object { Test-Running $_ })[0]
    foreach ($candidate in $coldStarts) {
        if ($candidate.Id -ne $replacement.Id) { Wait-DuplicateExit $candidate }
    }
    Assert-OneOriginal $replacement
    Write-Output "PASS: forced termination releases the lock; $ConcurrentLaunches concurrent restarts leave one usable instance."
    Write-Output 'PASS: automated single-instance scenarios completed. Streaming continuity remains a separate manual check.'
} finally {
    foreach ($owned in $script:ownedProcesses) {
        try {
            if (Test-Running $owned) {
                $owned.Kill()
                [void]$owned.WaitForExit($TimeoutSeconds * 1000)
            }
        } catch {
            Write-Warning "Could not clean up test-owned PID $($owned.Id): $($_.Exception.Message)"
        }
        $owned.Dispose()
    }
}
