use anyhow::{bail, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub use crate::load_balancer::TargetAddressType;

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

/// Send a SOCKS5 success response
pub async fn send_success_response(conn: &mut TcpStream) -> Result<()> {
    let response = [5, SUCCESS, 0, 1, 0, 0, 0, 0, 0, 0];
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

/// Parse client connection request and return target address with its type
async fn client_connection_request(conn: &mut TcpStream) -> Result<(String, TargetAddressType)> {
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

    if cmd_code != CONNECT {
        send_error_response(conn, COMMAND_NOT_SUPPORTED).await?;
        bail!("Unsupported command code");
    }

    let (address, target_type) = match address_type {
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
            (format!(
                "{}.{}.{}.{}:{}",
                ipv4_addr[0], ipv4_addr[1], ipv4_addr[2], ipv4_addr[3], port
            ), TargetAddressType::IPv4)
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
            (format!("{}:{}", domain_str, port), TargetAddressType::Domain)
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
            let addr = std::net::Ipv6Addr::from(ipv6_addr);
            (format!("[{}]:{}", addr, port), TargetAddressType::IPv6)
        }
        _ => {
            send_error_response(conn, ADDRTYPE_NOT_SUPPORTED).await?;
            bail!("Unsupported address type");
        }
    };

    Ok((address, target_type))
}

/// Handle complete SOCKS5 handshake and return target address with its type
pub async fn handle_socks_handshake(conn: &mut TcpStream) -> Result<(String, TargetAddressType)> {
    // Client greeting
    let (version, _auth_methods) = client_greeting(conn).await?;
    if version != 5 {
        bail!("Unsupported SOCKS version: {}", version);
    }

    // Server's choice (no auth)
    servers_choice(conn).await?;

    // Client connection request
    let (address, target_type) = client_connection_request(conn).await?;

    Ok((address, target_type))
}
