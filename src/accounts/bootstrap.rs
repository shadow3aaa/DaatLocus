//! Initial account creation requires an actual local OS-owner connection.
//! Neither Origin/Host nor "loopback" alone identifies the owner of a profile.
use std::net::SocketAddr;
pub(crate) fn local_owner_connection(peer: SocketAddr, listener: Option<SocketAddr>) -> bool {
    let Some(listener) = listener else {
        return false;
    };
    if !peer.ip().is_loopback() || !listener.ip().is_loopback() {
        return false;
    }
    platform_owner(peer, listener)
}

#[cfg(windows)]
fn platform_owner(peer: SocketAddr, listener: SocketAddr) -> bool {
    use std::{mem::size_of, ptr};
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE},
        NetworkManagement::IpHelper::{
            GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCPROW_OWNER_PID,
            TCP_TABLE_OWNER_PID_ALL,
        },
        Networking::WinSock::{AF_INET, AF_INET6},
        Security::{
            EqualSid, GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenIsAppContainer, TokenUser,
        },
        System::Threading::{
            GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
        },
    };
    unsafe fn same_owner(pid: u32) -> bool {
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if process.is_null() {
                return false;
            }
            let mut client: HANDLE = ptr::null_mut();
            let mut own: HANDLE = ptr::null_mut();
            let opened = OpenProcessToken(process, TOKEN_QUERY, &mut client) != 0
                && OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut own) != 0;
            CloseHandle(process);
            let mut container: u32 = 0;
            let mut returned = 0;
            let unrestricted = opened
                && GetTokenInformation(
                    client,
                    TokenIsAppContainer,
                    (&mut container as *mut u32).cast(),
                    size_of::<u32>() as u32,
                    &mut returned,
                ) != 0
                && container == 0;
            let user = |token: HANDLE| -> Option<Vec<u8>> {
                let mut size = 0;
                GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut size);
                if size == 0 || size > 65536 {
                    return None;
                }
                let mut buffer = vec![0; size as usize];
                (GetTokenInformation(
                    token,
                    TokenUser,
                    buffer.as_mut_ptr().cast(),
                    size,
                    &mut size,
                ) != 0)
                    .then_some(buffer)
            };
            let equal = if unrestricted {
                match (user(client), user(own)) {
                    (Some(client), Some(own)) => {
                        let client_user = ptr::read_unaligned(client.as_ptr().cast::<TOKEN_USER>());
                        let own_user = ptr::read_unaligned(own.as_ptr().cast::<TOKEN_USER>());
                        EqualSid(client_user.User.Sid, own_user.User.Sid) != 0
                    }
                    _ => false,
                }
            } else {
                false
            };
            if !client.is_null() {
                CloseHandle(client);
            }
            if !own.is_null() {
                CloseHandle(own);
            }
            equal
        }
    }
    let family = if peer.is_ipv4() && listener.is_ipv4() {
        AF_INET
    } else if peer.is_ipv6() && listener.is_ipv6() {
        AF_INET6
    } else {
        return false;
    };
    unsafe {
        let mut length = 0;
        GetExtendedTcpTable(
            ptr::null_mut(),
            &mut length,
            0,
            family as u32,
            TCP_TABLE_OWNER_PID_ALL,
            0,
        );
        if !(4..=16 * 1024 * 1024).contains(&length) {
            return false;
        }
        let mut buffer = vec![0u8; length as usize];
        if GetExtendedTcpTable(
            buffer.as_mut_ptr().cast(),
            &mut length,
            0,
            family as u32,
            TCP_TABLE_OWNER_PID_ALL,
            0,
        ) != 0
        {
            return false;
        }
        let count = ptr::read_unaligned(buffer.as_ptr().cast::<u32>()) as usize;
        let row_size = if family == AF_INET {
            size_of::<MIB_TCPROW_OWNER_PID>()
        } else {
            size_of::<MIB_TCP6ROW_OWNER_PID>()
        };
        if count > 65536 || 4 + count * row_size > buffer.len() {
            return false;
        }
        for index in 0..count {
            let pointer = buffer.as_ptr().add(4 + index * row_size);
            let (local, remote, pid) = if family == AF_INET {
                let row = ptr::read_unaligned(pointer.cast::<MIB_TCPROW_OWNER_PID>());
                (
                    SocketAddr::new(
                        std::net::Ipv4Addr::from(row.dwLocalAddr.to_ne_bytes()).into(),
                        u16::from_be(row.dwLocalPort as u16),
                    ),
                    SocketAddr::new(
                        std::net::Ipv4Addr::from(row.dwRemoteAddr.to_ne_bytes()).into(),
                        u16::from_be(row.dwRemotePort as u16),
                    ),
                    row.dwOwningPid,
                )
            } else {
                let row = ptr::read_unaligned(pointer.cast::<MIB_TCP6ROW_OWNER_PID>());
                (
                    SocketAddr::new(
                        std::net::Ipv6Addr::from(row.ucLocalAddr).into(),
                        u16::from_be(row.dwLocalPort as u16),
                    ),
                    SocketAddr::new(
                        std::net::Ipv6Addr::from(row.ucRemoteAddr).into(),
                        u16::from_be(row.dwRemotePort as u16),
                    ),
                    row.dwOwningPid,
                )
            };
            if local == peer && remote == listener {
                return same_owner(pid);
            }
        }
        false
    }
}

