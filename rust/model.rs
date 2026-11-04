use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Backend {
    #[default]
    Auto,
    Dxgi,
    Gdi,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PixelFormat {
    #[default]
    Nv12,
    Bgrx,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Transport {
    #[default]
    Ndi,
    Tcp,
    Udp,
}
impl Transport {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ndi => "NDI",
            Self::Tcp => "TCP",
            Self::Udp => "UDP",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Region {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}
impl Region {
    pub fn centered(monitor: &Monitor, size: u32) -> Result<Self, String> {
        if size < 2 || size > monitor.width || size > monitor.height {
            return Err("所选区域超出显示器，请选较小尺寸".into());
        }
        Ok(Self {
            x: monitor.x + ((monitor.width - size) / 2) as i32,
            y: monitor.y + ((monitor.height - size) / 2) as i32,
            width: size,
            height: size,
        })
    }
    pub fn fits(&self, monitor: &Monitor) -> bool {
        i64::from(self.x) >= i64::from(monitor.x)
            && i64::from(self.y) >= i64::from(monitor.y)
            && i64::from(self.x) + i64::from(self.width)
                <= i64::from(monitor.x) + i64::from(monitor.width)
            && i64::from(self.y) + i64::from(self.height)
                <= i64::from(monitor.y) + i64::from(monitor.height)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Monitor {
    pub id: String,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub refresh_hz: u32,
    pub primary: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub name: String,
    pub transport: Transport,
    /// Literal IP:port; no DNS lookup on the capture thread.
    pub destination: String,
    pub send_timeout_ms: u32,
    pub jpeg_quality: u8,
    pub monitor: String,
    /// Empty follows existing NDI settings; otherwise a stable Windows adapter ID.
    pub network_adapter: String,
    pub region: Region,
    #[serde(default)]
    pub preset: Option<u32>,
    pub fps: u32,
    pub backend: Backend,
    pub pixel: PixelFormat,
    pub cursor: bool,
    pub precision: bool,
    pub preview: bool,
    pub close_to_tray: bool,
    #[serde(skip)]
    pub diagnostics: bool,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            name: crate::brand::APP_ID.into(),
            transport: Transport::Ndi,
            destination: "127.0.0.1:7878".into(),
            send_timeout_ms: 20,
            jpeg_quality: 75,
            monitor: String::new(),
            network_adapter: String::new(),
            region: Region {
                x: 0,
                y: 0,
                width: 512,
                height: 512,
            },
            preset: Some(512),
            fps: 300,
            backend: Backend::Auto,
            pixel: PixelFormat::Nv12,
            cursor: false,
            precision: false,
            preview: false,
            close_to_tray: true,
            diagnostics: false,
        }
    }
}
impl Config {
    pub fn validate(&self) -> Result<(), String> {
        if self.transport != Transport::Ndi {
            let address: std::net::SocketAddr = self
                .destination
                .trim()
                .parse()
                .map_err(|_| "目标地址需为 IP:端口，例如 192.168.1.20:7878 或 [::1]:7878")?;
            if address.port() == 0
                || address.ip().is_unspecified()
                || address.ip().is_multicast()
                || address.ip() == std::net::Ipv4Addr::BROADCAST
            {
                return Err("目标地址需为有效的单播 IP 和非零端口".into());
            }
            if !(1..=100).contains(&self.jpeg_quality) {
                return Err("JPEG 质量范围是 1–100".into());
            }
            if !(1..=1000).contains(&self.send_timeout_ms) {
                return Err("发送超时范围是 1–1000 ms".into());
            }
        }
        let r = self.region;
        if self.name.trim().is_empty()
            || self.name.len() > 180
            || self
                .name
                .chars()
                .any(|c| "\\/:*?\"<>|".contains(c) || c.is_control())
        {
            return Err("源名称不能为空、超过180字节或含有特殊字符".into());
        }
        if !(2..=32768).contains(&r.width)
            || !(2..=32768).contains(&r.height)
            || !r.width.is_multiple_of(2)
            || u64::from(r.width) * u64::from(r.height) > 33_177_600
        {
            return Err("宽度需为偶数，尺寸至少2像素，总像素不能超过33177600".into());
        }
        if self.capture_pixel() == PixelFormat::Nv12 && !r.height.is_multiple_of(2) {
            return Err("NV12的高度需为偶数".into());
        }
        if !(1..=1000).contains(&self.fps) {
            return Err("固定帧率范围是1–1000 fps".into());
        }
        if !(-100_000..=100_000).contains(&r.x) || !(-100_000..=100_000).contains(&r.y) {
            return Err("采集坐标超出支持范围".into());
        }
        Ok(())
    }
    pub fn capture_pixel(&self) -> PixelFormat {
        if self.transport == Transport::Ndi {
            self.pixel
        } else {
            PixelFormat::Bgrx
        }
    }
    pub fn select_monitor(&mut self, monitor: &Monitor) -> Result<(), String> {
        let region = if let Some(size) = self.preset {
            Region::centered(monitor, size)?
        } else {
            self.region
        };
        self.monitor = monitor.id.clone();
        self.region = region;
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub enum Phase {
    #[default]
    Idle,
    Starting,
    Sending,
    Recovering,
    Stopping,
    Error,
}
#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub phase: Phase,
    pub source: String,
    pub backend: String,
    pub fps: f64,
    pub updates_fps: f64,
    pub receivers: i32,
    pub capture_ms: f64,
    pub conversion_ms: f64,
    pub submit_ms: f64,
    pub wait_ms: f64,
    pub frames: u64,
    pub repeats: u64,
    pub dropped: u64,
    pub uptime_sec: f64,
    pub error: Option<String>,
    pub ndi_version: String,
    pub timings: Vec<crate::diagnostics::FrameTiming>,
}
#[derive(Clone, Debug)]
pub struct Preview {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
    pub sequence: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn monitor() -> Monitor {
        Monitor {
            id: "secondary".into(),
            name: "副屏".into(),
            x: -2560,
            y: -1440,
            width: 2560,
            height: 1440,
            refresh_hz: 240,
            primary: false,
        }
    }
    #[test]
    fn centers_negative_monitor_and_preserves_size() {
        for size in [320, 416, 512, 640] {
            let r = Region::centered(&monitor(), size).unwrap();
            assert_eq!(r.x, -2560 + ((2560 - size) / 2) as i32);
            assert!(r.fits(&monitor()));
            assert_eq!((r.width, r.height), (size, size));
        }
        assert!(Region::centered(&monitor(), 2048).is_err());
    }
    #[test]
    fn rejects_bad_input_before_allocating() {
        let mut c = Config::default();
        assert!(c.validate().is_ok());
        c.region.width = 513;
        assert!(c.validate().is_err());
        c.region.width = 512;
        c.region.height = 511;
        assert!(c.validate().is_err());
        c.pixel = PixelFormat::Bgrx;
        assert!(c.validate().is_ok());
        c.fps = 0;
        assert!(c.validate().is_err());
        c.fps = 240;
        c.name = "bad\0name".into();
        assert!(c.validate().is_err());
    }
}
