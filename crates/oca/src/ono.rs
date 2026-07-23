use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// An internal object identity, analogous to an AES70 object number (ONo)
/// but not wire-significant: nothing outside this process decodes it yet
/// (see the crate doc comment). Allocated deterministically per
/// `(device_id, channel, class, role)` by [`OnoAllocator`], not packed
/// from any vendor's bit layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Ono(pub u32);

impl std::fmt::Display for Ono {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "0x{:08x}", self.0)
    }
}

impl From<u32> for Ono {
    fn from(v: u32) -> Self {
        Ono(v)
    }
}

impl From<Ono> for u32 {
    fn from(v: Ono) -> Self {
        v.0
    }
}

/// Hands out stable, unique [`Ono`]s for one process run, keyed by the
/// tuple that identifies an object's meaning on a device: which channel,
/// which class, and which human role. Re-asking for the same key (e.g.
/// re-registering a device after a reconnect) returns the same `Ono` it
/// returned before, so addresses stay stable across a device's lifetime
/// within one run - they are not, however, guaranteed stable across a
/// process restart, since allocation order depends on registration order.
#[derive(Debug, Default)]
pub struct OnoAllocator {
    next: u32,
    assigned: HashMap<(String, u16, String, String), Ono>,
}

impl OnoAllocator {
    pub fn new() -> Self {
        // 0 is reserved (mirrors AES70's convention of reserving low ONos
        // for framework objects) so an unset/default Ono is never mistaken
        // for a real allocation.
        Self { next: 1, assigned: HashMap::new() }
    }

    /// Returns the `Ono` for this `(device_id, channel, class, role)`,
    /// allocating a new one the first time it's asked for.
    pub fn allocate(
        &mut self,
        device_id: &str,
        channel: u16,
        class: crate::OcaClass,
        role: &str,
    ) -> Ono {
        let key = (device_id.to_string(), channel, format!("{class:?}"), role.to_string());
        if let Some(ono) = self.assigned.get(&key) {
            return *ono;
        }
        let ono = Ono(self.next);
        self.next += 1;
        self.assigned.insert(key, ono);
        ono
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OcaClass;

    #[test]
    fn allocations_start_after_the_reserved_zero() {
        let mut alloc = OnoAllocator::new();
        let ono = alloc.allocate("x32-1", 1, OcaClass::Gain, "Ch 1 Gain");
        assert_ne!(ono, Ono(0));
    }

    #[test]
    fn same_key_returns_the_same_ono_every_time() {
        let mut alloc = OnoAllocator::new();
        let first = alloc.allocate("x32-1", 3, OcaClass::Gain, "Ch 3 Gain");
        let second = alloc.allocate("x32-1", 3, OcaClass::Gain, "Ch 3 Gain");
        assert_eq!(first, second);
    }

    #[test]
    fn different_keys_get_different_onos() {
        let mut alloc = OnoAllocator::new();
        let gain = alloc.allocate("x32-1", 1, OcaClass::Gain, "Ch 1 Gain");
        let mute = alloc.allocate("x32-1", 1, OcaClass::Mute, "Ch 1 Mute");
        let other_device = alloc.allocate("x32-2", 1, OcaClass::Gain, "Ch 1 Gain");
        let other_channel = alloc.allocate("x32-1", 2, OcaClass::Gain, "Ch 2 Gain");
        assert_ne!(gain, mute);
        assert_ne!(gain, other_device);
        assert_ne!(gain, other_channel);
    }

    #[test]
    fn allocation_order_determines_ono_value_deterministically() {
        // Re-running the exact same sequence of allocate() calls against a
        // fresh allocator always yields the exact same Onos - this is the
        // "deterministic for one process run" guarantee, not stability
        // across restarts with a different registration order.
        let mut a = OnoAllocator::new();
        let mut b = OnoAllocator::new();
        let seq = [("x32-1", 1, OcaClass::Gain), ("x32-1", 1, OcaClass::Switch), ("wing-1", 2, OcaClass::Gain)];
        let run = |alloc: &mut OnoAllocator| -> Vec<Ono> {
            seq.iter().map(|(id, ch, class)| alloc.allocate(id, *ch, *class, "role")).collect()
        };
        assert_eq!(run(&mut a), run(&mut b));
    }
}
