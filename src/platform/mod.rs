#[cfg(target_os = "linux")]
mod linux;

#[cfg(not(target_os = "linux"))]
mod generic;

use crate::load_balancer::{LoadBalancerPool, TargetAddressType};
use crate::socks;
use anyhow::Result;
use socket2::{Domain, Protocol, Socket, Type};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::net::{TcpStream, UdpSocket};
use tracing::{debug, info, warn};

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
    let relay_domain = if listen_ip.is_ipv6() { Domain::IPV6 } else { Domain::IPV4 };
    let relay_sock = Socket::new(relay_domain, Type::DGRAM, Some(Protocol::UDP))?;
    let _ = relay_sock.set_recv_buffer_size(2 * 1024 * 1024);
    let _ = relay_sock.set_send_buffer_size(2 * 1024 * 1024);
    relay_sock.set_reuse_address(true)?;
    relay_sock.bind(&SocketAddr::new(listen_ip, 0).into())?;
    relay_sock.set_nonblocking(true)?;
    let relay = UdpSocket::from_std(relay_sock.into())?;
    let relay_addr = relay.local_addr()?;

    info!("UDP ASSOCIATE relay on {} via {} LB: {}", relay_addr, lb.address, idx);

    // Send the relay address back to the client
    socks::send_udp_associate_response(&mut client, relay_addr).await?;

    // Relay loop - separate buffers to avoid double mutable borrow in select!
    let mut client_buf = vec![0u8; 65535];
    let mut remote_buf = vec![0u8; 65535];
    let mut tcp_buf = [0u8; 1];
    let mut client_addr: Option<SocketAddr> = None;
    let mut dns_cache: HashMap<String, SocketAddr> = HashMap::new();

    loop {
        tokio::select! {
            // Client -> Destination (via relay socket)
            result = relay.recv_from(&mut client_buf) => {
                match result {
                    Ok((len, from)) => {
                        client_addr = Some(from);

                        match socks::parse_udp_header(&client_buf[..len]) {
                            Ok((dst_addr, header_len)) => {
                                let payload = &client_buf[header_len..len];
                                let dst: SocketAddr = match dst_addr.parse() {
                                    Ok(a) => a,
                                    Err(_) => {
                                        // Try async DNS resolution for domain targets (with cache)
                                        if let Some(&cached) = dns_cache.get(&dst_addr) {
                                            cached
                                        } else {
                                            match tokio::net::lookup_host(&dst_addr).await {
                                                Ok(mut addrs) => {
                                                    match addrs.next() {
                                                        Some(a) => {
                                                            dns_cache.insert(dst_addr.clone(), a);
                                                            a
                                                        }
                                                        None => {
                                                            warn!("UDP: no addrs for {}", dst_addr);
                                                            continue;
                                                        }
                                                    }
                                                }
                                                Err(e) => {
                                                    warn!("UDP: failed to resolve {}: {}", dst_addr, e);
                                                    continue;
                                                }
                                            }
                                        }
                                    }
                                };
                                debug!("UDP: {} -> {} ({} bytes)", from, dst, payload.len());
                                if let Err(e) = outbound.send_to(payload, dst).await {
                                    warn!("UDP: failed to send to {}: {}", dst, e);
                                }
                            }
                            Err(e) => {
                                warn!("UDP: bad header from {}: {}", from, e);
                            }
                        }
                    }
                    Err(e) => {
                        warn!("UDP: relay recv error: {}", e);
                    }
                }
            }

            // Destination -> Client (via outbound socket)
            result = outbound.recv_from(&mut remote_buf) => {
                match result {
                    Ok((len, from)) => {
                        if let Some(caddr) = client_addr {
                            debug!("UDP: {} -> client {} ({} bytes)", from, caddr, len);
                            let header = socks::build_udp_header(&from);
                            let mut packet = header;
                            packet.extend_from_slice(&remote_buf[..len]);
                            if let Err(e) = relay.send_to(&packet, caddr).await {
                                warn!("UDP: failed to send to client {}: {}", caddr, e);
                            }
                        }
                    }
                    Err(e) => {
                        warn!("UDP: outbound recv error: {}", e);
                    }
                }
            }

            // TCP control connection: detect close via async read
            result = client.read(&mut tcp_buf) => {
                match result {
                    Ok(0) | Err(_) => {
                        info!("UDP ASSOCIATE: TCP control connection closed");
                        break;
                    }
                    Ok(_) => {
                        // Client sent data on control connection; ignore
                    }
                }
            }
        }
    }

    Ok(())
}
