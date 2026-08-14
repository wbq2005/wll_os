pub mod la;
pub mod rv;
pub mod user_va;

pub use rv::*;

/// Maximum CPUs supported by the kernel's fixed early-boot resources.
/// Current QEMU finals use up to twelve vCPUs; the selected real boards use fewer.
pub const MAX_CPUS: usize = 12;

/// Each CPU owns a dedicated early/kernel trap stack until a task-specific
/// kernel stack is selected.
pub const SMP_BOOT_STACK_SIZE: usize = 128 * 1024;
