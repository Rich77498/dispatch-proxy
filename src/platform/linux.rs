//! Linux-specific platform implementation
//! Uses SO_BINDTODEVICE for true per-interface binding

use crate::load_balancer::LoadBalancer;
use anyhow::Result;
use nix::sys::socket::{setsockopt, sockopt::BindToDevice};
use socket2::{Domain, Protocol, Socket, Type};
use std::net::{SocketAddr, ToSocketAddrs};
use std::os::unix::io::AsRawFd;
use tokio::net::TcpStream;
use tracing::warn;

/// Connect to target address with interface binding using SO_BINDTODEVICE
pub async fn connect_with_interface(
    target_addr: &str,
    lb: &LoadBalancer,
) -> Result<TcpStream> {
    // Parse local address (the load balancer's IP with port 0)
    let local_addr: SocketAddr = lb
        .address
        .to_socket_addrs()?
        .find(|a| a.is_ipv4())
        .ok_or_else(|| anyhow::anyhow!("Could not resolve local address"))?;

    // Parse target address
    let target: SocketAddr = target_addr
        .to_socket_addrs()?
        .find(|a| a.is_ipv4())
        .ok_or_else(|| anyhow::anyhow!("Could not resolve target address"))?;

    // Create socket
    let socket = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP))?;
    socket.set_reuse_address(true)?;

    // Bind to interface using SO_BINDTODEVICE if interface name is provided
    // NOTE: Requires root or CAP_NET_RAW capability
    // sudo setcap cap_net_raw=eip ./dispatch-proxy
    if let Some(ref iface) = lb.iface {
        let fd = socket.as_raw_fd();
        if let Err(e) = setsockopt(fd, BindToDevice, &std::ffi::OsString::from(iface)) {
            warn!("Couldn't bind to interface {}: {}", iface, e);
        }
    }

    // Bind to local address
    socket.bind(&local_addr.into())?;
    socket.set_nonblocking(true)?;

    // Connect to target
    match socket.connect(&target.into()) {
        Ok(()) => {}
        Err(e) if e.raw_os_error() == Some(libc::EINPROGRESS) => {}
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
        Err(e) => return Err(e.into()),
    }

    // Convert to tokio TcpStream
    let std_stream: std::net::TcpStream = socket.into();
    let stream = TcpStream::from_std(std_stream)?;

    // Wait for connection to complete
    stream.writable().await?;

    // Check for connection errors
    if let Some(e) = stream.take_error()? {
        return Err(e.into());
    }

    Ok(stream)
}
