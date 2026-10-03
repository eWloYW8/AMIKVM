//! Match the connected socket's local IP to its interface, rather than using
//! an unrelated first adapter (JVAPP hX/hY and a(1, null)).
use std::{
    io,
    net::{IpAddr, SocketAddr},
};

#[derive(Clone)]
pub struct Interface {
    pub ipv6_index: u32,
    pub ips: Vec<IpAddr>,
    pub mac: Option<[u8; 6]>,
}

pub fn interfaces() -> io::Result<Vec<Interface>> {
    native::interfaces()
}

/// JVAPP iV uses the OS account name (DOMAIN\user on Windows).
pub fn client_name() -> io::Result<String> {
    native::client_name()
}

/// A scope identifies the IPv6 interface when link-local addresses repeat.
/// Without a unique match, omit the MAC instead of borrowing another adapter.
pub fn client_mac(local: SocketAddr, interfaces: &[Interface]) -> Option<[u8; 6]> {
    let ip = local.ip().to_canonical();
    if ip.is_unspecified() || ip.is_loopback() {
        return None;
    }
    let scope = match local {
        SocketAddr::V6(address) => address.scope_id(),
        _ => 0,
    };
    let mut matching = interfaces.iter().filter(|interface| {
        (scope == 0 || interface.ipv6_index == scope)
            && interface
                .ips
                .iter()
                .any(|address| address.to_canonical() == ip)
    });
    let interface = matching.next()?;
    if matching.next().is_some() {
        return None;
    }
    interface.mac.filter(|mac| *mac != [0; 6])
}

