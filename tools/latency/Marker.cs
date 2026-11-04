using System;
using System.Diagnostics;
using System.Drawing;
using System.Globalization;
using System.IO;
using System.Runtime.InteropServices;
using System.Windows.Forms;

sealed class Marker : Form {
    [DllImport("kernel32.dll")] static extern void GetSystemTimePreciseAsFileTime(out long ticks);
    [DllImport("user32.dll")] static extern bool SetProcessDpiAwarenessContext(IntPtr context);
    [DllImport("user32.dll")] static extern bool PeekMessage(out MessageNative msg, IntPtr window, uint min, uint max, uint remove);
    [DllImport("dwmapi.dll")] static extern int DwmFlush();
    [StructLayout(LayoutKind.Sequential)] struct MessageNative { public IntPtr window; public uint message; public IntPtr wparam; public IntPtr lparam; public uint time; public Point point; public uint extra; }
    readonly Stopwatch elapsed = Stopwatch.StartNew();
    readonly string report;
    readonly double seconds;
    readonly SolidBrush[] colors = new SolidBrush[256];
    int frames;
    bool painting;
    protected override bool ShowWithoutActivation { get { return true; } }
    protected override CreateParams CreateParams { get { var p = base.CreateParams; p.ExStyle |= 0x08000000; return p; } }

    Marker(int x, int y, int size, double duration, string output) {
        seconds = duration; report = output;
        Text = "FrameLink latency test - closes automatically";
        FormBorderStyle = FormBorderStyle.None;
        StartPosition = FormStartPosition.Manual;
        Bounds = new Rectangle(x, y, size, size);
        TopMost = true; ShowInTaskbar = true; DoubleBuffered = true;
        for (int i = 0; i < colors.Length; i++) colors[i] = new SolidBrush(Color.FromArgb(255, (i * 7) % 256, (i * 13) % 256, (i * 19) % 256));
        Application.Idle += OnIdle;
    }
    void OnIdle(object sender, EventArgs args) {
        MessageNative msg;
        while (!IsDisposed && !PeekMessage(out msg, IntPtr.Zero, 0, 0, 0)) {
            if (elapsed.Elapsed.TotalSeconds >= seconds) { Close(); return; }
            Invalidate(); Update(); DwmFlush();
        }
    }
    protected override void OnPaint(PaintEventArgs e) {
        if (painting) return;
        painting = true;
        long fileTicks; GetSystemTimePreciseAsFileTime(out fileTicks);
        ulong ticks = (ulong)(fileTicks - 116444736000000000L);
        var g = e.Graphics;
        for (int y = 0; y < Height; y += 32)
            for (int x = 0; x < Width; x += 32)
                g.FillRectangle(colors[((x / 32) * 17 + (y / 32) * 31 + frames * 3) & 255], x, y, 32, 32);
        for (int bit = 0; bit < 64; bit++) {
            bool value = ((ticks >> bit) & 1) != 0;
            int x = 32 + (bit % 16) * 24, y = 32 + (bit / 16) * 24;
            g.FillRectangle(value ? Brushes.White : Brushes.Black, x, y, 24, 24);
            g.FillRectangle(value ? Brushes.Black : Brushes.White, x, y + 128, 24, 24);
        }
        for (int bit = 0; bit < 16; bit++)
            g.FillRectangle(((0xA55A >> bit) & 1) != 0 ? Brushes.White : Brushes.Black, 32 + bit * 24, 300, 24, 24);
        g.FillRectangle(Brushes.Black, 24, 360, Width - 48, 110);
        g.DrawString("FrameLink NDI latency test", SystemFonts.CaptionFont, Brushes.White, 36, 374);
        g.DrawString("Temporary test pattern - closes automatically", SystemFonts.CaptionFont, Brushes.White, 36, 402);
        g.DrawString("Frame " + frames.ToString() + "   " + Math.Max(0, seconds - elapsed.Elapsed.TotalSeconds).ToString("F0") + " s", SystemFonts.CaptionFont, Brushes.White, 36, 430);
        frames++;
        painting = false;
    }
    protected override void OnFormClosed(FormClosedEventArgs e) {
        Application.Idle -= OnIdle;
        string json = string.Format(CultureInfo.InvariantCulture, "{{\"scope\":\"render_start_marker\",\"frames\":{0},\"seconds\":{1:F6},\"render_fps\":{2:F3},\"x\":{3},\"y\":{4},\"size\":{5}}}", frames, elapsed.Elapsed.TotalSeconds, frames / elapsed.Elapsed.TotalSeconds, Left, Top, Width);
        File.WriteAllText(report, json);
        foreach (var brush in colors) brush.Dispose();
        base.OnFormClosed(e);
    }
    [STAThread] static void Main(string[] args) {
        if (args.Length != 5) throw new ArgumentException("Usage: Marker <x> <y> <size>=512 <seconds:1-60> <report.json>");
        int size = int.Parse(args[2], CultureInfo.InvariantCulture);
        double duration = double.Parse(args[3], CultureInfo.InvariantCulture);
        if (size < 512 || size > 2160 || double.IsNaN(duration) || double.IsInfinity(duration) || duration < 1 || duration > 60)
            throw new ArgumentException("Invalid marker size or duration");
        if (File.Exists(args[4])) throw new IOException("Report already exists");
        string parent = Path.GetDirectoryName(Path.GetFullPath(args[4]));
        Directory.CreateDirectory(parent);
        SetProcessDpiAwarenessContext(new IntPtr(-4));
        Application.EnableVisualStyles();
        Application.Run(new Marker(int.Parse(args[0]), int.Parse(args[1]), size, duration, args[4]));
    }
}
