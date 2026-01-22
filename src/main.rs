mod load_balancer;
mod platform;
mod socks;

use anyhow::{bail, Result};
use clap::Parser;
use load_balancer::{LoadBalancer, LoadBalancerPool};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
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

    /// Load balancer addresses (IP@ratio or host:port@ratio for tunnel mode)
    addresses: Vec<String>,
}

/// Detect and list available network interfaces
fn detect_interfaces() {
    println!("--- Listing the available addresses for dispatching");

    if let Ok(interfaces) = get_if_addrs::get_if_addrs() {
        for iface in interfaces {
            if !iface.is_loopback() {
                if let IpAddr::V4(ipv4) = iface.ip() {
                    println!("[+] {}, IPv4:{}", iface.name, ipv4);
                }
            }
        }
    }
}

/// Get interface name from IP address
fn get_iface_from_ip(ip: &Ipv4Addr) -> Option<String> {
    if let Ok(interfaces) = get_if_addrs::get_if_addrs() {
        for iface in interfaces {
            if !iface.is_loopback() {
                if let IpAddr::V4(ipv4) = iface.ip() {
                    if &ipv4 == ip {
                        return Some(iface.name);
                    }
                }
            }
        }
    }
    None
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

        let (address, iface) = if tunnel {
            // Tunnel mode: expect host:port format
            let host_port: Vec<&str> = address_part.split(':').collect();
            if host_port.len() != 2 {
                bail!("Invalid address specification {}", address_part);
            }

            let port: u16 = host_port[1]
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid port {}", address_part))?;

            if port == 0 {
                bail!("Invalid port {}", address_part);
            }

            (format!("{}:{}", host_port[0], port), None)
        } else {
            // Normal mode: expect IP address
            let ip: Ipv4Addr = address_part
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid address {}", address_part))?;

            let iface = get_iface_from_ip(&ip)
                .ok_or_else(|| anyhow::anyhow!("IP address not associated with an interface {}", ip))?;

            (format!("{}:0", ip), Some(iface))
        };

        let port_display = if tunnel {
            format!(":{}", address.split(':').nth(1).unwrap_or("0"))
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

        load_balancers.push(LoadBalancer::new(address, iface, contention_ratio));
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
            Ok(target_addr) => {
                if let Err(e) = platform::connect_and_relay(client, &target_addr, pool).await {
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
        let (lb, idx) = pool.get_load_balancer(Some(&tried));

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

    // Setup logging
    if !args.quiet {
        let subscriber = FmtSubscriber::builder()
            .with_max_level(Level::DEBUG)
            .with_target(false)
            .with_thread_ids(false)
            .without_time()
            .finish();
        tracing::subscriber::set_global_default(subscriber)?;
    }

    // Validate host
    let _: Ipv4Addr = args
        .lhost
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid host {}", args.lhost))?;

    // Parse load balancers
    let load_balancers = parse_load_balancers(&args.addresses, args.tunnel)?;
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
