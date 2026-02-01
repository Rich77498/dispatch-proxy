use anyhow::{bail, Result};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub use crate::load_balancer::TargetAddressType;

/// Result of SOCKS5 command parsing
#[derive(Debug)]
pub enum SocksCommand {
    /// CONNECT: target address and address type
    Connect(String, TargetAddressType),
    /// UDP ASSOCIATE: client's declared UDP source address (often 0.0.0.0:0)
    UdpAssociate(SocketAddr),
}

// SOCKS5 Constants

// Auth methods
#[allow(dead_code)]
pub const NOAUTH: u8 = 0x00;
#[allow(dead_code)]
pub const GSSAPI: u8 = 0x01;
#[allow(dead_code)]
pub const USERNAME_PASSWORD: u8 = 0x02;
#[allow(dead_code)]
pub const NO_ACCEPTABLE_METHOD: u8 = 0xFF;

// Commands
pub const CONNECT: u8 = 0x01;
#[allow(dead_code)]
pub const BIND: u8 = 0x02;
#[allow(dead_code)]
pub const UDP_ASSOCIATE: u8 = 0x03;

// Address types
pub const IPV4: u8 = 0x01;
pub const DOMAIN: u8 = 0x03;
#[allow(dead_code)]
pub const IPV6: u8 = 0x04;

// Response status codes
pub const SUCCESS: u8 = 0x00;
pub const SERVER_FAILURE: u8 = 0x01;
#[allow(dead_code)]
pub const CONNECTION_NOT_ALLOWED: u8 = 0x02;
pub const NETWORK_UNREACHABLE: u8 = 0x03;
#[allow(dead_code)]
pub const HOST_UNREACHABLE: u8 = 0x04;
#[allow(dead_code)]
pub const CONNECTION_REFUSED: u8 = 0x05;
#[allow(dead_code)]
pub const TTL_EXPIRED: u8 = 0x06;
pub const COMMAND_NOT_SUPPORTED: u8 = 0x07;
pub const ADDRTYPE_NOT_SUPPORTED: u8 = 0x08;

/// Send a SOCKS5 error response and close the connection
async fn send_error_response(conn: &mut TcpStream, status: u8) -> Result<()> {
    let response = [5, status, 0, 1, 0, 0, 0, 0, 0, 0];
    conn.write_all(&response).await?;
    Ok(())
}

/// Send a SOCKS5 success response (with 0.0.0.0:0 bind address)
pub async fn send_success_response(conn: &mut TcpStream) -> Result<()> {
    let response = [5, SUCCESS, 0, 1, 0, 0, 0, 0, 0, 0];
    conn.write_all(&response).await?;
    Ok(())
}

/// Send a SOCKS5 UDP ASSOCIATE success response with the relay address
pub async fn send_udp_associate_response(conn: &mut TcpStream, relay_addr: SocketAddr) -> Result<()> {
    let mut response = Vec::with_capacity(10);
    response.extend_from_slice(&[5, SUCCESS, 0]);
    match relay_addr {
        SocketAddr::V4(v4) => {
            response.push(IPV4);
            response.extend_from_slice(&v4.ip().octets());
            response.extend_from_slice(&v4.port().to_be_bytes());
        }
        SocketAddr::V6(v6) => {
            response.push(IPV6);
            response.extend_from_slice(&v6.ip().octets());
            response.extend_from_slice(&v6.port().to_be_bytes());
        }
    }
    conn.write_all(&response).await?;
    Ok(())
}

/// Send a SOCKS5 network unreachable response
pub async fn send_network_unreachable(conn: &mut TcpStream) -> Result<()> {
    let response = [5, NETWORK_UNREACHABLE, 0, 1, 0, 0, 0, 0, 0, 0];
    conn.write_all(&response).await?;
    Ok(())
}

/// Parse SOCKS5 client greeting
async fn client_greeting(conn: &mut TcpStream) -> Result<(u8, Vec<u8>)> {
    let mut header = [0u8; 2];
    conn.read_exact(&mut header).await?;

    let socks_version = header[0];
    let num_auth_methods = header[1] as usize;

    let mut auth_methods = vec![0u8; num_auth_methods];
    conn.read_exact(&mut auth_methods).await?;

    Ok((socks_version, auth_methods))
}

/// Send server's authentication choice (no auth required)
async fn servers_choice(conn: &mut TcpStream) -> Result<()> {
    conn.write_all(&[5, NOAUTH]).await?;
    Ok(())
}

