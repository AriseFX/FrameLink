#[cfg(any(windows, test))]
mod config {
    use crate::network_adapters::Adapter;
    use serde_json::{Map, Value};
    use std::{ffi::OsStr, net::Ipv4Addr, process::Command};

    pub(super) fn adapter_ips(id: &str, adapters: &[Adapter]) -> Result<Vec<Ipv4Addr>, String> {
        let adapter = adapters
            .iter()
            .find(|adapter| adapter.id == id)
            .ok_or("所选网卡不存在，请重新选择网卡或使用自动选择")?;
        if !adapter.connected {
            return Err(format!("网卡“{}”未连接，请连接后重启应用", adapter.name));
        }
        let mut ips = adapter.ipv4.clone();
        ips.sort_unstable();
        ips.dedup();
        if ips.is_empty() {
            return Err(format!("网卡“{}”没有可用的 IPv4 地址", adapter.name));
        }
        Ok(ips)
    }

    pub(super) fn check_ips(applied: &[Ipv4Addr], live: &[Ipv4Addr]) -> Result<(), String> {
        if applied == live {
            Ok(())
        } else {
            Err("所选网卡的 IP 地址已变化，请重启应用后发送".into())
        }
    }

    pub(super) fn merge(base: Option<&[u8]>, ips: &[Ipv4Addr]) -> Result<Vec<u8>, String> {
        let mut document = match base {
            Some(bytes) => {
                serde_json::from_slice(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes))
                    .map_err(|error| format!("NDI 配置不是有效的 JSON：{error}"))?
            }
            None => Value::Object(Map::new()),
        };
        let root = document.as_object_mut().ok_or("NDI 配置根节点必须是对象")?;
        let ndi = root
            .entry("ndi")
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .ok_or("NDI 配置的 ndi 字段必须是对象")?;
        let adapters = ndi
            .entry("adapters")
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .ok_or("NDI 配置的 adapters 字段必须是对象")?;
        adapters.insert(
            "allowed".into(),
            Value::Array(ips.iter().map(|ip| Value::String(ip.to_string())).collect()),
        );
        serde_json::to_vec_pretty(&document).map_err(|error| error.to_string())
    }

    pub(super) fn restore_environment(command: &mut Command, original: Option<&OsStr>) {
        if let Some(value) = original {
            command.env("NDI_CONFIG_DIR", value);
        } else {
            command.env_remove("NDI_CONFIG_DIR");
        }
    }
}

#[cfg(windows)]
pub use runtime::{
    ensure_ready, initialize, request_restart, restart, restart_requested, restart_required,
    startup_error, validate_selection,
};

