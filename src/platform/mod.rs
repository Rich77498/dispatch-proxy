#[cfg(target_os = "linux")]
mod linux;

#[cfg(not(target_os = "linux"))]
mod generic;

use crate::load_balancer::{LoadBalancerPool, TargetAddressType};
use crate::socks;
use anyhow::Result;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpStream, UdpSocket};
use tracing::{info, warn};

#[cfg(target_os = "linux")]
use linux::{connect_with_interface, bind_udp_to_interface};

#[cfg(not(target_os = "linux"))]
use generic::{connect_with_interface, bind_udp_to_interface};

/// Connect to target address through load balancer and relay data
pub async fn connect_and_relay(
    mut client: TcpStream,
    target_addr: &str,
    target_type: TargetAddressType,
    pool: Arc<LoadBalancerPool>,
) -> Result<()> {
    let (lb, idx) = pool.get_load_balancer(None, Some(target_type));

    match connect_with_interface(target_addr, &lb).await {
        Ok(mut remote) => {
            info!("{} -> {} LB: {}", target_addr, lb.address, idx);
            socks::send_success_response(&mut client).await?;

            // Bidirectional relay
            let _ = tokio::io::copy_bidirectional(&mut client, &mut remote).await;
            Ok(())
        }
        Err(e) => {
            warn!("{} -> {} {{{}}} LB: {}", target_addr, lb.address, e, idx);
            socks::send_network_unreachable(&mut client).await?;
            Err(e)
        }
    }
}

/// Handle UDP ASSOCIATE: open relay socket, send response, relay UDP packets
pub async fn udp_associate_and_relay(
    mut client: TcpStream,
    listen_ip: std::net::IpAddr,
    pool: Arc<LoadBalancerPool>,
) -> Result<()> {
    // Pick a load balancer for the outbound UDP socket
    let (lb, idx) = pool.get_load_balancer(None, None);

    // Create outbound UDP socket bound to the selected interface
    let outbound_std = bind_udp_to_interface(&lb)?;
    let outbound = UdpSocket::from_std(outbound_std)?;

    // Create the relay socket that the SOCKS client will send UDP to
    let relay = UdpSocket::bind(SocketAddr::new(listen_ip, 0)).await?;
    let relay_addr = relay.local_addr()?;

    info!("UDP ASSOCIATE relay on {} via {} LB: {}", relay_addr, lb.address, idx);

    // Send the relay address back to the client
    socks::send_udp_associate_response(&mut client, relay_addr).await?;

    // Relay loop - separate buffers to avoid double mutable borrow in select!
    let mut client_buf = vec![0u8; 65535];
    let mut remote_buf = vec![0u8; 65535];
    let mut client_addr: Option<SocketAddr> = None;

    loop {
        tokio::select! {
            // Client -> Destination (via relay socket)
            result = relay.recv_from(&mut client_buf) => {
                let (len, from) = result?;
                client_addr = Some(from);

                match socks::parse_udp_header(&client_buf[..len]) {
                    Ok((dst_addr, header_len)) => {
                        let payload = &client_buf[header_len..len];
                        let dst: SocketAddr = match dst_addr.parse() {
                            Ok(a) => a,
                            Err(_) => {
                                // Try DNS resolution for domain targets
                                use std::net::ToSocketAddrs;
                                match dst_addr.to_socket_addrs().and_then(|mut i| i.next().ok_or_else(|| {
                                    std::io::Error::other("no addrs")
                                })) {
                                    Ok(a) => a,
                                    Err(e) => {
                                        warn!("UDP: failed to resolve {}: {}", dst_addr, e);
                                        continue;
                                    }
                                }
                            }
                        };
                        if let Err(e) = outbound.send_to(payload, dst).await {
                            warn!("UDP: failed to send to {}: {}", dst, e);
                        }
                    }
                    Err(e) => {
                        warn!("UDP: bad header from {}: {}", from, e);
                    }
                }
            }

            // Destination -> Client (via outbound socket)
            result = outbound.recv_from(&mut remote_buf) => {
                let (len, from) = result?;
                if let Some(caddr) = client_addr {
                    let header = socks::build_udp_header(&from);
                    let mut packet = header;
                    packet.extend_from_slice(&remote_buf[..len]);
                    if let Err(e) = relay.send_to(&packet, caddr).await {
                        warn!("UDP: failed to send to client {}: {}", caddr, e);
                    }
                }
            }

            // TCP control connection: detect close
            result = client.readable() => {
                match result {
                    Ok(()) => {
                        let mut probe = [0u8; 1];
                        match client.try_read(&mut probe) {
                            Ok(0) | Err(_) => {
                                info!("UDP ASSOCIATE: TCP control connection closed");
                                break;
                            }
                            Ok(_) => {
                                // Client sent data on control connection; ignore
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    }

    Ok(())
}
