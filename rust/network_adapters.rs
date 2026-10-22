use std::net::Ipv4Addr;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Adapter {
    /// Windows AdapterName stays stable when DHCP assigns a different address.
    pub id: String,
    pub name: String,
    pub ipv4: Vec<Ipv4Addr>,
    /// Negotiated transmit link rate, not measured application throughput.
    pub link_speed_bps: u64,
    pub connected: bool,
    pub kind: String,
}

#[cfg(not(windows))]
pub fn enumerate() -> Result<Vec<Adapter>, String> {
    Ok(Vec::new())
}

#[cfg(windows)]
pub use windows_adapters::enumerate;

#[cfg(windows)]
mod windows_adapters {
    use super::Adapter;
    use std::{collections::HashSet, mem, net::Ipv4Addr};
    use windows::Win32::{
        Foundation::{ERROR_BUFFER_OVERFLOW, ERROR_NO_DATA, NO_ERROR},
        NetworkManagement::{
            IpHelper::{
                GAA_FLAG_INCLUDE_ALL_INTERFACES, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER,
                GAA_FLAG_SKIP_MULTICAST, GetAdaptersAddresses, IF_TYPE_ETHERNET_CSMACD,
                IF_TYPE_IEEE80211, IF_TYPE_SOFTWARE_LOOPBACK, IF_TYPE_TUNNEL,
                IP_ADAPTER_ADDRESSES_LH, IP_ADAPTER_UNICAST_ADDRESS_LH,
            },
            Ndis::IfOperStatusUp,
        },
        Networking::WinSock::{AF_INET, IpDadStateDeprecated, IpDadStatePreferred, SOCKADDR_IN},
    };

    // GetAdaptersAddresses writes structures and strings into one caller-owned buffer.
    // u64 storage satisfies the alignment of the Windows adapter structures.
    struct Buffer(Vec<u64>);

    impl Buffer {
        fn new(bytes: u32) -> Self {
            assert!(mem::align_of::<IP_ADAPTER_ADDRESSES_LH>() <= mem::align_of::<u64>());
            Self(vec![0; (bytes as usize).div_ceil(mem::size_of::<u64>())])
        }

        fn remaining(&self, ptr: *const u8) -> Option<usize> {
            let offset = (ptr as usize).checked_sub(self.0.as_ptr() as usize)?;
            mem::size_of_val(self.0.as_slice()).checked_sub(offset)
        }

        // Only called with Win32 POD structs whose bit patterns are all valid.
        unsafe fn read<T: Copy>(&self, ptr: *const T) -> Result<T, String> {
            if self.remaining(ptr.cast()).unwrap_or(0) < mem::size_of::<T>() {
                return Err("Windows 返回了无效的网卡数据".into());
            }
            // SAFETY: The range is inside the initialized buffer; callers use POD structs.
            Ok(unsafe { ptr.read_unaligned() })
        }

        fn ansi(&self, ptr: *const u8) -> Result<String, String> {
            if ptr.is_null() {
                return Ok(String::new());
            }
            let remaining = self.remaining(ptr).ok_or("Windows 返回了无效的网卡名称")?;
            // SAFETY: The pointer and length have been checked against the owned buffer.
            let bytes = unsafe { std::slice::from_raw_parts(ptr, remaining) };
            let end = bytes
                .iter()
                .position(|byte| *byte == 0)
                .ok_or("Windows 返回了未终止的网卡名称")?;
            Ok(String::from_utf8_lossy(&bytes[..end]).into_owned())
        }

        fn wide(&self, ptr: *const u16) -> Result<String, String> {
            if ptr.is_null() {
                return Ok(String::new());
            }
            let remaining = self
                .remaining(ptr.cast())
                .ok_or("Windows 返回了无效的网卡名称")?
                / mem::size_of::<u16>();
            let mut units = Vec::new();
            for index in 0..remaining {
                // SAFETY: Each unit is inside the owned buffer; alignment is not assumed.
                let unit = unsafe { ptr.add(index).read_unaligned() };
                if unit == 0 {
                    return Ok(String::from_utf16_lossy(&units));
                }
                units.push(unit);
            }
            Err("Windows 返回了未终止的网卡名称".into())
        }
    }

    pub fn enumerate() -> Result<Vec<Adapter>, String> {
        let flags = GAA_FLAG_INCLUDE_ALL_INTERFACES
            | GAA_FLAG_SKIP_ANYCAST
            | GAA_FLAG_SKIP_MULTICAST
            | GAA_FLAG_SKIP_DNS_SERVER;
        let mut bytes = 15 * 1024;
        for _ in 0..4 {
            if bytes == 0 || bytes > 8 * 1024 * 1024 {
                return Err("Windows 返回的网卡列表大小异常".into());
            }
            let mut buffer = Buffer::new(bytes);
            let first = buffer.0.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
            // SAFETY: The aligned, writable allocation is at least `bytes` long and lives
            // until every returned pointer has been copied into owned Rust values.
            let result = unsafe {
                GetAdaptersAddresses(AF_INET.0 as u32, flags, None, Some(first), &mut bytes)
            };
            if result == ERROR_BUFFER_OVERFLOW.0 {
                continue;
            }
            if result == ERROR_NO_DATA.0 {
                return Ok(Vec::new());
            }
            if result != NO_ERROR.0 {
                return Err(format!("读取网卡失败：Windows 错误 {result}"));
            }
            return read_adapters(&buffer, first);
        }
        Err("网卡列表正在变化，请稍后刷新".into())
    }

