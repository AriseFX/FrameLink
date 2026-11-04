use crate::model::PixelFormat;
#[cfg(any(windows, test))]
use crate::model::{Config, Transport};

#[derive(Clone, Debug, Default)]
pub struct SendOptions {
    fps: Option<u32>,
    pixel: Option<PixelFormat>,
    gpu_nv12: Option<bool>,
}

impl SendOptions {
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let mut options = Self::default();
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--conversion" => {
                    if options.gpu_nv12.is_some() {
                        return Err("--conversion 不能重复指定".into());
                    }
                    options.gpu_nv12 = Some(match args.next().map(String::as_str) {
                        Some("cpu") => false,
                        Some("gpu") => true,
                        _ => return Err("--conversion 需要 cpu 或 gpu".into()),
                    });
                }
                "--fps" => {
                    if options.fps.is_some() {
                        return Err("--fps 不能重复指定".into());
                    }
                    options.fps = Some(
                        args.next()
                            .and_then(|s| s.parse::<u32>().ok())
                            .filter(|fps| (1..=1000).contains(fps))
                            .ok_or("--fps 需要 1–1000 的整数")?,
                    );
                }
                "--follow-refresh" => {
                    return Err("跟随刷新率模式已移除，请使用 --fps 指定固定帧率".into());
                }
                "--pixel" => {
                    if options.pixel.is_some() {
                        return Err("--pixel 不能重复指定".into());
                    }
                    options.pixel = Some(match args.next().map(String::as_str) {
                        Some("nv12") => PixelFormat::Nv12,
                        Some("bgrx") => PixelFormat::Bgrx,
                        _ => return Err("--pixel 需要 nv12 或 bgrx".into()),
                    });
                }
                _ => return Err(format!("未知诊断参数：{arg}")),
            }
        }
        Ok(options)
    }

    #[cfg(any(windows, test))]
    pub(crate) fn apply(&self, config: &mut Config) -> Result<(), String> {
        if let Some(pixel) = self.pixel {
            if config.transport != Transport::Ndi {
                return Err("--pixel 仅适用于 NDI 发送诊断".into());
            }
            config.pixel = pixel;
        }
        if let Some(fps) = self.fps {
            config.fps = fps;
        }
        if let Some(gpu) = self.gpu_nv12 {
            if gpu && !config.supports_gpu_nv12() {
                return Err("GPU转换诊断需要NDI NV12、DXGI或自动采集，并关闭鼠标合成".into());
            }
            config.gpu_nv12 = gpu;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parse(args: &[&str]) -> Result<SendOptions, String> {
        SendOptions::parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }
    #[test]
    fn overrides_pacing_without_changing_unrelated_settings() {
        let saved = Config::default();
        let mut run = saved.clone();
        parse(&["--fps", "475", "--pixel", "bgrx"])
            .unwrap()
            .apply(&mut run)
            .unwrap();
        assert_eq!(run.fps, 475);
        assert_eq!(run.pixel, PixelFormat::Bgrx);
        assert_eq!(run.region, saved.region);
        assert_eq!(run.network_adapter, saved.network_adapter);
        parse(&["--fps", "120"]).unwrap().apply(&mut run).unwrap();
        assert_eq!(run.fps, 120);
        assert_eq!(saved.fps, 1000);
        assert_eq!(saved.pixel, PixelFormat::Nv12);
    }
    #[test]
    fn conversion_override_is_temporary_and_rejects_incompatible_gpu_requests() {
        let mut run = Config::default();
        parse(&["--conversion", "gpu"])
            .unwrap()
            .apply(&mut run)
            .unwrap();
        assert!(run.gpu_nv12);
        parse(&[]).unwrap().apply(&mut run).unwrap();
        assert!(run.gpu_nv12);
        parse(&["--conversion", "cpu"])
            .unwrap()
            .apply(&mut run)
            .unwrap();
        assert!(!run.gpu_nv12);
        run.cursor = true;
        assert!(
            parse(&["--conversion", "gpu"])
                .unwrap()
                .apply(&mut run)
                .is_err()
        );
        run.cursor = false;
        assert!(
            parse(&["--conversion", "gpu", "--pixel", "bgrx"])
                .unwrap()
                .apply(&mut run)
                .is_err()
        );
        assert!(parse(&["--conversion", "unknown"]).is_err());
        assert!(parse(&["--conversion"]).is_err());
        assert!(parse(&["--conversion", "cpu", "--conversion", "gpu"]).is_err());
    }
    #[test]
    fn rejects_ambiguous_or_invalid_overrides() {
        for args in [
            vec!["--fps"],
            vec!["--fps", "0"],
            vec!["--fps", "1001"],
            vec!["--fps", "240", "--follow-refresh"],
            vec!["--fps", "240", "--fps", "300"],
            vec!["--follow-refresh"],
            vec!["--pixel", "rgba"],
            vec!["--pixel", "nv12", "--pixel", "bgrx"],
            vec!["--unknown"],
        ] {
            assert!(parse(&args).is_err());
        }
        let mut jpeg = Config {
            transport: Transport::Tcp,
            ..Config::default()
        };
        assert!(
            parse(&["--pixel", "bgrx"])
                .unwrap()
                .apply(&mut jpeg)
                .is_err()
        );
    }
}
