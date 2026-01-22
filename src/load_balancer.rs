use std::sync::Mutex;

/// Target address type from SOCKS5 request
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TargetAddressType {
    IPv4,
    IPv6,
    Domain,
}

/// A single load balancer endpoint
#[derive(Debug, Clone)]
pub struct LoadBalancer {
    pub address: String,
    pub iface: Option<String>,
    pub contention_ratio: u32,
    pub is_ipv6: bool,
}

impl LoadBalancer {
    pub fn new(address: String, iface: Option<String>, contention_ratio: u32, is_ipv6: bool) -> Self {
        Self {
            address,
            iface,
            contention_ratio,
            is_ipv6,
        }
    }
}

/// Thread-safe pool of load balancers with weighted round-robin selection
pub struct LoadBalancerPool {
    balancers: Vec<LoadBalancer>,
    state: Mutex<PoolState>,
}

struct PoolState {
    current_index: usize,
    current_connections: u32,
}

impl LoadBalancerPool {
    pub fn new(balancers: Vec<LoadBalancer>) -> Self {
        Self {
            balancers,
            state: Mutex::new(PoolState {
                current_index: 0,
                current_connections: 0,
            }),
        }
    }

    pub fn len(&self) -> usize {
        self.balancers.len()
    }

    /// Get the next load balancer according to contention ratio.
    /// If `skip` is provided, skip balancers marked as true in the slice.
    /// If `target_type` is provided, only select balancers matching the address family.
    pub fn get_load_balancer(&self, skip: Option<&[bool]>, target_type: Option<TargetAddressType>) -> (LoadBalancer, usize) {
        let mut state = self.state.lock().unwrap();

        // For address family matching:
        // - IPv4 target -> prefer IPv4 interfaces
        // - IPv6 target -> prefer IPv6 interfaces
        // - Domain -> use any interface (DNS will determine)
        let family_filter = |lb: &LoadBalancer| -> bool {
            match target_type {
                Some(TargetAddressType::IPv4) => !lb.is_ipv6,
                Some(TargetAddressType::IPv6) => lb.is_ipv6,
                Some(TargetAddressType::Domain) | None => true,
            }
        };

        // Count available balancers (not skipped and matching family)
        let available_count = self.balancers.iter().enumerate().filter(|(i, lb)| {
            let not_skipped = skip.map_or(true, |s| !s.get(*i).copied().unwrap_or(false));
            not_skipped && family_filter(lb)
        }).count();

        // If no balancers match the family, fall back to any available (for Domain or mixed scenarios)
        let use_family_filter = available_count > 0;

        // Find next valid balancer
        let start_index = state.current_index;
        let mut iterations = 0;

        loop {
            let idx = state.current_index;
            let lb = &self.balancers[idx];

            let is_skipped = skip.map_or(false, |s| s.get(idx).copied().unwrap_or(false));
            let matches_family = !use_family_filter || family_filter(lb);

            if !is_skipped && matches_family {
                // Found a valid balancer
                state.current_connections += 1;

                if state.current_connections >= lb.contention_ratio {
                    state.current_connections = 0;
                    state.current_index = (state.current_index + 1) % self.balancers.len();
                }

                return (lb.clone(), idx);
            }

            // Move to next
            state.current_connections = 0;
            state.current_index = (state.current_index + 1) % self.balancers.len();
            iterations += 1;

            // If we've checked all balancers and found none, return current anyway
            if iterations >= self.balancers.len() {
                // Fall back to first non-skipped balancer
                for (i, lb) in self.balancers.iter().enumerate() {
                    let is_skipped = skip.map_or(false, |s| s.get(i).copied().unwrap_or(false));
                    if !is_skipped {
                        return (lb.clone(), i);
                    }
                }
                // If all are skipped, return current index anyway
                return (self.balancers[start_index].clone(), start_index);
            }
        }
    }
}