#[cfg(target_os = "linux")]
fn platform_owner(peer: SocketAddr, listener: SocketAddr) -> bool {
    fn address(value: SocketAddr) -> String {
        let encoded = match value.ip() {
            std::net::IpAddr::V4(ip) => format!("{:08X}", u32::from_ne_bytes(ip.octets())),
            std::net::IpAddr::V6(ip) => ip
                .octets()
                .as_chunks::<4>()
                .0
                .iter()
                .map(|chunk| format!("{:08X}", u32::from_ne_bytes(*chunk)))
                .collect(),
        };
        format!("{encoded}:{:04X}", value.port())
    }
    let path = if peer.is_ipv4() && listener.is_ipv4() {
        "/proc/net/tcp"
    } else if peer.is_ipv6() && listener.is_ipv6() {
        "/proc/net/tcp6"
    } else {
        return false;
    };
    let Ok(table) = std::fs::read_to_string(path) else {
        return false;
    };
    if table.len() > 16 * 1024 * 1024 {
        return false;
    }
    let local = address(peer);
    let remote = address(listener);
    table.lines().take(65536).any(|line| {
        let columns: Vec<_> = line.split_whitespace().collect();
        columns.len() >= 10
            && columns[1] == local
            && columns[2] == remote
            && columns[7].parse::<u32>().ok() == Some(unsafe { libc::geteuid() })
            && columns[9]
                .parse::<u64>()
                .is_ok_and(socket_in_owner_namespace)
    })
}

#[cfg(target_os = "linux")]
fn socket_in_owner_namespace(inode: u64) -> bool {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    if inode == 0 {
        return false;
    }
    let Ok(namespace) = std::fs::read_link("/proc/self/ns/user") else {
        return false;
    };
    let target = std::path::PathBuf::from(format!("socket:[{inode}]"));
    let inspect = |path: &std::path::Path| -> bool {
        // Pin the proc directory to the process instance, not a reusable PID.
        let Ok(process) = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(path)
        else {
            return false;
        };
        if !process
            .metadata()
            .is_ok_and(|md| md.uid() == unsafe { libc::geteuid() })
        {
            return false;
        }
        let pinned = std::path::PathBuf::from(format!("/proc/self/fd/{}", process.as_raw_fd()));
        if std::fs::read_link(pinned.join("ns/user")).ok().as_ref() != Some(&namespace) {
            return false;
        }
        let Ok(descriptors) = std::fs::read_dir(pinned.join("fd")) else {
            return false;
        };
        descriptors
            .take(65536)
            .filter_map(Result::ok)
            .any(|entry| std::fs::read_link(entry.path()).is_ok_and(|path| path == target))
    };
    // Native transport and the isolated fixture commonly connect from this
    // process. Browser first-registration checks use the bounded proc lookup.
    if inspect(std::path::Path::new("/proc/self")) {
        return true;
    }
    let Ok(processes) = std::fs::read_dir("/proc") else {
        return false;
    };
    processes
        .take(65536)
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().parse::<u32>().is_ok())
        .any(|entry| inspect(&entry.path()))
}

#[cfg(not(any(windows, target_os = "linux")))]
fn platform_owner(_: SocketAddr, _: SocketAddr) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remote_unknown_and_non_loopback_listeners_cannot_bootstrap() {
        assert!(!local_owner_connection(
            "192.0.2.1:12345".parse().unwrap(),
            Some("127.0.0.1:45825".parse().unwrap())
        ));
        assert!(!local_owner_connection(
            "127.0.0.1:12345".parse().unwrap(),
            None
        ));
        assert!(!local_owner_connection(
            "127.0.0.1:12345".parse().unwrap(),
            Some("0.0.0.0:45825".parse().unwrap())
        ));
    }
    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn actual_owner_socket_can_bootstrap_ipv4_and_ipv6() {
        for address in ["127.0.0.1:0", "[::1]:0"] {
            let listener = std::net::TcpListener::bind(address).unwrap();
            let _client = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            let (_socket, peer) = listener.accept().unwrap();
            assert!(local_owner_connection(
                peer,
                Some(listener.local_addr().unwrap())
            ));
        }
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn owner_socket_from_another_process_can_bootstrap() {
        use std::io::{Read, Write};
        use std::time::{Duration, Instant};
        const FIXTURE: &str = "DAAT_LOCUS_TEST_OWNER_SOCKET";
        if let Ok(address) = std::env::var(FIXTURE) {
            let mut socket = std::net::TcpStream::connect(address).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut ack = [0];
            socket.read_exact(&mut ack).unwrap();
            assert_eq!(ack, [1]);
            return;
        }
        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        for address in ["127.0.0.1:0", "[::1]:0"] {
            let listener = std::net::TcpListener::bind(address).unwrap();
            listener.set_nonblocking(true).unwrap();
            let mut child=Child(std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","accounts::bootstrap::tests::owner_socket_from_another_process_can_bootstrap"])
                .env(FIXTURE,listener.local_addr().unwrap().to_string()).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).spawn().unwrap());
            let deadline = Instant::now() + Duration::from_secs(10);
            let (mut socket, peer) = loop {
                match listener.accept() {
                    Ok(accepted) => break accepted,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(error) => panic!("accept local owner fixture: {error}"),
                }
                assert!(Instant::now() < deadline, "owner fixture did not connect");
                assert!(
                    child.0.try_wait().unwrap().is_none(),
                    "owner fixture exited before connecting"
                );
                std::thread::sleep(Duration::from_millis(5));
            };
            assert!(local_owner_connection(
                peer,
                Some(listener.local_addr().unwrap())
            ));
            socket.write_all(&[1]).unwrap();
            assert!(child.0.wait().unwrap().success());
        }
    }
}