/// Parse address from SOCKS5 request, returning (address_string, target_type)
async fn parse_socks_address(conn: &mut TcpStream, address_type: u8) -> Result<(String, TargetAddressType)> {
    match address_type {
        IPV4 => {
            let mut ipv4_addr = [0u8; 4];
            let mut port_bytes = [0u8; 2];

            conn.read_exact(&mut ipv4_addr).await.map_err(|_| {
                anyhow::anyhow!("Failed to read IPv4 address")
            })?;

            conn.read_exact(&mut port_bytes).await.map_err(|_| {
                anyhow::anyhow!("Failed to read port")
            })?;

            let port = u16::from_be_bytes(port_bytes);
            Ok((format!(
                "{}.{}.{}.{}:{}",
                ipv4_addr[0], ipv4_addr[1], ipv4_addr[2], ipv4_addr[3], port
            ), TargetAddressType::IPv4))
        }
        DOMAIN => {
            let mut domain_len = [0u8; 1];
            conn.read_exact(&mut domain_len).await.map_err(|_| {
                anyhow::anyhow!("Failed to read domain length")
            })?;

            let mut domain = vec![0u8; domain_len[0] as usize];
            conn.read_exact(&mut domain).await.map_err(|_| {
                anyhow::anyhow!("Failed to read domain name")
            })?;

            let mut port_bytes = [0u8; 2];
            conn.read_exact(&mut port_bytes).await.map_err(|_| {
                anyhow::anyhow!("Failed to read port")
            })?;

            let port = u16::from_be_bytes(port_bytes);
            let domain_str = String::from_utf8_lossy(&domain);
            Ok((format!("{}:{}", domain_str, port), TargetAddressType::Domain))
        }
        IPV6 => {
            let mut ipv6_addr = [0u8; 16];
            let mut port_bytes = [0u8; 2];

            conn.read_exact(&mut ipv6_addr).await.map_err(|_| {
                anyhow::anyhow!("Failed to read IPv6 address")
            })?;

            conn.read_exact(&mut port_bytes).await.map_err(|_| {
                anyhow::anyhow!("Failed to read port")
            })?;

            let port = u16::from_be_bytes(port_bytes);
            let addr = Ipv6Addr::from(ipv6_addr);
            Ok((format!("[{}]:{}", addr, port), TargetAddressType::IPv6))
        }
        _ => {
            send_error_response(conn, ADDRTYPE_NOT_SUPPORTED).await?;
            bail!("Unsupported address type");
        }
    }
}

/// Parse client connection request and return the SOCKS command
async fn client_connection_request(conn: &mut TcpStream) -> Result<SocksCommand> {
    let mut header = [0u8; 4];
    conn.read_exact(&mut header).await.map_err(|_| {
        anyhow::anyhow!("Failed to read connection request header")
    })?;

    let socks_version = header[0];
    let cmd_code = header[1];
    // header[2] is reserved
    let address_type = header[3];

    if socks_version != 5 {
        send_error_response(conn, SERVER_FAILURE).await?;
        bail!("Unsupported SOCKS version");
    }

    let (address, target_type) = parse_socks_address(conn, address_type).await?;

    match cmd_code {
        CONNECT => Ok(SocksCommand::Connect(address, target_type)),
        UDP_ASSOCIATE => {
            // Parse the client's declared source address into a SocketAddr
            let sock_addr: SocketAddr = address.parse().unwrap_or_else(|_| {
                SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)
            });
            Ok(SocksCommand::UdpAssociate(sock_addr))
        }
        _ => {
            send_error_response(conn, COMMAND_NOT_SUPPORTED).await?;
            bail!("Unsupported command code: {}", cmd_code);
        }
    }
}

/// Handle complete SOCKS5 handshake and return the parsed command
pub async fn handle_socks_handshake(conn: &mut TcpStream) -> Result<SocksCommand> {
    // Client greeting
    let (version, _auth_methods) = client_greeting(conn).await?;
    if version != 5 {
        bail!("Unsupported SOCKS version: {}", version);
    }

    // Server's choice (no auth)
    servers_choice(conn).await?;

    // Client connection request
    client_connection_request(conn).await
}

/// Parse a SOCKS5 UDP request header from a datagram.
/// Returns (destination address string, payload slice start index).
/// Header format: [RSV(2) | FRAG(1) | ATYP(1) | DST.ADDR(var) | DST.PORT(2)]
pub fn parse_udp_header(data: &[u8]) -> Result<(String, usize)> {
    if data.len() < 10 {
        bail!("UDP header too short");
    }

    // RSV must be 0x0000
    // FRAG: only support fragment 0 (standalone)
    let frag = data[2];
    if frag != 0 {
        bail!("Fragmented UDP not supported (frag={})", frag);
    }

    let atyp = data[3];
    match atyp {
        IPV4 => {
            if data.len() < 10 {
                bail!("UDP header too short for IPv4");
            }
            let ip = Ipv4Addr::new(data[4], data[5], data[6], data[7]);
            let port = u16::from_be_bytes([data[8], data[9]]);
            Ok((format!("{}:{}", ip, port), 10))
        }
        DOMAIN => {
            let dlen = data[4] as usize;
            let needed = 4 + 1 + dlen + 2;
            if data.len() < needed {
                bail!("UDP header too short for domain");
            }
            let domain = String::from_utf8_lossy(&data[5..5 + dlen]);
            let port = u16::from_be_bytes([data[5 + dlen], data[6 + dlen]]);
            Ok((format!("{}:{}", domain, port), needed))
        }
        IPV6 => {
            let needed = 4 + 16 + 2;
            if data.len() < needed {
                bail!("UDP header too short for IPv6");
            }
            let mut octets = [0u8; 16];
            octets.copy_from_slice(&data[4..20]);
            let ip = Ipv6Addr::from(octets);
            let port = u16::from_be_bytes([data[20], data[21]]);
            Ok((format!("[{}]:{}", ip, port), needed))
        }
        _ => bail!("Unsupported address type in UDP header: {}", atyp),
    }
}

/// Build a SOCKS5 UDP response header for a datagram from the given source address.
pub fn build_udp_header(src_addr: &SocketAddr) -> Vec<u8> {
    let mut header = vec![0u8; 3]; // RSV(2) + FRAG(1) = 0
    match src_addr {
        SocketAddr::V4(v4) => {
            header.push(IPV4);
            header.extend_from_slice(&v4.ip().octets());
            header.extend_from_slice(&v4.port().to_be_bytes());
        }
        SocketAddr::V6(v6) => {
            header.push(IPV6);
            header.extend_from_slice(&v6.ip().octets());
            header.extend_from_slice(&v6.port().to_be_bytes());
        }
    }
    header
}
