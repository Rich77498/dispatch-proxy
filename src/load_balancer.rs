use std::sync::Mutex;

/// A single load balancer endpoint
#[derive(Debug, Clone)]
pub struct LoadBalancer {
    pub address: String,
    pub iface: Option<String>,
    pub contention_ratio: u32,
}

impl LoadBalancer {
    pub fn new(address: String, iface: Option<String>, contention_ratio: u32) -> Self {
        Self {
            address,
            iface,
            contention_ratio,
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
    pub fn get_load_balancer(&self, skip: Option<&[bool]>) -> (LoadBalancer, usize) {
        let mut state = self.state.lock().unwrap();

        // Skip already-tried balancers if provided
        if let Some(skip_list) = skip {
            while skip_list.get(state.current_index).copied().unwrap_or(false) {
                state.current_connections = 0;
                state.current_index = (state.current_index + 1) % self.balancers.len();
            }
        }

        let lb = &self.balancers[state.current_index];
        let idx = state.current_index;

        state.current_connections += 1;

        if state.current_connections >= lb.contention_ratio {
            state.current_connections = 0;
            state.current_index = (state.current_index + 1) % self.balancers.len();
        }

        (lb.clone(), idx)
    }
}
