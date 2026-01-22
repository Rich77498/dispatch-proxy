mod load_balancer;
mod platform;
mod socks;

use anyhow::{bail, Result};
use clap::Parser;
use load_balancer::{LoadBalancer, LoadBalancerPool};
use socket2::{Domain, Protocol, Socket, Type};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tracing::{info, warn, Level};
use tracing_subscriber::FmtSubscriber;

#[derive(Parser, Debug)]
#[command(name = "dispatch-proxy")]
#[command(about = "A SOCKS5 load balancing proxy that combines multiple internet connections")]
struct Args {
    /// The host to listen for SOCKS connections
    #[arg(long, default_value = "127.0.0.1")]
    lhost: String,

    /// The local port to listen for SOCKS connections
    #[arg(long, default_value = "8080")]
    lport: u16,

    /// Shows the available addresses for dispatching (non-tunnelling mode only)
    #[arg(short, long)]
    list: bool,

    /// Use tunnelling mode (acts as a transparent load balancing proxy)
    #[arg(short, long)]
    tunnel: bool,

    /// Disable logs
    #[arg(short, long)]
    quiet: bool,

    /// Auto-detect interfaces with working internet connectivity
    #[arg(short, long)]
    auto: bool,

    /// Load balancer addresses (IP@ratio or host:port@ratio for tunnel mode)
    addresses: Vec<String>,
}

/// Detect and list available network interfaces
fn detect_interfaces() {
    println!("--- Listing the available addresses for dispatching");

    if let Ok(interfaces) = get_if_addrs::get_if_addrs() {
        for iface in interfaces {
            if !iface.is_loopback() {
                match iface.ip() {
                    IpAddr::V4(ipv4) => {
                        println!("[+] {}, IPv4:{}", iface.name, ipv4);
                    }
                    IpAddr::V6(ipv6) => {
                        println!("[+] {}, IPv6:{}", iface.name, ipv6);
                    }
                }
            }
        }
    }
}

/// Get interface name from IP address (supports both IPv4 and IPv6)
fn get_iface_from_ip(ip: &IpAddr) -> Option<String> {
    if let Ok(interfaces) = get_if_addrs::get_if_addrs() {
        for iface in interfaces {
            if !iface.is_loopback() && &iface.ip() == ip {
                return Some(iface.name);
            }
        }
    }
    None
}

/// Test if an interface has working internet connectivity
async fn test_interface_connectivity(ip: IpAddr) -> bool {
    // Use Cloudflare DNS (1.1.1.1:53 for IPv4, [2606:4700:4700::1111]:53 for IPv6)
    let (test_addr, domain): (SocketAddr, Domain) = match ip {
        IpAddr::V4(_) => ("1.1.1.1:53".parse().unwrap(), Domain::IPV4),
        IpAddr::V6(_) => ("[2606:4700:4700::1111]:53".parse().unwrap(), Domain::IPV6),
    };

    let local_addr: SocketAddr = match ip {
        IpAddr::V4(v4) => SocketAddr::new(IpAddr::V4(v4), 0),
        IpAddr::V6(v6) => SocketAddr::new(IpAddr::V6(v6), 0),
    };

    // Try to connect with a timeout
    let result = tokio::time::timeout(Duration::from_secs(3), async {
        let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP)).ok()?;
        socket.set_reuse_address(true).ok()?;
        socket.bind(&local_addr.into()).ok()?;
        socket.set_nonblocking(true).ok()?;

        match socket.connect(&test_addr.into()) {
            Ok(()) => {}
            Err(e) if e.raw_os_error() == Some(libc::EINPROGRESS) => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => return None,
        }

        let std_stream: std::net::TcpStream = socket.into();
        let stream = tokio::net::TcpStream::from_std(std_stream).ok()?;
        stream.writable().await.ok()?;

        if stream.take_error().ok()?.is_some() {
            return None;
        }

        Some(())
    })
    .await;

    matches!(result, Ok(Some(())))
}

