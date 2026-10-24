use crate::model::{Backend, Config, PixelFormat};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

pub fn path() -> PathBuf {
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        PathBuf::from(local).join("FrameLink/settings.toml")
    } else if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".config/FrameLink/settings.toml")
    } else {
        PathBuf::from("settings.toml")
    }
}

pub fn load() -> (Config, Option<String>) {
    let destination = path();
    if destination.exists() {
        return match fs::read_to_string(&destination)
            .map_err(|e| e.to_string())
            .and_then(|text| parse(&text))
        {
            Ok(config) => (config, None),
            Err(error) => (
                Config::default(),
                Some(format!("设置读取失败，已使用默认配置：{error}")),
            ),
        };
    }
    let previous = if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        Some(PathBuf::from(local).join("ScreenCastSendRust/settings.toml"))
    } else {
        std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join(".config/ScreenCastSendRust/settings.toml"))
    };
    if let Some(previous) = previous.filter(|path| path.exists()) {
        return match fs::read_to_string(previous)
            .map_err(|e| e.to_string())
            .and_then(|text| parse(&text))
        {
            Ok(config) => (config, Some("已导入上一版推流设置；原配置文件保留".into())),
            Err(error) => (Config::default(), Some(format!("旧设置无法导入：{error}"))),
        };
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let legacy = PathBuf::from(local).join("ScreenCastSendCpp/settings.ini");
        if legacy.exists() {
            return match fs::read(legacy)
                .map_err(|e| e.to_string())
                .and_then(|bytes| import_ini(&bytes))
            {
                Ok(config) => (config, Some("已导入上一版设置；原配置文件保留".into())),
                Err(error) => (Config::default(), Some(format!("旧设置无法导入：{error}"))),
            };
        }
    }
    (Config::default(), None)
}

fn parse(text: &str) -> Result<Config, String> {
    let config: Config = toml::from_str(text).map_err(|e| e.to_string())?;
    config.validate()?;
    Ok(config)
}

pub fn save(config: &Config) -> Result<(), String> {
    save_to(&path(), config)
}

fn save_to(destination: &Path, config: &Config) -> Result<(), String> {
    config.validate()?;
    let text = toml::to_string_pretty(config).map_err(|e| e.to_string())?;
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let temporary = destination.with_extension("toml.tmp");
    fs::write(&temporary, text).map_err(|e| e.to_string())?;
    // std::fs::rename atomically replaces an existing file on Windows and Unix.
    fs::rename(&temporary, destination).map_err(|e| format!("无法保存设置：{e}"))
}

pub fn import_ini(bytes: &[u8]) -> Result<Config, String> {
    let text = if bytes.starts_with(&[0xff, 0xfe]) {
        if !bytes.len().is_multiple_of(2) {
            return Err("UTF-16配置文件长度不完整".into());
        }
        let words: Vec<_> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        String::from_utf16(&words).map_err(|e| e.to_string())?
    } else {
        String::from_utf8(bytes.to_vec()).map_err(|e| e.to_string())?
    };
    let mut capture = false;
    let mut values = HashMap::new();
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            capture = line.eq_ignore_ascii_case("[capture]");
            continue;
        }
        if capture
            && !line.starts_with([';', '#'])
            && let Some((key, value)) = line.split_once('=')
        {
            values.insert(key.trim().to_lowercase(), value.trim().to_owned());
        }
    }
    let integer = |name: &str, fallback: i32| -> Result<i32, String> {
        values.get(name).map_or(Ok(fallback), |value| {
            value
                .parse()
                .map_err(|_| format!("旧配置字段{name}不是数字"))
        })
    };
    let mut c = Config::default();
    if let Some(name) = values.get("name") {
        c.name = name.clone();
    }
    c.monitor = values.get("monitor").cloned().unwrap_or_default();
    c.region.x = integer("x", c.region.x)?;
    c.region.y = integer("y", c.region.y)?;
    c.region.width =
        u32::try_from(integer("width", c.region.width as i32)?).map_err(|_| "旧配置宽度为负数")?;
    c.region.height = u32::try_from(integer("height", c.region.height as i32)?)
        .map_err(|_| "旧配置高度为负数")?;
    c.fps = u32::try_from(integer("fps", c.fps as i32)?).map_err(|_| "旧配置帧率为负数")?;
    c.cursor = integer("cursor", 0)? != 0;
    c.unlimited = integer("unlimited", 0)? != 0;
    c.precision = if values.contains_key("lowlatency") {
        integer("lowlatency", 0)? != 0
    } else {
        integer("precision", 0)? != 0
    };
    c.backend = match integer("backend", 0)? {
        0 => Backend::Auto,
        1 => Backend::Dxgi,
        2 => Backend::Gdi,
        _ => return Err("未知旧采集方式".into()),
    };
    c.pixel = match integer("pixel", 1)? {
        0 => PixelFormat::Bgrx,
        1 => PixelFormat::Nv12,
        _ => return Err("未知旧像素格式".into()),
    };
    c.preset = match integer("centerpreset", 0)? {
        1 => Some(320),
        2 => Some(416),
        3 => Some(640),
        4 => Some(512),
        _ => None,
    };
    c.validate()?;
    Ok(c)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn imports_unicode_and_original_keys() {
        let text = "[capture]\r\nname=测试源\r\nx=-1000\r\ny=80\r\nwidth=640\r\nheight=640\r\nfps=240\r\nlowLatency=1\r\nprecision=0\r\ncenterPreset=3\r\npixel=1\r\n";
        let bytes: Vec<_> = [0xfeffu16]
            .into_iter()
            .chain(text.encode_utf16())
            .flat_map(u16::to_le_bytes)
            .collect();
        let c = import_ini(&bytes).unwrap();
        assert_eq!(c.name, "测试源");
        assert_eq!(c.region.x, -1000);
        assert!(c.precision);
        assert_eq!(c.preset, Some(640));
    }
    #[test]
    fn rejects_corrupt_or_unsafe_settings() {
        assert!(import_ini(&[0xff, 0xfe, 0x11]).is_err());
        assert!(import_ini(b"[capture]\nwidth=-8").is_err());
        assert!(import_ini(b"[capture]\nwidth=513").is_err());
        assert!(parse("fps = 0").is_err());
    }
    #[test]
    fn round_trip_preserves_preferences() {
        let c = Config {
            preview: true,
            precision: true,
            network_adapter: "{stable-adapter-id}".into(),
            ..Config::default()
        };
        let text = toml::to_string(&c).unwrap();
        let restored = parse(&text).unwrap();
        assert!(restored.preview && restored.precision);
        assert_eq!(restored.region, c.region);
        assert_eq!(restored.network_adapter, c.network_adapter);
        // Existing installations have no adapter setting and continue to follow NDI.
        assert!(parse("fps = 240").unwrap().network_adapter.is_empty());
    }
    #[test]
    fn custom_region_does_not_become_a_center_preset_on_restart() {
        let mut c = Config {
            preset: None,
            ..Config::default()
        };
        c.region.x = 123;
        c.region.y = -456;
        let restored = parse(&toml::to_string(&c).unwrap()).unwrap();
        assert_eq!(restored.preset, None);
        assert_eq!(restored.region, c.region);
    }
}