    fn read_adapters(
        buffer: &Buffer,
        mut ptr: *const IP_ADAPTER_ADDRESSES_LH,
    ) -> Result<Vec<Adapter>, String> {
        let mut adapters = Vec::new();
        let mut visited = HashSet::new();
        while !ptr.is_null() {
            if !visited.insert(ptr as usize) {
                return Err("Windows 返回了循环的网卡列表".into());
            }
            // SAFETY: The first field is the POD structure's length; read checks the range.
            let length: u32 = unsafe { buffer.read(ptr.cast())? };
            if length < mem::size_of::<IP_ADAPTER_ADDRESSES_LH>() as u32 {
                return Err("Windows 返回的网卡信息版本不受支持".into());
            }
            // SAFETY: IP_ADAPTER_ADDRESSES_LH is a Win32 POD; read checks the buffer range.
            let adapter = unsafe { buffer.read(ptr)? };
            ptr = adapter.Next;
            if matches!(adapter.IfType, IF_TYPE_SOFTWARE_LOOPBACK | IF_TYPE_TUNNEL) {
                continue;
            }
            let id = buffer.ansi(adapter.AdapterName.0)?;
            if id.is_empty() {
                continue;
            }
            let mut name = buffer.wide(adapter.FriendlyName.0)?;
            if name.is_empty() {
                name = buffer.wide(adapter.Description.0)?;
            }
            if name.is_empty() {
                name.clone_from(&id);
            }
            adapters.push(Adapter {
                id,
                name,
                ipv4: read_ipv4(buffer, adapter.FirstUnicastAddress)?,
                link_speed_bps: match adapter.TransmitLinkSpeed {
                    u64::MAX => 0,
                    speed => speed,
                },
                connected: adapter.OperStatus == IfOperStatusUp,
                kind: match adapter.IfType {
                    IF_TYPE_ETHERNET_CSMACD => "以太网",
                    IF_TYPE_IEEE80211 => "Wi-Fi",
                    _ => "其他",
                }
                .into(),
            });
        }
        let kind_order = |kind: &str| match kind {
            "以太网" => 0,
            "Wi-Fi" => 1,
            _ => 2,
        };
        adapters.sort_by(|a, b| {
            (!a.connected, kind_order(&a.kind), &a.name, &a.id).cmp(&(
                !b.connected,
                kind_order(&b.kind),
                &b.name,
                &b.id,
            ))
        });
        Ok(adapters)
    }

    fn read_ipv4(
        buffer: &Buffer,
        mut ptr: *const IP_ADAPTER_UNICAST_ADDRESS_LH,
    ) -> Result<Vec<Ipv4Addr>, String> {
        let mut addresses = Vec::new();
        let mut visited = HashSet::new();
        while !ptr.is_null() {
            if !visited.insert(ptr as usize) {
                return Err("Windows 返回了循环的网卡地址列表".into());
            }
            // SAFETY: The first field is the POD structure's length; read checks the range.
            let length: u32 = unsafe { buffer.read(ptr.cast())? };
            if length < mem::size_of::<IP_ADAPTER_UNICAST_ADDRESS_LH>() as u32 {
                return Err("Windows 返回的网卡地址版本不受支持".into());
            }
            // SAFETY: IP_ADAPTER_UNICAST_ADDRESS_LH is a Win32 POD; read checks the range.
            let entry = unsafe { buffer.read(ptr)? };
            ptr = entry.Next;
            if entry.ValidLifetime == 0
                || (entry.DadState != IpDadStatePreferred && entry.DadState != IpDadStateDeprecated)
                || entry.Address.iSockaddrLength < mem::size_of::<SOCKADDR_IN>() as i32
            {
                continue;
            }
            // SAFETY: SOCKADDR_IN is a Win32 POD; read checks the range.
            let address: SOCKADDR_IN = unsafe { buffer.read(entry.Address.lpSockaddr.cast())? };
            if address.sin_family != AF_INET {
                continue;
            }
            // SAFETY: AF_INET establishes an IPv4 address; the union exposes its four bytes.
            let octets = unsafe { address.sin_addr.S_un.S_un_b };
            let ip = Ipv4Addr::new(octets.s_b1, octets.s_b2, octets.s_b3, octets.s_b4);
            if !ip.is_unspecified() && !ip.is_loopback() && !ip.is_multicast() && !ip.is_broadcast()
            {
                addresses.push(ip);
            }
        }
        addresses.sort_unstable();
        addresses.dedup();
        Ok(addresses)
    }
}