/// Auto-detect interfaces with working internet connectivity
async fn auto_detect_interfaces() -> Vec<(String, IpAddr)> {
    let mut interfaces = Vec::new();

    if let Ok(all_interfaces) = get_if_addrs::get_if_addrs() {
        for iface in all_interfaces {
            if !iface.is_loopback() {
                let ip = iface.ip();
                interfaces.push((iface.name, ip));
            }
        }
    }

    // Test all interfaces concurrently
    let mut working = Vec::new();
    let mut handles = Vec::new();

    for (name, ip) in interfaces {
        let name_clone = name.clone();
        let handle = tokio::spawn(async move {
            let works = test_interface_connectivity(ip).await;
            (name_clone, ip, works)
        });
        handles.push(handle);
    }

    for handle in handles {
        if let Ok((name, ip, works)) = handle.await {
            if works {
                working.push((name, ip));
            }
        }
    }

    working
}

/// Parse an IP address that may be in bracket notation for IPv6
fn parse_ip_address(s: &str) -> Option<IpAddr> {
    // Handle bracketed IPv6 addresses like [::1] or [fe80::1]
    if s.starts_with('[') && s.ends_with(']') {
        s[1..s.len() - 1].parse().ok()
    } else {
        s.parse().ok()
    }
}

/// Parse load balancer addresses from command line arguments
fn parse_load_balancers(args: &[String], tunnel: bool) -> Result<Vec<LoadBalancer>> {
    if args.is_empty() {
        bail!("Please specify one or more load balancers");
    }

    let mut load_balancers = Vec::with_capacity(args.len());

    for (idx, arg) in args.iter().enumerate() {
        let parts: Vec<&str> = arg.split('@').collect();
        let address_part = parts[0];

        // Parse contention ratio
        let contention_ratio: u32 = if parts.len() > 1 {
            parts[1]
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid contention ratio for {}", address_part))?
        } else {
            1
        };

        if contention_ratio == 0 {
            bail!("Invalid contention ratio for {}", address_part);
        }

        let (address, iface, is_ipv6) = if tunnel {
            // Tunnel mode: expect host:port format
            // Handle IPv6 addresses like [::1]:7777
            let (host, port_str) = if address_part.starts_with('[') {
                // IPv6 address in brackets
                let bracket_end = address_part
                    .find(']')
                    .ok_or_else(|| anyhow::anyhow!("Invalid IPv6 address {}", address_part))?;
                let host = &address_part[..=bracket_end];
                let rest = &address_part[bracket_end + 1..];
                if !rest.starts_with(':') {
                    bail!("Invalid address specification {}", address_part);
                }
                (host.to_string(), &rest[1..])
            } else {
                // IPv4 or hostname
                let parts: Vec<&str> = address_part.rsplitn(2, ':').collect();
                if parts.len() != 2 {
                    bail!("Invalid address specification {}", address_part);
                }
                (parts[1].to_string(), parts[0])
            };

            let port: u16 = port_str
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid port {}", address_part))?;

            if port == 0 {
                bail!("Invalid port {}", address_part);
            }

            let is_ipv6 = host.starts_with('[');
            (format!("{}:{}", host, port), None, is_ipv6)
        } else {
            // Normal mode: expect IP address
            let ip: IpAddr = parse_ip_address(address_part)
                .ok_or_else(|| anyhow::anyhow!("Invalid address {}", address_part))?;

            let iface = get_iface_from_ip(&ip)
                .ok_or_else(|| anyhow::anyhow!("IP address not associated with an interface {}", ip))?;

            let is_ipv6 = ip.is_ipv6();
            let address = match ip {
                IpAddr::V4(v4) => format!("{}:0", v4),
                IpAddr::V6(v6) => format!("[{}]:0", v6),
            };

            (address, Some(iface), is_ipv6)
        };

        let port_display = if tunnel {
            let port = address.rsplit(':').next().unwrap_or("0");
            format!(":{}", port)
        } else {
            String::new()
        };

        info!(
            "Load balancer {}: {}{}, contention ratio: {}",
            idx + 1,
            address_part,
            if tunnel { &port_display } else { "" },
            contention_ratio
        );

        load_balancers.push(LoadBalancer::new(address, iface, contention_ratio, is_ipv6));
    }

    Ok(load_balancers)
}