pub fn wire_mac(mac: [u8; 6]) -> String {
    mac.map(|byte| format!("{byte:02X}")).join("-")
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod native {
    use super::*;
    use std::{
        collections::BTreeMap,
        ffi::CStr,
        net::{Ipv4Addr, Ipv6Addr},
        ptr,
    };

    struct Addresses(*mut libc::ifaddrs);
    impl Drop for Addresses {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // The list, including names and sockaddrs, belongs to getifaddrs.
                unsafe { libc::freeifaddrs(self.0) };
            }
        }
    }

    pub fn client_name() -> io::Result<String> {
        let mut size = 16_384usize;
        loop {
            let mut buffer = Vec::new();
            buffer
                .try_reserve_exact(size)
                .map_err(|_| io::Error::from(io::ErrorKind::OutOfMemory))?;
            buffer.resize(size, 0u8);
            let mut record = std::mem::MaybeUninit::<libc::passwd>::uninit();
            let mut found = ptr::null_mut();
            let status = unsafe {
                libc::getpwuid_r(
                    libc::getuid(),
                    record.as_mut_ptr(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                    &mut found,
                )
            };
            if status == libc::ERANGE {
                size = size.checked_mul(2).ok_or(io::ErrorKind::OutOfMemory)?;
                continue;
            }
            if status != 0 {
                return Err(io::Error::from_raw_os_error(status));
            }
            if found.is_null() {
                return Err(io::ErrorKind::NotFound.into());
            }
            let record = unsafe { record.assume_init() };
            if record.pw_name.is_null() {
                return Err(io::ErrorKind::NotFound.into());
            }
            // getpwuid_r stores strings in the still-live caller buffer.
            let name = unsafe { CStr::from_ptr(record.pw_name) }
                .to_string_lossy()
                .into_owned();
            return if name.is_empty() {
                Err(io::ErrorKind::NotFound.into())
            } else {
                Ok(name)
            };
        }
    }

    pub fn interfaces() -> io::Result<Vec<Interface>> {
        let mut head = ptr::null_mut();
        if unsafe { libc::getifaddrs(&mut head) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let addresses = Addresses(head);
        let mut result = BTreeMap::new();
        let mut cursor = addresses.0;
        // All pointed-to storage remains live until the Addresses guard drops.
        while let Some(entry) = unsafe { cursor.as_ref() } {
            cursor = entry.ifa_next;
            if entry.ifa_name.is_null() {
                continue;
            }
            let index = unsafe { libc::if_nametoindex(entry.ifa_name) };
            // IPv4 labels (e.g. eth0:kvm) share the underlying link's index,
            // even though their getifaddrs names differ from its MAC record.
            // Preserve names for unknown indexes instead of merging unrelated links.
            let name = if index == 0 {
                unsafe { CStr::from_ptr(entry.ifa_name) }
                    .to_bytes()
                    .to_vec()
            } else {
                vec![]
            };
            let interface = result.entry((index, name)).or_insert_with(|| Interface {
                ipv6_index: index,
                ips: vec![],
                mac: None,
            });
            if entry.ifa_addr.is_null() {
                continue;
            }
            match unsafe { (*entry.ifa_addr).sa_family as i32 } {
                libc::AF_INET => {
                    let address = unsafe { &*entry.ifa_addr.cast::<libc::sockaddr_in>() };
                    interface
                        .ips
                        .push(Ipv4Addr::from(address.sin_addr.s_addr.to_ne_bytes()).into());
                }
                libc::AF_INET6 => {
                    let address = unsafe { &*entry.ifa_addr.cast::<libc::sockaddr_in6>() };
                    interface
                        .ips
                        .push(Ipv6Addr::from(address.sin6_addr.s6_addr).into());
                }
                _ => {
                    if let Some(mac) = unsafe { hardware(entry.ifa_addr) } {
                        interface.mac = Some(mac);
                    }
                }
            }
        }
        Ok(result.into_values().collect())
    }

    #[cfg(target_os = "linux")]
    unsafe fn hardware(address: *const libc::sockaddr) -> Option<[u8; 6]> {
        if unsafe { (*address).sa_family as i32 } != libc::AF_PACKET {
            return None;
        }
        let link = unsafe { &*address.cast::<libc::sockaddr_ll>() };
        if link.sll_halen != 6 {
            return None;
        }
        let mac = link.sll_addr[..6].try_into().ok()?;
        (mac != [0; 6]).then_some(mac)
    }

    #[cfg(target_os = "macos")]
    unsafe fn hardware(address: *const libc::sockaddr) -> Option<[u8; 6]> {
        if unsafe { (*address).sa_family as i32 } != libc::AF_LINK {
            return None;
        }
        let bytes = address.cast::<u8>();
        let length = usize::from(unsafe { bytes.read() });
        let data = std::mem::offset_of!(libc::sockaddr_dl, sdl_data);
        if length < data {
            return None;
        }
        let name_length = unsafe {
            bytes
                .add(std::mem::offset_of!(libc::sockaddr_dl, sdl_nlen))
                .read()
        };
        let mac_length = unsafe {
            bytes
                .add(std::mem::offset_of!(libc::sockaddr_dl, sdl_alen))
                .read()
        };
        let start = data + usize::from(name_length);
        if mac_length != 6 || start + 6 > length {
            return None;
        }
        // sockaddr_dl's variable tail may exceed the declared sdl_data array.
        let bytes = unsafe { std::slice::from_raw_parts(address.cast::<u8>().add(start), 6) };
        let mac = bytes.try_into().ok()?;
        (mac != [0; 6]).then_some(mac)
    }
}

#[cfg(windows)]
mod native {
    use super::*;
    use std::{
        mem::size_of,
        net::{Ipv4Addr, Ipv6Addr},
        ptr,
    };
    use windows_sys::Win32::{
        Foundation::{ERROR_BUFFER_OVERFLOW, ERROR_MORE_DATA, ERROR_NO_DATA, ERROR_SUCCESS},
        NetworkManagement::IpHelper::{
            GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
            GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH,
        },
        Networking::WinSock::{AF_INET, AF_INET6, AF_UNSPEC, SOCKADDR_IN, SOCKADDR_IN6},
        Security::Authentication::Identity::{GetUserNameExW, NameSamCompatible},
    };

    pub fn client_name() -> io::Result<String> {
        let mut needed = 256usize;
        for _ in 0..3 {
            let mut buffer = vec![0u16; needed];
            let mut size = u32::try_from(buffer.len()).map_err(|_| io::ErrorKind::InvalidData)?;
            if unsafe { GetUserNameExW(NameSamCompatible, buffer.as_mut_ptr(), &mut size) } {
                let name = buffer
                    .get(..size as usize)
                    .ok_or(io::ErrorKind::InvalidData)?;
                return if name.is_empty() {
                    Err(io::ErrorKind::NotFound.into())
                } else {
                    Ok(String::from_utf16_lossy(name))
                };
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_MORE_DATA as i32) {
                return Err(error);
            }
            needed = size as usize;
        }
        Err(io::Error::from_raw_os_error(ERROR_MORE_DATA as i32))
    }

    pub fn interfaces() -> io::Result<Vec<Interface>> {
        let mut needed = 15_000usize;
        for _ in 0..3 {
            // A typed allocation provides the native adapter struct's alignment.
            let count = needed.div_ceil(size_of::<IP_ADAPTER_ADDRESSES_LH>());
            let mut buffer = Vec::new();
            buffer
                .try_reserve_exact(count)
                .map_err(|_| io::Error::from(io::ErrorKind::OutOfMemory))?;
            buffer.resize(count, IP_ADAPTER_ADDRESSES_LH::default());
            let mut size = u32::try_from(count * size_of::<IP_ADAPTER_ADDRESSES_LH>())
                .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
            let status = unsafe {
                GetAdaptersAddresses(
                    AF_UNSPEC as u32,
                    GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER,
                    ptr::null(),
                    buffer.as_mut_ptr(),
                    &mut size,
                )
            };
            if status == ERROR_BUFFER_OVERFLOW {
                needed = size as usize;
                continue;
            }
            if status == ERROR_NO_DATA {
                return Ok(vec![]);
            }
            if status != ERROR_SUCCESS {
                return Err(io::Error::from_raw_os_error(status as i32));
            }
            let mut result = vec![];
            let mut adapter = buffer.as_ptr();
            // Native linked records remain valid while this buffer is live.
            while !adapter.is_null() {
                let entry = unsafe { adapter.read_unaligned() };
                adapter = entry.Next;
                let mac = if entry.PhysicalAddressLength == 6 {
                    let mac = entry.PhysicalAddress[..6].try_into().unwrap();
                    (mac != [0; 6]).then_some(mac)
                } else {
                    None
                };
                let mut interface = Interface {
                    ipv6_index: entry.Ipv6IfIndex,
                    ips: vec![],
                    mac,
                };
                let mut unicast = entry.FirstUnicastAddress;
                while !unicast.is_null() {
                    let address = unsafe { unicast.read_unaligned() };
                    unicast = address.Next;
                    let socket = address.Address.lpSockaddr;
                    if socket.is_null() || address.Address.iSockaddrLength < size_of::<u16>() as i32
                    {
                        continue;
                    }
                    let family = unsafe { socket.cast::<u16>().read_unaligned() };
                    if family == AF_INET
                        && address.Address.iSockaddrLength >= size_of::<SOCKADDR_IN>() as i32
                    {
                        let address = unsafe { socket.cast::<SOCKADDR_IN>().read_unaligned() };
                        interface.ips.push(
                            Ipv4Addr::from(unsafe { address.sin_addr.S_un.S_addr }.to_ne_bytes())
                                .into(),
                        );
                    } else if family == AF_INET6
                        && address.Address.iSockaddrLength >= size_of::<SOCKADDR_IN6>() as i32
                    {
                        let address = unsafe { socket.cast::<SOCKADDR_IN6>().read_unaligned() };
                        interface
                            .ips
                            .push(Ipv6Addr::from(unsafe { address.sin6_addr.u.Byte }).into());
                    }
                }
                result.push(interface);
            }
            return Ok(result);
        }
        Err(io::Error::from_raw_os_error(ERROR_BUFFER_OVERFLOW as i32))
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod native {
    use super::*;
    pub fn client_name() -> io::Result<String> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
    pub fn interfaces() -> io::Result<Vec<Interface>> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}