#[cfg(windows)]
mod runtime {
    use super::config;
    use std::{
        ffi::OsString,
        fs,
        io::ErrorKind,
        net::Ipv4Addr,
        path::PathBuf,
        process::Command,
        sync::{
            OnceLock,
            atomic::{AtomicBool, Ordering},
        },
    };
    use windows::{
        Win32::{
            Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS},
            System::Registry::{
                HKEY_LOCAL_MACHINE, RRF_RT_ANY, RRF_SUBKEY_WOW6464KEY, RegGetValueW,
            },
        },
        core::w,
    };

    struct State {
        original_config_dir: Option<OsString>,
        adapter_id: String,
        ips: Vec<Ipv4Addr>,
        error: Option<String>,
    }

    static STATE: OnceLock<State> = OnceLock::new();
    static RESTART: AtomicBool = AtomicBool::new(false);

    /// Called once before loading NDI. A failed manual selection never falls back to auto.
    pub fn initialize(adapter_id: &str) {
        STATE.get_or_init(|| {
            let original_config_dir = std::env::var_os("NDI_CONFIG_DIR");
            let result = if adapter_id.is_empty() {
                Ok(Vec::new())
            } else {
                configure(adapter_id, original_config_dir.as_ref())
            };
            let (ips, error) = match result {
                Ok(ips) => (ips, None),
                Err(error) => (Vec::new(), Some(error)),
            };
            State {
                original_config_dir,
                adapter_id: adapter_id.into(),
                ips,
                error,
            }
        });
    }

    pub fn startup_error() -> Option<String> {
        STATE.get().and_then(|state| state.error.clone())
    }

    pub fn restart_required(
        adapter_id: &str,
        adapters: &[crate::network_adapters::Adapter],
    ) -> bool {
        STATE.get().is_none_or(|state| {
            state.error.is_some()
                || state.adapter_id != adapter_id
                || (!adapter_id.is_empty()
                    && config::adapter_ips(adapter_id, adapters)
                        .and_then(|ips| config::check_ips(&state.ips, &ips))
                        .is_err())
        })
    }

    pub fn ensure_ready() -> Result<(), String> {
        let state = STATE.get().ok_or("网络设置尚未初始化")?;
        match &state.error {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }

    pub fn validate_selection(adapter_id: &str) -> Result<(), String> {
        let state = STATE.get().ok_or("网络设置尚未初始化")?;
        if state.adapter_id != adapter_id {
            return Err("网卡设置已修改，请重启应用后发送".into());
        }
        ensure_ready()?;
        if !adapter_id.is_empty() {
            let ips = config::adapter_ips(adapter_id, &crate::network_adapters::enumerate()?)?;
            config::check_ips(&state.ips, &ips)?;
        }
        Ok(())
    }

    pub fn request_restart() {
        RESTART.store(true, Ordering::Release);
    }

    pub fn restart_requested() -> bool {
        RESTART.load(Ordering::Acquire)
    }

    /// The caller must release the single-instance guard before spawning the replacement.
    pub fn restart() -> Result<(), String> {
        let state = STATE.get().ok_or("网络设置尚未初始化")?;
        let executable = std::env::current_exe().map_err(|error| error.to_string())?;
        let mut command = Command::new(executable);
        config::restore_environment(&mut command, state.original_config_dir.as_deref());
        command
            .spawn()
            .map_err(|error| format!("无法重启应用：{error}"))?;
        Ok(())
    }

    fn configure(
        adapter_id: &str,
        original_dir: Option<&OsString>,
    ) -> Result<Vec<Ipv4Addr>, String> {
        if administrator_controls_adapters()? {
            return Err("网卡由系统 NDI 设置控制，请选择“自动选择”并重启应用".into());
        }
        let ips = config::adapter_ips(adapter_id, &crate::network_adapters::enumerate()?)?;
        let base_dir = match original_dir {
            Some(directory) if directory.is_empty() => {
                return Err("NDI_CONFIG_DIR 为空，无法保留原有 NDI 配置".into());
            }
            Some(directory) => PathBuf::from(directory),
            None => environment_directory("ProgramData")?.join("NDI"),
        };
        let base_path = base_dir.join("ndi-config.v1.json");
        let base = match fs::read(&base_path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == ErrorKind::NotFound => None,
            Err(error) => {
                return Err(format!(
                    "无法读取 NDI 配置 {}：{error}",
                    base_path.display()
                ));
            }
        };
        let merged = config::merge(base.as_deref(), &ips)?;
        let directory = environment_directory("LOCALAPPDATA")?.join("FrameLink/ndi");
        fs::create_dir_all(&directory).map_err(|error| format!("无法创建网络配置目录：{error}"))?;
        let destination = directory.join("ndi-config.v1.json");
        let temporary = directory.join(format!("ndi-config.v1.{}.tmp", std::process::id()));
        let result =
            fs::write(&temporary, merged).and_then(|()| fs::rename(&temporary, destination));
        if let Err(error) = result {
            let _ = fs::remove_file(&temporary);
            return Err(format!("无法保存网络配置：{error}"));
        }
        // SAFETY: This module only compiles on Windows, where set_var is thread-safe.
        // initialize is called before NDI is loaded; subsequent changes require a new process.
        unsafe { std::env::set_var("NDI_CONFIG_DIR", directory) };
        Ok(ips)
    }

    fn environment_directory(name: &str) -> Result<PathBuf, String> {
        let directory = std::env::var_os(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| format!("缺少系统目录 {name}"))?;
        if !directory.is_absolute() {
            return Err(format!("系统目录 {name} 不是绝对路径"));
        }
        Ok(directory)
    }

    fn administrator_controls_adapters() -> Result<bool, String> {
        // No data buffer is needed: presence is sufficient, including an empty string.
        // SAFETY: All pointers are static UTF-16 strings; output pointers are absent.
        let result = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                w!("SOFTWARE\\NDI\\Adapters"),
                w!("Allowed"),
                RRF_RT_ANY | RRF_SUBKEY_WOW6464KEY,
                None,
                None,
                None,
            )
        };
        if result == ERROR_SUCCESS {
            Ok(true)
        } else if result == ERROR_FILE_NOT_FOUND || result == ERROR_PATH_NOT_FOUND {
            Ok(false)
        } else {
            Err(format!(
                "无法检查系统 NDI 网卡设置：Windows 错误 {}",
                result.0
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::config;
    use crate::network_adapters::Adapter;
    use serde_json::{Value, json};
    use std::{ffi::OsStr, net::Ipv4Addr, process::Command};

    fn adapter() -> Adapter {
        Adapter {
            id: "stable-guid".into(),
            name: "Ethernet".into(),
            ipv4: vec![Ipv4Addr::new(192, 168, 1, 20)],
            link_speed_bps: 1_000_000_000,
            connected: true,
            kind: "以太网".into(),
        }
    }

    #[test]
    fn preserves_existing_discovery_groups_transport_and_unknown_fields() {
        let original = json!({
            "other": {"keep": 1},
            "ndi": {
                "networks": {"discovery": "192.168.1.2", "ips": "192.168.1.3"},
                "groups": {"send": "Private"},
                "rudp": {"send": {"enable": false}},
                "adapters": {"allowed": ["10.0.0.5"], "extra": 7}
            }
        });
        let bytes = serde_json::to_vec(&original).unwrap();
        let output = config::merge(Some(&bytes), &adapter().ipv4).unwrap();
        let mut expected = original;
        expected["ndi"]["adapters"]["allowed"] = json!(["192.168.1.20"]);
        assert_eq!(serde_json::from_slice::<Value>(&output).unwrap(), expected);
    }

    #[test]
    fn missing_config_is_valid_but_malformed_existing_config_is_not_discarded() {
        let output = config::merge(None, &adapter().ipv4).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&output).unwrap(),
            json!({
                "ndi": {"adapters": {"allowed": ["192.168.1.20"]}}
            })
        );
        for input in [
            "",
            "{",
            "[]",
            r#"{"ndi":false}"#,
            r#"{"ndi":{"adapters":null}}"#,
        ] {
            assert!(config::merge(Some(input.as_bytes()), &adapter().ipv4).is_err());
        }
        assert!(config::merge(Some(b"\xef\xbb\xbf{}"), &adapter().ipv4).is_ok());
    }

    #[test]
    fn stable_id_resolves_current_dhcp_addresses_and_rejects_unavailable_nic() {
        let mut nic = adapter();
        let original_ips = config::adapter_ips(&nic.id, std::slice::from_ref(&nic)).unwrap();
        nic.ipv4 = vec![Ipv4Addr::new(192, 168, 1, 21)];
        let updated = config::adapter_ips(&nic.id, std::slice::from_ref(&nic)).unwrap();
        assert_eq!(updated, nic.ipv4);
        assert!(config::check_ips(&original_ips, &updated).is_err());
        assert!(config::adapter_ips(&nic.id, &[]).is_err());
        nic.connected = false;
        assert!(config::adapter_ips(&nic.id, std::slice::from_ref(&nic)).is_err());
        nic.connected = true;
        nic.ipv4.clear();
        assert!(config::adapter_ips(&nic.id, std::slice::from_ref(&nic)).is_err());
    }

    #[test]
    fn address_order_or_duplicates_do_not_require_restart() {
        let mut nic = adapter();
        nic.ipv4.push(Ipv4Addr::new(10, 0, 0, 20));
        let applied = config::adapter_ips(&nic.id, std::slice::from_ref(&nic)).unwrap();
        nic.ipv4.reverse();
        nic.ipv4.push(nic.ipv4[0]);
        let live = config::adapter_ips(&nic.id, std::slice::from_ref(&nic)).unwrap();
        assert!(config::check_ips(&applied, &live).is_ok());
    }

    #[test]
    fn restart_restores_external_config_or_removes_our_override() {
        for original in [None, Some(OsStr::new("custom-ndi"))] {
            let mut command = Command::new("FrameLink");
            command.env("NDI_CONFIG_DIR", "our-generated-config");
            config::restore_environment(&mut command, original);
            assert_eq!(
                command.get_envs().collect::<Vec<_>>(),
                vec![(OsStr::new("NDI_CONFIG_DIR"), original)]
            );
        }
    }
}
