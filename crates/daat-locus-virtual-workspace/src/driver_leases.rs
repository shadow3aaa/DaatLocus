//! Driver-side monitor ownership. Old file cleanup must never remove a connector
//! reused after a power/device generation change. Handles are opaque nonzero values.
use super::driver_protocol::MAX_MONITORS;

#[repr(C)]
#[derive(Default)]
pub struct Token {
    generation: u64,
    connector_plus_one: usize,
    closed: u32,
}

#[derive(Default)]
pub struct Leases {
    generation: u64,
    monitors: [usize; MAX_MONITORS],
}
impl Leases {
    pub fn lookup(&self, token: &Token) -> Option<usize> {
        if token.closed != 0 || token.connector_plus_one == 0 || token.generation != self.generation
        {
            return None;
        }
        let connector = token.connector_plus_one - 1;
        (self.monitors[connector] != 0).then_some(connector)
    }
    pub fn vacant(&self) -> Option<usize> {
        self.monitors.iter().position(|monitor| *monitor == 0)
    }
    pub fn assign(&mut self, connector: usize, monitor: usize) -> Token {
        self.monitors[connector] = monitor;
        Token {
            generation: self.generation,
            connector_plus_one: connector + 1,
            closed: 0,
        }
    }
    pub fn release(&mut self, token: &mut Token) -> Option<usize> {
        // Cleanup can race an already queued IOCTL after the last OS handle closes.
        // Remember closure even for an empty/stale lease so that request cannot create.
        token.closed = 1;
        let slot = std::mem::take(&mut token.connector_plus_one);
        if slot == 0 || token.generation != self.generation {
            return None;
        }
        let monitor = std::mem::take(&mut self.monitors[slot - 1]);
        (monitor != 0).then_some(monitor)
    }
    pub fn retire(&mut self) -> [usize; MAX_MONITORS] {
        self.generation = self.generation.wrapping_add(1);
        std::mem::replace(&mut self.monitors, [0; MAX_MONITORS])
    }
}
impl Token {
    pub fn is_closed(&self) -> bool {
        self.closed != 0
    }
    pub fn is_assigned(&self) -> bool {
        self.connector_plus_one != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cleanup_is_per_file_and_retired_cleanup_cannot_remove_reused_connectors() {
        let mut pool = Leases::default();
        let mut first = pool.assign(0, 101);
        let mut second = pool.assign(1, 202);
        assert_eq!(pool.release(&mut first), Some(101));
        assert!(first.is_closed());
        assert_eq!(pool.release(&mut first), None);
        assert_eq!(pool.vacant(), Some(0));
        let retired = pool.retire();
        assert_eq!(retired[0], 0);
        assert_eq!(retired[1], 202);
        let mut replacement = pool.assign(1, 303);
        assert_eq!(pool.lookup(&second), None);
        assert_eq!(pool.lookup(&replacement), Some(1));
        assert_eq!(pool.release(&mut second), None);
        assert_eq!(pool.release(&mut replacement), Some(303));
        let mut empty_file = Token::default();
        assert_eq!(pool.release(&mut empty_file), None);
        assert!(empty_file.is_closed());
    }
}
