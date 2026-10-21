param([string]$Executable = (Join-Path $PSScriptRoot '../build-mingw/ScreenCastSendCpp.exe'))
$ErrorActionPreference = 'Stop'
Add-Type -ReferencedAssemblies System.Drawing -TypeDefinition @'
using System;
using System.Drawing;
using System.Drawing.Imaging;
using System.Runtime.InteropServices;
using System.Text;
public static class NativeUI {
    [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)] public struct Point { public int X, Y; }
    [StructLayout(LayoutKind.Sequential,CharSet=CharSet.Unicode)] public struct MonitorInfo { public int Size; public Rect Monitor, Work; public uint Flags; [MarshalAs(UnmanagedType.ByValTStr,SizeConst=32)] public string Device; }
    [DllImport("user32.dll")] static extern IntPtr MonitorFromPoint(Point point,uint flags);
    [DllImport("user32.dll",CharSet=CharSet.Unicode)] static extern bool GetMonitorInfo(IntPtr h,ref MonitorInfo info);
    static MonitorInfo PrimaryInfo() { var m=new MonitorInfo(); m.Size=Marshal.SizeOf(m); if(!GetMonitorInfo(MonitorFromPoint(new Point(),1),ref m)) throw new Exception("Monitor unavailable"); return m; }
    public static Rect PrimaryMonitor() { return PrimaryInfo().Monitor; }
    public static string PrimaryDevice() { return PrimaryInfo().Device; }
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string cls, string title);
    [DllImport("user32.dll")] public static extern IntPtr GetDlgItem(IntPtr h, int id);
    [DllImport("user32.dll", CharSet=CharSet.Unicode, EntryPoint="SendMessageW")] static extern IntPtr SendText(IntPtr h, uint m, IntPtr w, string text);
    [DllImport("user32.dll", CharSet=CharSet.Unicode, EntryPoint="SendMessageW")] static extern IntPtr ReadText(IntPtr h, uint m, IntPtr w, StringBuilder text);
    public static bool SetWindowText(IntPtr h,string text) { return SendText(h,0xc,IntPtr.Zero,text)!=IntPtr.Zero; }
    [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool IsWindowEnabled(IntPtr h);
    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr h, out Rect r);
    [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr h, int x, int y, int w, int height, bool repaint);
    [DllImport("user32.dll")] static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
    public static string Text(IntPtr h) { var s=new StringBuilder(256); ReadText(h,0xd,(IntPtr)256,s); return s.ToString(); }
    public static string ComboText(IntPtr h,int index) { var s=new StringBuilder(256); ReadText(h,0x148,(IntPtr)index,s); return s.ToString(); }
    public static void Screenshot(IntPtr h, string path) {
        Rect r; GetWindowRect(h,out r);
        using(var b=new Bitmap(r.Right-r.Left,r.Bottom-r.Top)) {
            using(var g=Graphics.FromImage(b)) { var dc=g.GetHdc(); try { if(!PrintWindow(h,dc,2)) throw new Exception("PrintWindow failed"); } finally { g.ReleaseHdc(dc); } }
            b.Save(path,ImageFormat.Png);
        }
    }
}
'@
$exe = (Resolve-Path -LiteralPath $Executable).Path
$ini = Join-Path $env:LOCALAPPDATA 'ScreenCastSendCpp/settings.ini'
$original = if (Test-Path $ini) { [IO.File]::ReadAllBytes($ini) } else { $null }
$settingsTouched = $false
$appProcess = $null
$sourceName = -join ([char[]](0x6d4b,0x8bd5,0x6e90))
function Assert-UI($value, $message) { if (!$value) { throw $message } }
function Control($window, $id) { [NativeUI]::GetDlgItem($window,$id) }
function Command($window, $id) { [void][NativeUI]::SendMessage($window,0x111,[IntPtr]$id,[IntPtr]::Zero) }
function Select-Option($window, $id, $index) {
    [void][NativeUI]::SendMessage((Control $window $id),0x14e,[IntPtr]$index,[IntPtr]::Zero)
    [void][NativeUI]::SendMessage($window,0x111,[IntPtr](($id) -bor (1 -shl 16)),(Control $window $id))
}
function Select-PrimaryMonitor($window) {
    $control = Control $window 110
    $device = [NativeUI]::PrimaryDevice()
    $count = [NativeUI]::SendMessage($control,0x146,[IntPtr]::Zero,[IntPtr]::Zero).ToInt64()
    for($index=0; $index -lt $count; $index++) {
        if([NativeUI]::ComboText($control,$index).StartsWith($device + '  ')) {
            Select-Option $window 110 $index
            return
        }
    }
    throw 'Primary monitor missing from monitor list'
}
function Assert-Centered($window,$size) {
    $monitor=[NativeUI]::PrimaryMonitor()
    $expectedX=$monitor.Left+[int][Math]::Floor(($monitor.Right-$monitor.Left-$size)/2)
    $expectedY=$monitor.Top+[int][Math]::Floor(($monitor.Bottom-$monitor.Top-$size)/2)
    Assert-UI ([NativeUI]::Text((Control $window 101)) -eq [string]$expectedX) 'Center X incorrect'
    Assert-UI ([NativeUI]::Text((Control $window 102)) -eq [string]$expectedY) 'Center Y incorrect'
    Assert-UI ([NativeUI]::Text((Control $window 103)) -eq [string]$size) 'Center width incorrect'
    Assert-UI ([NativeUI]::Text((Control $window 104)) -eq [string]$size) 'Center height incorrect'
}
try {
    Assert-UI ([NativeUI]::FindWindow('ScreenCastSendCpp','ScreenCast Send C++') -eq [IntPtr]::Zero) 'Exit the existing app before running UI verification'
    # Start with defaults; restore the user's exact settings in finally.
    $settingsTouched = $true
    if(Test-Path $ini) { Remove-Item -LiteralPath $ini }
    $appProcess = Start-Process -FilePath $exe -WindowStyle Hidden -PassThru
    $window = [IntPtr]::Zero
    for($attempt=0;$attempt -lt 100 -and $window -eq [IntPtr]::Zero;$attempt++) {
        Start-Sleep -Milliseconds 100
        $window = [NativeUI]::FindWindow('ScreenCastSendCpp','ScreenCast Send C++')
    }
    Assert-UI ($window -ne [IntPtr]::Zero) 'Main window missing'
    [void][NativeUI]::SendMessage($window,0x8001,[IntPtr]::Zero,[IntPtr]0x203)
    Assert-UI ((Control $window 120) -ne [IntPtr]::Zero) 'Precision scheduling checkbox missing'
    Assert-UI ([NativeUI]::SendMessage((Control $window 120),0xf0,[IntPtr]::Zero,[IntPtr]::Zero).ToInt64() -eq 0) 'Precision scheduling must default off'
    Assert-UI ([NativeUI]::SendMessage((Control $window 108),0xf0,[IntPtr]::Zero,[IntPtr]::Zero).ToInt64() -eq 0) 'Cursor must default off'
    Assert-UI ([NativeUI]::SendMessage((Control $window 109),0xf0,[IntPtr]::Zero,[IntPtr]::Zero).ToInt64() -eq 0) 'Preview must default off'
    Assert-UI ([NativeUI]::Text((Control $window 105)) -eq '240') 'Default FPS must be 240'
    [void][NativeUI]::SendMessage($window,0x10,[IntPtr]::Zero,[IntPtr]::Zero)
    Assert-UI (![NativeUI]::IsWindowVisible($window)) 'Idle close did not hide to tray'
    Assert-UI (!$appProcess.HasExited) 'Idle close exited the app'
    [void][NativeUI]::SendMessage($window,0x8001,[IntPtr]::Zero,[IntPtr]0x203)
    Assert-UI ([NativeUI]::IsWindowVisible($window)) 'Idle close could not restore'
    Assert-UI ([NativeUI]::IsWindowEnabled((Control $window 113))) 'Idle tray restore unexpectedly started sending'
    [void][NativeUI]::MoveWindow($window,20,20,820,820,$true)
    [NativeUI]::Screenshot($window,(Join-Path $PSScriptRoot 'native-ui-desktop.png'))
    [void][NativeUI]::MoveWindow($window,20,20,720,760,$true)
    foreach($option in @(@(1,320),@(2,416),@(4,512),@(3,640))) {
        Select-Option $window 119 $option[0]
        Assert-Centered $window $option[1]
    }
    Select-PrimaryMonitor $window
    Assert-Centered $window 640
    [NativeUI]::Screenshot($window,(Join-Path $PSScriptRoot 'native-ui-minimum.png'))
    foreach($entry in @(@(100,$sourceName),@(101,'80'),@(102,'80'),@(103,'640'),@(104,'360'),@(105,'240'))) {
        [void][NativeUI]::SetWindowText((Control $window $entry[0]),$entry[1])
    }
    Assert-UI ([NativeUI]::SendMessage((Control $window 119),0x147,[IntPtr]::Zero,[IntPtr]::Zero).ToInt64() -eq 0) 'Manual edit did not switch to custom'
    Select-Option $window 119 2
    Assert-Centered $window 416
    [void][NativeUI]::SendMessage((Control $window 106),0xf1,[IntPtr]::Zero,[IntPtr]::Zero)
    Command $window 106
    [void][NativeUI]::SendMessage((Control $window 120),0xf1,[IntPtr]1,[IntPtr]::Zero)
    Command $window 113
    Start-Sleep -Seconds 2
    Assert-UI (![NativeUI]::IsWindowEnabled((Control $window 113))) 'Start still enabled while sending'
    Assert-UI ([NativeUI]::IsWindowEnabled((Control $window 114))) 'Stop disabled while sending'
    Assert-UI (![NativeUI]::IsWindowEnabled((Control $window 119))) 'Center presets enabled while sending'
    Assert-UI (![NativeUI]::IsWindowEnabled((Control $window 120))) 'Precision scheduling enabled while sending'
    [NativeUI]::Screenshot($window,(Join-Path $PSScriptRoot 'native-ui-sending.png'))
    Assert-UI ([IO.File]::ReadAllText($ini).Contains($sourceName)) 'Unicode source settings lost'
    Assert-UI ([IO.File]::ReadAllText($ini).Contains('centerPreset=2')) 'Center preset was not saved'
    Assert-UI ([IO.File]::ReadAllText($ini).Contains('lowLatency=1')) 'Precision scheduling was not saved under the 2.4.3 key'
    Assert-UI ([NativeUI]::SendMessage($window,0x11,[IntPtr]::Zero,[IntPtr]::Zero).ToInt64() -eq 1) 'Session-end query was rejected'
    [void][NativeUI]::SendMessage($window,0x16,[IntPtr]::Zero,[IntPtr]::Zero)
    Assert-UI ([NativeUI]::IsWindowEnabled((Control $window 114))) 'Canceled session end stopped sending'
    Command $window 115
    Assert-UI (![NativeUI]::IsWindowVisible($window)) 'Tray hide failed'
    [void][NativeUI]::SendMessage($window,0x8001,[IntPtr]::Zero,[IntPtr]0x203)
    Assert-UI ([NativeUI]::IsWindowVisible($window)) 'Tray restore failed'
    Command $window 114
    Start-Sleep -Seconds 1
    Assert-UI ([NativeUI]::IsWindowEnabled((Control $window 113))) 'Stop did not complete'
    Assert-UI ([NativeUI]::IsWindowEnabled((Control $window 120))) 'Precision scheduling did not re-enable after stop'
    [void][NativeUI]::PostMessage($window,0x111,[IntPtr]112,[IntPtr]::Zero)
    Start-Sleep -Milliseconds 400
    $picker = [NativeUI]::FindWindow('CastRegionPicker',$null)
    Assert-UI ($picker -ne [IntPtr]::Zero) 'Picker missing'
    [void][NativeUI]::SendMessage($picker,0x201,[IntPtr]1,[IntPtr]((500 -shl 16) -bor 500))
    [void][NativeUI]::SendMessage($picker,0x200,[IntPtr]1,[IntPtr]((100 -shl 16) -bor 100))
    [void][NativeUI]::PostMessage($picker,0x202,[IntPtr]::Zero,[IntPtr]((100 -shl 16) -bor 100))
    Start-Sleep -Milliseconds 400
    Assert-UI ([NativeUI]::Text((Control $window 103)) -eq '400') 'Reverse picker width failed'
    Assert-UI ([NativeUI]::Text((Control $window 104)) -eq '400') 'Reverse picker height failed'
    Assert-UI ([NativeUI]::SendMessage((Control $window 119),0x147,[IntPtr]::Zero,[IntPtr]::Zero).ToInt64() -eq 0) 'Picker did not switch to custom'
    [void][NativeUI]::PostMessage($window,0x111,[IntPtr]112,[IntPtr]::Zero)
    Start-Sleep -Milliseconds 400
    $picker = [NativeUI]::FindWindow('CastRegionPicker',$null)
    Assert-UI ($picker -ne [IntPtr]::Zero) 'Second picker missing'
    [void][NativeUI]::PostMessage($picker,0x100,[IntPtr]27,[IntPtr]::Zero)
    Start-Sleep -Milliseconds 400
    Assert-UI ([NativeUI]::Text((Control $window 103)) -eq '400') 'Cancel changed region'
    Select-Option $window 119 4
    Command $window 113
    Start-Sleep -Seconds 1
    [void][NativeUI]::SendMessage($window,0x10,[IntPtr]::Zero,[IntPtr]::Zero)
    Start-Sleep -Milliseconds 400
    Assert-UI (![NativeUI]::IsWindowVisible($window)) 'Close during send did not hide to tray'
    Assert-UI (!$appProcess.HasExited) 'Close during send exited the app'
    [void][NativeUI]::SendMessage($window,0x8001,[IntPtr]::Zero,[IntPtr]0x203)
    Assert-UI ([NativeUI]::IsWindowVisible($window)) 'Close during send could not restore'
    Assert-UI (![NativeUI]::IsWindowEnabled((Control $window 113))) 'Close interrupted sending'
    Assert-UI ([NativeUI]::IsWindowEnabled((Control $window 114))) 'Stop unavailable after close and restore'
    Command $window 115
    Command $window 121
    Assert-UI ($appProcess.WaitForExit(5000)) 'Tray exit during send did not finish'
    Assert-UI ($appProcess.ExitCode -eq 0) 'Native GUI crashed'
    $appProcess = Start-Process -FilePath $exe -WindowStyle Hidden -PassThru
    $window=[IntPtr]::Zero
    for($attempt=0;$attempt -lt 100 -and $window -eq [IntPtr]::Zero;$attempt++) {
        Start-Sleep -Milliseconds 100; $window=[NativeUI]::FindWindow('ScreenCastSendCpp','ScreenCast Send C++')
    }
    Assert-UI ($window -ne [IntPtr]::Zero) 'Settings reload window missing'
    Assert-Centered $window 512
    Assert-UI ([NativeUI]::SendMessage((Control $window 119),0x147,[IntPtr]::Zero,[IntPtr]::Zero).ToInt64() -eq 4) 'Saved 512 preset did not reload'
    Assert-UI ([NativeUI]::SendMessage((Control $window 120),0xf0,[IntPtr]::Zero,[IntPtr]::Zero).ToInt64() -eq 1) 'Precision scheduling did not reload'
    Command $window 121
    Assert-UI ($appProcess.WaitForExit(5000)) 'Reloaded window did not exit'
    Write-Output 'PASS: 320/416/512/640 center presets, monitor recenter, custom edit/picker, saved settings reload, 240 FPS defaults, precision scheduling persistence and lockout, canceled session end, lifecycle, Unicode, tray close/restore/exit and cancel'
} finally {
    if($appProcess -and !$appProcess.HasExited) { $appProcess.Kill(); $appProcess.WaitForExit() }
    if($settingsTouched) {
        if($null -ne $original) { [IO.File]::WriteAllBytes($ini,$original) }
        elseif(Test-Path $ini) { Remove-Item -LiteralPath $ini }
    }
}