async fn handle_connection(
    mut client: tokio::net::TcpStream,
    pool: Arc<LoadBalancerPool>,
    tunnel: bool,
) {
    if tunnel {
        if let Err(e) = handle_tunnel_connection(client, pool).await {
            warn!("Tunnel connection error: {}", e);
        }
    } else {
        match socks::handle_socks_handshake(&mut client).await {
            Ok((target_addr, target_type)) => {
                if let Err(e) = platform::connect_and_relay(client, &target_addr, target_type, pool).await {
                    warn!("Connection error: {}", e);
                }
            }
            Err(e) => {
                warn!("SOCKS handshake error: {}", e);
            }
        }
    }
}

async fn handle_tunnel_connection(
    client: tokio::net::TcpStream,
    pool: Arc<LoadBalancerPool>,
) -> Result<()> {
    use tokio::io::copy_bidirectional;
    use tokio::net::TcpStream;

    let mut tried = vec![false; pool.len()];

    loop {
        // Tunnel mode doesn't know the target type, use None
        let (lb, idx) = pool.get_load_balancer(Some(&tried), None);

        match TcpStream::connect(&lb.address).await {
            Ok(mut remote) => {
                let mut client = client;
                info!("Tunnelled to {} LB: {}", lb.address, idx);
                let _ = copy_bidirectional(&mut client, &mut remote).await;
                return Ok(());
            }
            Err(e) => {
                warn!("{} {{{}}} LB: {}", lb.address, e, idx);
                tried[idx] = true;

                if tried.iter().all(|&t| t) {
                    warn!("All load balancers failed");
                    return Err(anyhow::anyhow!("All load balancers failed"));
                }
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Handle list mode
    if args.list {
        detect_interfaces();
        return Ok(());
    }

    // Setup logging (do this early for auto-detect feedback)
    if !args.quiet {
        let subscriber = FmtSubscriber::builder()
            .with_max_level(Level::DEBUG)
            .with_target(false)
            .with_thread_ids(false)
            .without_time()
            .finish();
        tracing::subscriber::set_global_default(subscriber)?;
    }

    // Determine load balancers
    let load_balancers = if args.auto {
        if args.tunnel {
            bail!("Auto-detection is not supported in tunnel mode");
        }

        info!("Auto-detecting interfaces with internet connectivity...");
        let working = auto_detect_interfaces().await;

        if working.is_empty() {
            bail!("No interfaces with working internet connectivity found");
        }

        let mut lbs = Vec::new();
        for (idx, (name, ip)) in working.iter().enumerate() {
            let is_ipv6 = ip.is_ipv6();
            let address = match ip {
                IpAddr::V4(v4) => format!("{}:0", v4),
                IpAddr::V6(v6) => format!("[{}]:0", v6),
            };
            info!(
                "Load balancer {}: {} ({}), contention ratio: 1",
                idx + 1,
                ip,
                name
            );
            lbs.push(LoadBalancer::new(address, Some(name.clone()), 1, is_ipv6));
        }
        lbs
    } else {
        // Validate host (supports both IPv4 and IPv6)
        let _: IpAddr = args
            .lhost
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid host {}", args.lhost))?;

        parse_load_balancers(&args.addresses, args.tunnel)?
    };

    let pool = Arc::new(LoadBalancerPool::new(load_balancers));

    // Start server
    let bind_addr = format!("{}:{}", args.lhost, args.lport);
    let listener = TcpListener::bind(&bind_addr).await?;
    info!("Local server started on {}", bind_addr);

    loop {
        match listener.accept().await {
            Ok((socket, _)) => {
                let pool = Arc::clone(&pool);
                let tunnel = args.tunnel;
                tokio::spawn(async move {
                    handle_connection(socket, pool, tunnel).await;
                });
            }
            Err(e) => {
                warn!("Could not accept connection: {}", e);
            }
        }
    }
}
