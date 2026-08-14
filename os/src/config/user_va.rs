use super::{PAGE_SIZE, USER_STACK_TOP};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UserVaRange {
    pub start: usize,
    pub end: usize,
}

impl UserVaRange {
    pub const fn contains_range(self, start: usize, end: usize) -> bool {
        start >= self.start && start < end && end <= self.end
    }

    pub const fn contains_addr(self, addr: usize) -> bool {
        addr >= self.start && addr < self.end
    }
}

pub const LOW_USER_RANGE: UserVaRange = UserVaRange {
    start: PAGE_SIZE,
    end: USER_STACK_TOP,
};

// Keep the overflow arena in the upper half of the positive Sv39 user range.
// Sv39 positive canonical addresses use root indices 0..255; indices 128..255
// cover 0x20_0000_0000..0x40_0000_0000.  LoongArch also accepts this range
// through PGDL, while its PLV0 DMW windows remain in the separate high kernel
// address space.  The kernel keeps the low arena for the historical ABI and
// uses this disjoint range only when the low arena cannot satisfy a reservation.
pub const HIGH_MMAP_ARENA: UserVaRange = UserVaRange {
    start: 0x20_0000_0000,
    end: 0x40_0000_0000,
};

pub const LOW_MMAP_ARENA: UserVaRange = UserVaRange {
    start: 0x2000_0000,
    end: USER_STACK_TOP,
};

pub const MMAP_ARENAS: [UserVaRange; 2] = [LOW_MMAP_ARENA, HIGH_MMAP_ARENA];

pub const DEFAULT_MMAP_BASE: usize = LOW_MMAP_ARENA.start;
pub const DEFAULT_HIGH_MMAP_BASE: usize = HIGH_MMAP_ARENA.start;

// Callers that scan all user mappings (shared-file writeback, diagnostics)
// need a bound covering both disjoint arenas.  ELF stack placement continues
// to use USER_STACK_TOP directly.
pub const USER_ADDRESS_LIMIT: usize = HIGH_MMAP_ARENA.end;

pub const HAS_HIGH_MMAP_ARENA: bool = true;

pub const fn is_user_mappable_range(start: usize, end: usize) -> bool {
    if LOW_USER_RANGE.contains_range(start, end) {
        return true;
    }
    HIGH_MMAP_ARENA.contains_range(start, end)
}

pub const fn mmap_arena_containing(addr: usize) -> Option<UserVaRange> {
    let mut index = 0;
    while index < MMAP_ARENAS.len() {
        if MMAP_ARENAS[index].contains_addr(addr) {
            return Some(MMAP_ARENAS[index]);
        }
        index += 1;
    }
    None
}
