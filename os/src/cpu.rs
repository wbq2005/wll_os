use alloc::vec::Vec;
use spin::{Mutex, MutexGuard};

/// A small per-CPU container selected by the platform's stable logical CPU ID.
/// Architecture-specific hardware ID lookup remains in `platform`.
pub struct CpuLocal<T> {
    slots: Vec<Mutex<T>>,
}

impl<T> CpuLocal<T> {
    pub fn new_with(mut init: impl FnMut(usize) -> T) -> Self {
        let mut slots = Vec::with_capacity(crate::config::MAX_CPUS);
        for cpu in 0..crate::config::MAX_CPUS {
            slots.push(Mutex::new(init(cpu)));
        }
        Self { slots }
    }

    #[inline]
    pub fn lock(&self) -> MutexGuard<'_, T> {
        self.slots[crate::platform::current_cpu_index()].lock()
    }
}
