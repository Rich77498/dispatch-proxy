#[cfg(target_os = "linux")]
mod linux;

#[cfg(not(target_os = "linux"))]
mod generic;

use crate::load_balancer::{LoadBalancerPool, TargetAddressType};
use crate::socks;
use anyhow::Result;
use std::sync::Arc;
use tokio::net::TcpStream;
use tracing::{info, warn};

#[cfg(target_os = "linux")]
use linux::connect_with_interface;

#[cfg(not(target_os = "linux"))]
use generic::connect_with_interface;

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
