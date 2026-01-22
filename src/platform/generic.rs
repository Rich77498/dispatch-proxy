//! Generic (non-Linux) platform implementation
//! Uses source address binding without SO_BINDTODEVICE

use crate::load_balancer::LoadBalancer;
use anyhow::Result;
use socket2::{Domain, Protocol, Socket, Type};
use std::net::{SocketAddr, ToSocketAddrs};
use tokio::net::TcpStream;

/// Connect to target address with local address binding
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

    // Create socket and bind to local address
    let socket = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP))?;
    socket.set_reuse_address(true)?;
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
