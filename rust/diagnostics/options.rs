use crate::model::PixelFormat;
#[cfg(any(windows, test))]
use crate::model::{Config, Transport};

#[derive(Clone, Copy, Debug)]
enum Pacing {
    Fixed(u32),
    FollowRefresh,
}

#[derive(Clone, Debug, Default)]
pub struct SendOptions {
    pacing: Option<Pacing>,
    pixel: Option<PixelFormat>,
}

impl SendOptions {
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let mut options = Self::default();
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--fps" | "--follow-refresh" => {
                    if options.pacing.is_some() {
                        return Err("--fps 与 --follow-refresh 只能指定其中一项，且不能重复".into());
                    }
                    options.pacing = Some(if arg == "--fps" {
                        let fps = args
                            .next()
                            .and_then(|s| s.parse::<u32>().ok())
                            .filter(|fps| (1..=1000).contains(fps))
                            .ok_or("--fps 需要 1–1000 的整数")?;
                        Pacing::Fixed(fps)
                    } else {
                        Pacing::FollowRefresh
                    });
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
        match self.pacing {
            Some(Pacing::Fixed(fps)) => {
                config.fps = fps;
                config.unlimited = false;
            }
            Some(Pacing::FollowRefresh) => config.unlimited = true,
            None => {}
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
        parse(&["--follow-refresh", "--pixel", "bgrx"])
            .unwrap()
            .apply(&mut run)
            .unwrap();
        assert!(run.unlimited);
        assert_eq!(run.pixel, PixelFormat::Bgrx);
        assert_eq!(run.region, saved.region);
        assert_eq!(run.network_adapter, saved.network_adapter);
        parse(&["--fps", "120"]).unwrap().apply(&mut run).unwrap();
        assert!(!run.unlimited);
        assert_eq!(run.fps, 120);
        assert!(!saved.unlimited);
        assert_eq!(saved.pixel, PixelFormat::Nv12);
    }
    #[test]
    fn rejects_ambiguous_or_invalid_overrides() {
        for args in [
            vec!["--fps"],
            vec!["--fps", "0"],
            vec!["--fps", "1001"],
            vec!["--fps", "240", "--follow-refresh"],
            vec!["--follow-refresh", "--follow-refresh"],
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
