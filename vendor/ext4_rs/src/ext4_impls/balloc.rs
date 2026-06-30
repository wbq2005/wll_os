use crate::ext4_defs::*;
use crate::prelude::*;
use crate::return_errno_with_message;
use crate::utils::bitmap::*;
use core::array;

// Cache for block group information
#[derive(Clone, Copy)]
struct BlockGroupCache {
    bitmap: [u8; BLOCK_SIZE],
    free_blocks: u64,
    last_used_idx: u32,
}

impl BlockGroupCache {
    fn new(bitmap: &[u8], free_blocks: u64) -> Self {
        let mut new_bitmap = [0u8; BLOCK_SIZE];
        new_bitmap.copy_from_slice(bitmap);
        Self {
            bitmap: new_bitmap,
            free_blocks,
            last_used_idx: 0,
        }
    }
}

// Simple fixed-size cache for block groups
struct BlockGroupCacheManager {
    caches: [(u32, BlockGroupCache); 8], // Cache for up to 8 block groups
    len: usize,
}

impl BlockGroupCacheManager {
    fn new() -> Self {
        let empty_cache = BlockGroupCache {
            bitmap: [0; BLOCK_SIZE],
            free_blocks: 0,
            last_used_idx: 0,
        };
        Self {
            caches: array::from_fn(|_| (0, empty_cache)),
            len: 0,
        }
    }

    fn get(&mut self, bgid: u32) -> Option<&mut BlockGroupCache> {
        for i in 0..self.len {
            if self.caches[i].0 == bgid {
                return Some(&mut self.caches[i].1);
            }
        }
        None
    }

    fn insert(&mut self, bgid: u32, cache: BlockGroupCache) {
        if self.len < 8 {
            self.caches[self.len] = (bgid, cache);
            self.len += 1;
        } else {
            // Simple LRU: remove the first entry and shift others
            for i in 0..self.len-1 {
                self.caches[i] = self.caches[i+1];
            }
            self.caches[self.len-1] = (bgid, cache);
        }
    }

    fn iter_caches(&self) -> impl Iterator<Item = &(u32, BlockGroupCache)> {
        self.caches[..self.len].iter()
    }
}

impl Ext4 {
    fn balloc_super_block(&self) -> Ext4Superblock {
        let block = Block::load(&self.block_device, SUPERBLOCK_OFFSET);
        block.read_as()
    }

    fn mark_system_zone_bits(
        &self,
        super_block: &Ext4Superblock,
        bgid: u32,
        bitmap: &mut [u8],
    ) {
        let start = self.get_block_of_bgid(bgid);
        let end = start + super_block.blocks_per_group() as u64;
        if let Some(zones) = &self.system_zone_cache {
            for zone in zones {
                let zone_start = zone.start_blk.max(start);
                let zone_end = (zone.end_blk + 1).min(end);
                if zone_start >= zone_end {
                    continue;
                }
                for block in zone_start..zone_end {
                    ext4_bmap_bit_set(bitmap, self.addr_to_idx_bg(block));
                }
            }
        }

        // The bitmap covers a whole group even when the final group extends
        // beyond the filesystem. Those tail bits must never be allocatable.
        let blocks_count = super_block.blocks_count();
        for block in blocks_count.max(start)..end {
            ext4_bmap_bit_set(bitmap, self.addr_to_idx_bg(block));
        }
    }

    fn bitmap_free_blocks(bitmap: &[u8], block_count: u32) -> u32 {
        let full_bytes = (block_count / 8) as usize;
        let tail_bits = block_count % 8;
        let mut used = bitmap[..full_bytes]
            .iter()
            .map(|byte| byte.count_ones())
            .sum::<u32>();
        if tail_bits != 0 {
            let mask = (1u8 << tail_bits) - 1;
            used += (bitmap[full_bytes] & mask).count_ones();
        }
        block_count - used
    }

    fn has_allocatable_clear_bit(
        &self,
        super_block: &Ext4Superblock,
        bgid: u32,
        bitmap: &[u8],
    ) -> bool {
        let start = self.get_block_of_bgid(bgid);
        let group_end = start + super_block.blocks_per_group() as u64;
        let end = group_end.min(super_block.blocks_count());

        for block in start..end {
            let idx = self.addr_to_idx_bg(block);
            if ext4_bmap_is_bit_clr(bitmap, idx) && !self.is_system_reserved_block(block, bgid) {
                return true;
            }
        }

        false
    }

    fn mark_block_if_in_group(&self, super_block: &Ext4Superblock, bgid: u32, bitmap: &mut [u8], block: u64) {
        if block == 0 || block >= super_block.blocks_count() {
            return;
        }

        if self.get_bgid_of_block(block) == bgid {
            ext4_bmap_bit_set(bitmap, self.addr_to_idx_bg(block));
        }
    }

    fn mark_block_range_if_in_group(
        &self,
        super_block: &Ext4Superblock,
        bgid: u32,
        bitmap: &mut [u8],
        start_block: u64,
        count: u32,
    ) {
        let group_start = self.get_block_of_bgid(bgid);
        let group_end = (group_start + super_block.blocks_per_group() as u64)
            .min(super_block.blocks_count());
        let range_start = start_block.max(group_start);
        let range_end = start_block
            .saturating_add(count as u64)
            .min(group_end);

        if range_start >= range_end {
            return;
        }

        for block in range_start..range_end {
            ext4_bmap_bit_set(bitmap, self.addr_to_idx_bg(block));
        }
    }

    fn mark_extent_node_blocks(
        &self,
        super_block: &Ext4Superblock,
        bgid: u32,
        bitmap: &mut [u8],
        data: &[u8],
        is_root: bool,
    ) {
        let node = match ExtentNode::load_from_data(data, is_root) {
            Ok(node) => node,
            Err(_) => return,
        };

        if node.header.magic != EXT4_EXTENT_MAGIC {
            return;
        }

        let entries = core::cmp::min(
            node.header.entries_count as usize,
            node.header.max_entries_count as usize,
        );

        if entries == 0 {
            return;
        }

        for pos in 0..entries {
            if node.header.depth == 0 {
                if let Some(extent) = node.get_extent(pos) {
                    self.mark_block_range_if_in_group(
                        super_block,
                        bgid,
                        bitmap,
                        extent.get_pblock(),
                        extent.get_actual_len() as u32,
                    );
                }
            } else if let Ok(index) = node.get_index(pos) {
                let child_block = index.get_pblock();
                self.mark_block_if_in_group(super_block, bgid, bitmap, child_block);

                if child_block < super_block.blocks_count() {
                    let child_data = self
                        .block_device
                        .read_offset(child_block as usize * BLOCK_SIZE);
                    self.mark_extent_node_blocks(super_block, bgid, bitmap, &child_data, false);
                }
            }
        }
    }

    fn mark_legacy_inode_blocks(
        &self,
        super_block: &Ext4Superblock,
        bgid: u32,
        bitmap: &mut [u8],
        inode: &Ext4Inode,
    ) {
        for block in inode.block.iter().take(12) {
            self.mark_block_if_in_group(super_block, bgid, bitmap, *block as u64);
        }

        // Mark indirect pointer blocks conservatively. Recursing into the full
        // legacy tree is unnecessary for the current ext4 images, but these
        // pointer blocks are allocated metadata and must not be reused.
        for block in inode.block.iter().skip(12) {
            self.mark_block_if_in_group(super_block, bgid, bitmap, *block as u64);
        }
    }

    fn rebuild_balloc_bitmap_from_metadata(
        &self,
        super_block: &Ext4Superblock,
        bgid: u32,
        expected_free: u64,
    ) -> Vec<u8> {
        let mut rebuilt = vec![0u8; BLOCK_SIZE];
        self.mark_system_zone_bits(super_block, bgid, &mut rebuilt);

        let group_count = super_block.block_group_count();
        for inode_bgid in 0..group_count {
            let mut inode_group =
                Ext4BlockGroup::load_new(&self.block_device, super_block, inode_bgid as usize);
            let inode_bitmap_block = inode_group.get_inode_bitmap_block(super_block);
            let inode_bitmap = self
                .block_device
                .read_offset(inode_bitmap_block as usize * BLOCK_SIZE);
            let inodes_in_group = super_block.get_inodes_in_group_cnt(inode_bgid);

            for idx in 0..inodes_in_group {
                if !ext4_bmap_is_bit_set(&inode_bitmap, idx) {
                    continue;
                }

                let inode_num = inode_bgid * super_block.inodes_per_group() + idx + 1;
                let inode_ref = self.get_inode_ref(inode_num);
                let inode = inode_ref.inode;

                if inode.mode() == 0 || inode.dtime() != 0 {
                    continue;
                }

                if (inode.flags() & EXT4_INODE_FLAG_EXTENTS as u32) != 0 {
                    let root_data = unsafe {
                        core::slice::from_raw_parts(
                            inode.block.as_ptr() as *const u8,
                            inode.block.len() * size_of::<u32>(),
                        )
                    };
                    self.mark_extent_node_blocks(super_block, bgid, &mut rebuilt, root_data, true);
                } else {
                    self.mark_legacy_inode_blocks(super_block, bgid, &mut rebuilt, &inode);
                }
            }
        }

        // If the metadata walk misses format-specific reservations, keep the
        // descriptor free count as an upper bound by reserving high blocks until
        // the rebuilt bitmap is no more permissive than the descriptor.
        let mut rebuilt_free =
            Self::bitmap_free_blocks(&rebuilt, super_block.blocks_per_group()) as u64;
        let mut idx = super_block.blocks_per_group();
        while rebuilt_free > expected_free && idx > 0 {
            idx -= 1;
            if ext4_bmap_is_bit_clr(&rebuilt, idx) {
                ext4_bmap_bit_set(&mut rebuilt, idx);
                rebuilt_free -= 1;
            }
        }

        rebuilt
    }

    fn reconcile_super_free_blocks(&self, old_free: u64, new_free: u64) {
        if old_free == new_free {
            return;
        }

        let mut super_block = self.balloc_super_block();
        let current = super_block.free_blocks_count();
        let reconciled = if new_free > old_free {
            current.saturating_add(new_free - old_free)
        } else {
            current.saturating_sub(old_free - new_free)
        };
        super_block.set_free_blocks_count(reconciled);
        super_block.sync_to_disk_with_csum(&self.block_device);
    }

    fn load_balloc_bitmap_for_alloc(
        &self,
        super_block: &Ext4Superblock,
        bgid: u32,
        block_group: &mut Ext4BlockGroup,
    ) -> Vec<u8> {
        let bmp_blk_adr = block_group.get_block_bitmap_block(super_block);
        let mut bitmap = self
            .block_device
            .read_offset(bmp_blk_adr as usize * BLOCK_SIZE);

        let old_free = block_group.get_free_blocks_count();
        let bitmap_has_allocatable =
            self.has_allocatable_clear_bit(super_block, bgid, &bitmap);
        if old_free > 0 && !bitmap_has_allocatable {
            bitmap = self.rebuild_balloc_bitmap_from_metadata(super_block, bgid, old_free);
            let rebuilt_free =
                Self::bitmap_free_blocks(&bitmap, super_block.blocks_per_group()) as u64;
            if rebuilt_free != old_free {
                block_group.set_free_blocks_count(rebuilt_free as u32);
                self.reconcile_super_free_blocks(old_free, rebuilt_free);
            }
            if block_group.has_block_uninit() {
                block_group.clear_block_uninit();
            }
            block_group.set_block_group_balloc_bitmap_csum(super_block, &bitmap);
            self.block_device
                .write_offset(bmp_blk_adr as usize * BLOCK_SIZE, &bitmap);
            block_group.sync_to_disk_with_csum(&self.block_device, bgid as usize, super_block);
            return bitmap;
        }

        if !block_group.has_block_uninit() {
            return bitmap;
        }

        self.mark_system_zone_bits(super_block, bgid, &mut bitmap);
        let initialized_free =
            Self::bitmap_free_blocks(&bitmap, super_block.blocks_per_group()) as u64;
        if initialized_free != old_free {
            block_group.set_free_blocks_count(initialized_free as u32);
            self.reconcile_super_free_blocks(old_free, initialized_free);
        }

        block_group.clear_block_uninit();
        block_group.set_block_group_balloc_bitmap_csum(super_block, &bitmap);
        self.block_device
            .write_offset(bmp_blk_adr as usize * BLOCK_SIZE, &bitmap);
        block_group.sync_to_disk_with_csum(&self.block_device, bgid as usize, super_block);
        bitmap
    }

    /// Compute number of block group from block address.
    ///
    /// Params:
    ///
    /// `baddr` - Absolute address of block.
    ///
    /// # Returns
    /// `u32` - Block group index
    pub fn get_bgid_of_block(&self, baddr: u64) -> u32 {
        let mut baddr = baddr;
        if self.super_block.first_data_block() != 0 && baddr != 0 {
            baddr -= 1;
        }
        (baddr / self.super_block.blocks_per_group() as u64) as u32
    }

    /// Compute the starting block address of a block group.
    ///
    /// Params:
    /// `bgid` - Block group index
    ///
    /// Returns:
    /// `u64` - Block address
    pub fn get_block_of_bgid(&self, bgid: u32) -> u64 {
        let mut baddr = 0;
        if self.super_block.first_data_block() != 0 {
            baddr += 1;
        }
        baddr + bgid as u64 * self.super_block.blocks_per_group() as u64
    }

    /// Convert block address to relative index in block group.
    ///
    /// Params:
    /// `baddr` - Block number to convert.
    ///
    /// Returns:
    /// `u32` - Relative number of block.
    pub fn addr_to_idx_bg(&self, baddr: u64) -> u32 {
        let mut baddr = baddr;
        if self.super_block.first_data_block() != 0 && baddr != 0 {
            baddr -= 1;
        }
        (baddr % self.super_block.blocks_per_group() as u64) as u32
    }

    /// Convert relative block address in group to absolute address.
    ///
    /// # Arguments
    ///
    /// * `index` - Relative block address.
    /// * `bgid` - Block group.
    ///
    /// # Returns
    ///
    /// * `Ext4Fsblk` - Absolute block address.
    pub fn bg_idx_to_addr(&self, index: u32, bgid: u32) -> Ext4Fsblk {
        let mut index = index;
        if self.super_block.first_data_block() != 0 {
            index += 1;
        }
        (self.super_block.blocks_per_group() as u64 * bgid as u64) + index as u64
    }


    /// Allocate a new block.
    ///
    /// Params:
    /// `inode_ref` - Reference to the inode.
    /// `goal` - Absolute address of the block.
    ///
    /// Returns:
    /// `Result<Ext4Fsblk>` - The physical block number allocated.
    pub fn balloc_alloc_block(
        &self,
        inode_ref: &mut Ext4InodeRef,
        goal: Option<Ext4Fsblk>,
    ) -> Result<Ext4Fsblk> {
        let mut alloc: Ext4Fsblk = 0;
        let super_block = &self.super_block;
        let blocks_per_group = super_block.blocks_per_group();
        let mut bgid;
        let mut idx_in_bg;

        if let Some(goal) = goal {
            bgid = self.get_bgid_of_block(goal);
            idx_in_bg = self.addr_to_idx_bg(goal);
        } else {
            bgid = 1;
            idx_in_bg = 0;
        }

        let block_group_count = super_block.block_group_count();
        let mut count = block_group_count;

        while count > 0 {
            // Load block group reference
            let mut block_group =
                Ext4BlockGroup::load_new(&self.block_device, super_block, bgid as usize);

            let free_blocks = block_group.get_free_blocks_count();
            if free_blocks == 0 {
                // Try next block group
                bgid = (bgid + 1) % block_group_count;
                count -= 1;

                if count == 0 {
                    log::trace!("No free blocks available in all block groups");
                    return_errno_with_message!(Errno::ENOSPC, "No free blocks available in all block groups");
                }
                continue;
            }

            // Compute indexes
            let first_in_bg = self.get_block_of_bgid(bgid);
            let first_in_bg_index = self.addr_to_idx_bg(first_in_bg);

            if idx_in_bg < first_in_bg_index {
                idx_in_bg = first_in_bg_index;
            }

            // Load block with bitmap
            let bmp_blk_adr = block_group.get_block_bitmap_block(super_block);
            let mut bitmap_data =
                self.load_balloc_bitmap_for_alloc(super_block, bgid, &mut block_group);

            // Check if goal is free
            if ext4_bmap_is_bit_clr(&bitmap_data, idx_in_bg) {
                let block_num = self.bg_idx_to_addr(idx_in_bg, bgid);
                if self.is_system_reserved_block(block_num, bgid) {
                    // 跳过 system zone
                } else {
                    ext4_bmap_bit_set(&mut bitmap_data, idx_in_bg);
                    block_group.set_block_group_balloc_bitmap_csum(super_block, &bitmap_data);
                    self.block_device
                        .write_offset(bmp_blk_adr as usize * BLOCK_SIZE, &bitmap_data);
                    alloc = self.bg_idx_to_addr(idx_in_bg, bgid);

                    /* Update free block counts */
                    self.update_free_block_counts(inode_ref, &mut block_group, bgid as usize)?;
                    return Ok(alloc);
                }
            }

            // Try to find free block near to goal
            let blk_in_bg = blocks_per_group;
            let end_idx = min((idx_in_bg + 63) & !63, blk_in_bg);

            for tmp_idx in (idx_in_bg + 1)..end_idx {
                if ext4_bmap_is_bit_clr(&bitmap_data, tmp_idx) {
                    // Check if this is a system reserved block
                    let block_num = self.bg_idx_to_addr(tmp_idx, bgid);
                    if self.is_system_reserved_block(block_num, bgid) {
                        continue;
                    }
                    
                    ext4_bmap_bit_set(&mut bitmap_data, tmp_idx);
                    block_group.set_block_group_balloc_bitmap_csum(super_block, &bitmap_data);
                    self.block_device
                        .write_offset(bmp_blk_adr as usize * BLOCK_SIZE, &bitmap_data);
                    alloc = self.bg_idx_to_addr(tmp_idx, bgid);
                    self.update_free_block_counts(inode_ref, &mut block_group, bgid as usize)?;
                    return Ok(alloc);
                }
            }

            // Find a non-metadata free bit in this group. ext4 images may leave a
            // metadata bit clear; that must not make the whole group look full.
            let mut search_idx = idx_in_bg;
            while search_idx < blk_in_bg {
                let mut rel_blk_idx = 0;
                if !ext4_bmap_bit_find_clr(&bitmap_data, search_idx, blk_in_bg, &mut rel_blk_idx) {
                    break;
                }
                let block_num = self.bg_idx_to_addr(rel_blk_idx, bgid);
                if self.is_system_reserved_block(block_num, bgid) {
                    search_idx = rel_blk_idx.saturating_add(1);
                    continue;
                }
                ext4_bmap_bit_set(&mut bitmap_data, rel_blk_idx);
                block_group.set_block_group_balloc_bitmap_csum(super_block, &bitmap_data);
                self.block_device
                    .write_offset(bmp_blk_adr as usize * BLOCK_SIZE, &bitmap_data);
                alloc = block_num;
                self.update_free_block_counts(inode_ref, &mut block_group, bgid as usize)?;
                return Ok(alloc);
            }

            // No free block found in this group, try other block groups
            bgid = (bgid + 1) % block_group_count;
            count -= 1;
        }

        return_errno_with_message!(Errno::ENOSPC, "No free blocks available in all block groups");
    }

    /// Allocate a new block start from a specific bgid
    ///
    /// Params:
    /// `inode_ref` - Reference to the inode.
    /// `start_bgid` - Start bgid of free block search
    ///
    /// Returns:
    /// `Result<Ext4Fsblk>` - The physical block number allocated.
    pub fn balloc_alloc_block_from(
        &self,
        inode_ref: &mut Ext4InodeRef,
        start_bgid: &mut u32,
    ) -> Result<Ext4Fsblk> {
        let mut alloc: Ext4Fsblk = 0;
        let super_block = &self.super_block;
        let blocks_per_group = super_block.blocks_per_group();
        // Maximum number of blocks that can be represented by a bitmap block
        let max_blocks_in_bitmap = BLOCK_SIZE * 8;

        let mut bgid = *start_bgid;
        let mut idx_in_bg = 0;

        let block_group_count = super_block.block_group_count();
        let mut count = block_group_count;

        while count > 0 {
            // Load block group reference
            let mut block_group =
                Ext4BlockGroup::load_new(&self.block_device, super_block, bgid as usize);

            let free_blocks = block_group.get_free_blocks_count();
            if free_blocks == 0 {
                // Try next block group
                bgid = (bgid + 1) % block_group_count;
                count -= 1;

                if count == 0 {
                    log::trace!("No free blocks available in all block groups");
                    return_errno_with_message!(Errno::ENOSPC, "No free blocks available in all block groups");
                }
                continue;
            }

            // Compute indexes
            let first_in_bg = self.get_block_of_bgid(bgid);
            let first_in_bg_index = self.addr_to_idx_bg(first_in_bg);

            if idx_in_bg < first_in_bg_index {
                idx_in_bg = first_in_bg_index;
            }

            // Ensure idx_in_bg doesn't exceed bitmap size
            if idx_in_bg >= max_blocks_in_bitmap as u32 {
                // Try next block group if we've reached the end of this bitmap
                bgid = (bgid + 1) % block_group_count;
                count -= 1;
                idx_in_bg = 0;
                continue;
            }

            // Load block with bitmap
            let bmp_blk_adr = block_group.get_block_bitmap_block(super_block);
            let mut bitmap_data =
                self.load_balloc_bitmap_for_alloc(super_block, bgid, &mut block_group);

            // Check if goal is free
            if ext4_bmap_is_bit_clr(&bitmap_data, idx_in_bg) {
                let block_num = self.bg_idx_to_addr(idx_in_bg, bgid);
                if !self.is_system_reserved_block(block_num, bgid) {
                    ext4_bmap_bit_set(&mut bitmap_data, idx_in_bg);
                    block_group.set_block_group_balloc_bitmap_csum(super_block, &bitmap_data);
                    self.block_device
                        .write_offset(bmp_blk_adr as usize * BLOCK_SIZE, &bitmap_data);
                    alloc = block_num;

                    /* Update free block counts */
                    self.update_free_block_counts(inode_ref, &mut block_group, bgid as usize)?;

                    *start_bgid = bgid;
                    return Ok(alloc);
                }
            }

            // Try to find free block near to goal
            let end_idx = min((idx_in_bg + 63) & !63, max_blocks_in_bitmap as u32);

            for tmp_idx in (idx_in_bg + 1)..end_idx {
                if ext4_bmap_is_bit_clr(&bitmap_data, tmp_idx) {
                    // Check if this is a system reserved block
                    let block_num = self.bg_idx_to_addr(tmp_idx, bgid);
                    if self.is_system_reserved_block(block_num, bgid) {
                        continue;
                    }
                    
                    ext4_bmap_bit_set(&mut bitmap_data, tmp_idx);
                    block_group.set_block_group_balloc_bitmap_csum(super_block, &bitmap_data);
                    self.block_device
                        .write_offset(bmp_blk_adr as usize * BLOCK_SIZE, &bitmap_data);
                    alloc = self.bg_idx_to_addr(tmp_idx, bgid);
                    self.update_free_block_counts(inode_ref, &mut block_group, bgid as usize)?;

                    *start_bgid = bgid;
                    return Ok(alloc);
                }
            }

            // Find a non-metadata free bit in this group.
            let mut search_idx = idx_in_bg;
            while search_idx < max_blocks_in_bitmap as u32 {
                let mut rel_blk_idx = 0;
                if !ext4_bmap_bit_find_clr(
                    &bitmap_data,
                    search_idx,
                    max_blocks_in_bitmap as u32,
                    &mut rel_blk_idx,
                ) {
                    break;
                }
                let block_num = self.bg_idx_to_addr(rel_blk_idx, bgid);
                if self.is_system_reserved_block(block_num, bgid) {
                    search_idx = rel_blk_idx.saturating_add(1);
                    continue;
                }
                ext4_bmap_bit_set(&mut bitmap_data, rel_blk_idx);
                block_group.set_block_group_balloc_bitmap_csum(super_block, &bitmap_data);
                self.block_device
                    .write_offset(bmp_blk_adr as usize * BLOCK_SIZE, &bitmap_data);
                alloc = block_num;
                self.update_free_block_counts(inode_ref, &mut block_group, bgid as usize)?;

                *start_bgid = bgid;
                return Ok(alloc);
            }

            // No free block found in this group, try other block groups
            bgid = (bgid + 1) % block_group_count;
            count -= 1;
            idx_in_bg = 0;
        }

        return_errno_with_message!(Errno::ENOSPC, "No free blocks available in all block groups");
    }

    fn update_free_block_counts(
        &self,
        inode_ref: &mut Ext4InodeRef,
        block_group: &mut Ext4BlockGroup,
        bgid: usize,
    ) -> Result<()> {
        let mut super_block = self.balloc_super_block();
        let block_size = BLOCK_SIZE as u64;

        // Update superblock free blocks count
        let mut super_blk_free_blocks = super_block.free_blocks_count();
        super_blk_free_blocks -= 1;
        super_block.set_free_blocks_count(super_blk_free_blocks);
        super_block.sync_to_disk_with_csum(&self.block_device);

        // Update inode blocks (different block size!) count
        let mut inode_blocks = inode_ref.inode.blocks_count();
        inode_blocks += block_size / EXT4_INODE_BLOCK_SIZE as u64;
        inode_ref.inode.set_blocks_count(inode_blocks);
        self.write_back_inode(inode_ref);

        // Update block group free blocks count
        let mut fb_cnt = block_group.get_free_blocks_count();
        fb_cnt -= 1;
        block_group.set_free_blocks_count(fb_cnt as u32);
        block_group.sync_to_disk_with_csum(&self.block_device, bgid, &super_block);

        Ok(())
    }

    #[allow(unused)]
    pub fn balloc_free_blocks(&self, inode_ref: &mut Ext4InodeRef, start: Ext4Fsblk, count: u32) {
        // log::trace!("balloc_free_blocks start {:x?} count {:x?}", start, count);
        let mut remaining = count as usize;
        let mut block = start;

        if remaining == 0 {
            return;
        }

        let mut super_block = self.balloc_super_block();
        let blocks_per_group = super_block.blocks_per_group() as u64;

        while remaining > 0 {
            let bgid = self.get_bgid_of_block(block);
            let idx_in_bg = self.addr_to_idx_bg(block) as usize;
            let blocks_left_in_group = blocks_per_group as usize - idx_in_bg;
            let free_cnt = remaining.min(blocks_left_in_group);
            let end_idx = idx_in_bg + free_cnt - 1;

            let mut bg =
                Ext4BlockGroup::load_new(&self.block_device, &super_block, bgid as usize);

            let block_bitmap_block = bg.get_block_bitmap_block(&super_block);
            let mut raw_data = self
                .block_device
                .read_offset(block_bitmap_block as usize * BLOCK_SIZE);
            let mut data: &mut Vec<u8> = &mut raw_data;

            // ext4_bmap_bits_free takes an inclusive end bit; count spans may cross groups.
            ext4_bmap_bits_free(data, idx_in_bg as u32, end_idx as u32);

            remaining -= free_cnt;
            block += free_cnt as u64;

            bg.set_block_group_balloc_bitmap_csum(&super_block, data);
            self.block_device
                .write_offset(block_bitmap_block as usize * BLOCK_SIZE, data);

            /* Update superblock free blocks count */
            let mut super_blk_free_blocks = super_block.free_blocks_count();

            super_blk_free_blocks += free_cnt as u64;
            super_block.set_free_blocks_count(super_blk_free_blocks);
            super_block.sync_to_disk_with_csum(&self.block_device);

            /* Update inode blocks (different block size!) count */
            let mut inode_blocks = inode_ref.inode.blocks_count();

            inode_blocks -= (free_cnt * (BLOCK_SIZE / EXT4_INODE_BLOCK_SIZE)) as u64;
            inode_ref.inode.set_blocks_count(inode_blocks);
            self.write_back_inode(inode_ref);

            /* Update block group free blocks count */
            let mut fb_cnt = bg.get_free_blocks_count();
            fb_cnt += free_cnt as u64;
            bg.set_free_blocks_count(fb_cnt as u32);
            bg.sync_to_disk_with_csum(&self.block_device, bgid as usize, &super_block);
        }
    }


    pub fn is_system_reserved_block(&self, block_num: u64, _bgid: u32) -> bool {

        // 如果缓存未初始化，则不判断
        if self.system_zone_cache.is_none() {
            return false;
        }
        // 查缓存
        if let Some(zones) = &self.system_zone_cache {
            for zone in zones {
                if block_num >= zone.start_blk && block_num <= zone.end_blk {
                    return true;
                }
            }
        }
        false
    }
    /// Optimized block allocation inspired by lwext4
    /// 
    /// Params:
    /// `inode_ref` - Reference to the inode
    /// `start_bgid` - Starting block group ID, will be updated to the last used block group
    /// `count` - Number of blocks to allocate
    /// 
    /// Returns:
    /// `Result<Vec<Ext4Fsblk>>` - Vector of allocated physical block numbers
    pub fn balloc_alloc_block_batch(
        &self,
        inode_ref: &mut Ext4InodeRef,
        start_bgid: &mut u32,
        count: usize,
    ) -> Result<Vec<Ext4Fsblk>> {
        if count == 0 {
            return Ok(Vec::new());
        }
        
        log::debug!("[Block Alloc] Requesting {} blocks starting from bgid {}", count, *start_bgid);
        
        let super_block = &self.super_block;
        let block_group_count = super_block.block_group_count();
        
        // Validate inputs
        if block_group_count == 0 {
            log::error!("[Block Alloc] Invalid block group count: 0");
            return return_errno_with_message!(Errno::EINVAL, "Invalid block group count");
        }
        
        if *start_bgid >= block_group_count {
            log::warn!("[Block Alloc] Invalid start_bgid {}, resetting to 0", *start_bgid);
            *start_bgid = 0;
        }
        
        let mut bgid = *start_bgid;
        let mut result = Vec::with_capacity(count);
        let mut remaining = count;
        
        // Search through all block groups
        let mut groups_checked = 0;
        
        while remaining > 0 && groups_checked < block_group_count {
            // Load block group reference
            let mut block_group = 
                Ext4BlockGroup::load_new(&self.block_device, super_block, bgid as usize);
            
            // Check if this group has free blocks
            let free_blocks = block_group.get_free_blocks_count();
            if free_blocks == 0 {
                log::debug!("[Block Alloc] Block group {} has no free blocks", bgid);
                bgid = (bgid + 1) % block_group_count;
                groups_checked += 1;
                continue;
            }
            
            // Get block bitmap for this group
            let bmp_blk_adr = block_group.get_block_bitmap_block(super_block);
            let mut bitmap_data = self.load_balloc_bitmap_for_alloc(
                super_block,
                bgid,
                &mut block_group,
            );
            
            // Compute indexes and limits
            let first_in_bg = self.get_block_of_bgid(bgid);
            let first_in_bg_index = self.addr_to_idx_bg(first_in_bg);
            let idx_in_bg = first_in_bg_index; // Start from the beginning of the group
            let blocks_per_group = super_block.blocks_per_group();
            
            // Find free blocks in bitmap
            let mut found_blocks = 0;
            let max_to_find = core::cmp::min(remaining, free_blocks as usize);
            let mut rel_blk_idx = 0;
            let mut current_idx = idx_in_bg;
            
            // First try to find blocks in a simple loop starting from current_idx
            while found_blocks < max_to_find && current_idx < blocks_per_group {
                // Ensure we don't go beyond bitmap size (BLOCK_SIZE * 8 bits)
                if current_idx >= BLOCK_SIZE as u32 * 8 {
                    break;
                }
                
                if ext4_bmap_is_bit_clr(&bitmap_data, current_idx) {
                    // Check if this is a system reserved block
                    let block_num = self.bg_idx_to_addr(current_idx, bgid);
                    if self.is_system_reserved_block(block_num, bgid) {
                        log::error!("[Block Alloc] System reserved block found at {:x?}", block_num);
                        current_idx += 1;
                        continue;
                    }
                    
                    // Found a free block
                    ext4_bmap_bit_set(&mut bitmap_data, current_idx);
                    
                    // Calculate physical block address
                    let block_num = self.bg_idx_to_addr(current_idx, bgid);
                    
                    // Add to result
                    result.push(block_num);
                    found_blocks += 1;
                    
                    // For debugging continuity issues
                    if result.len() > 1 {
                        let prev_block = result[result.len() - 2];
                        if block_num != prev_block + 1 {
                            log::debug!("[Block Alloc] Non-contiguous blocks: prev={}, current={}, diff={}",
                                prev_block, block_num, block_num - prev_block);
                        }
                    }
                }
                
                current_idx += 1;
            }
            
            // If we didn't find enough blocks using sequential search, use bitmap search function
            if found_blocks < max_to_find {
                let mut start_idx = current_idx;
                
                while found_blocks < max_to_find {
                    // Make sure we don't exceed the bitmap size
                    let end_idx = core::cmp::min(blocks_per_group, BLOCK_SIZE as u32 * 8);
                    
                    // Find next clear bit
                    if !ext4_bmap_bit_find_clr(&bitmap_data, start_idx, end_idx, &mut rel_blk_idx) {
                        break; // No more free blocks in this group
                    }
                    
                    // Check if this is a system reserved block
                    let block_num = self.bg_idx_to_addr(rel_blk_idx, bgid);
                    if self.is_system_reserved_block(block_num, bgid) {
                        // Skip this block and continue search
                        log::error!("[Block Alloc] System reserved block found at {:x?} bgid {}", block_num, bgid);
                        start_idx = rel_blk_idx + 1;
                        continue;
                    }
                    
                    ext4_bmap_bit_set(&mut bitmap_data, rel_blk_idx);
                    
                    // Calculate physical block address
                    let block_num = self.bg_idx_to_addr(rel_blk_idx, bgid);
                    
                    // Add to result
                    result.push(block_num);
                    found_blocks += 1;
                    
                    // For debugging continuity issues
                    if result.len() > 1 {
                        let prev_block = result[result.len() - 2];
                        if block_num != prev_block + 1 {
                            log::debug!("[Block Alloc] Non-contiguous blocks: prev={}, current={}, diff={}",
                                prev_block, block_num, block_num - prev_block);
                        }
                    }
                }
            }
            
            // If we found any blocks, update metadata
            if found_blocks > 0 {
                // Update bitmap on disk
                block_group.set_block_group_balloc_bitmap_csum(super_block, &bitmap_data);
                self.block_device.write_offset(bmp_blk_adr as usize * BLOCK_SIZE, &bitmap_data);
                
                // Update block group free blocks count
                let new_free_count = free_blocks - found_blocks as u64;
                block_group.set_free_blocks_count(new_free_count as u32);
                block_group.sync_to_disk_with_csum(&self.block_device, bgid as usize, super_block);
                
                // Update superblock free blocks count
                let mut sb_copy = self.balloc_super_block();
                let sb_free_blocks = sb_copy.free_blocks_count();
                sb_copy.set_free_blocks_count(sb_free_blocks - found_blocks as u64);
                sb_copy.sync_to_disk_with_csum(&self.block_device);
                
                // Update inode blocks count
                let blocks_per_fs_block = BLOCK_SIZE as u64 / EXT4_INODE_BLOCK_SIZE as u64;
                let mut inode_blocks = inode_ref.inode.blocks_count();
                inode_blocks += found_blocks as u64 * blocks_per_fs_block;
                inode_ref.inode.set_blocks_count(inode_blocks);
                
                // Decrement remaining blocks to allocate
                remaining -= found_blocks;
                
                log::debug!("[Block Alloc] Allocated {} blocks from bg {}", found_blocks, bgid);
            }
            
            // Try next block group
            bgid = (bgid + 1) % block_group_count;
            groups_checked += 1;
        }
        
        // Log allocation results
        let allocated_count = result.len();
        log::debug!("[Block Alloc] Allocated {}/{} blocks", allocated_count, count);
        
        // Even if we couldn't allocate all requested blocks, return what we got
        if remaining > 0 {
            log::warn!("[Block Alloc] Could only allocate {} out of {} blocks. Remaining: {}", 
                allocated_count, count, remaining);
        }
        
        // Update start_bgid to continue from where we left off next time
        *start_bgid = bgid;
        
        // Write back inode to save block count changes
        if allocated_count > 0 {
            self.write_back_inode(inode_ref);
        }
        
        Ok(result)
    }

    /// Returns the number of meta blocks for a given block group, like Linux ext4_num_base_meta_blocks.
    pub fn num_base_meta_blocks(&self, bgid: u32) -> u32 {
        let has_super = self.ext4_bg_has_super(bgid);
        let gdt_blocks = self.ext4_bg_num_gdb(bgid);
        let meta_blocks = if has_super { 1 + gdt_blocks } else { 0 };
        // log::info!(
        //     "[num_base_meta_blocks] group={} has_super={} gdt_blocks={} meta_blocks={}",
        //     bgid, has_super, gdt_blocks, meta_blocks
        // );
        meta_blocks
    }

    /// 判断group是否有superblock备份（与Linux ext4_bg_has_super一致）
    pub fn ext4_bg_has_super(&self, group: u32) -> bool {
        if group == 0 {
            return true;
        }
        // Linux: group号为3/5/7的幂也有superblock备份
        fn is_power_of(mut n: u32, base: u32) -> bool {
            if n < base { return false; }
            while n % base == 0 { n /= base; }
            n == 1
        }
        is_power_of(group, 3) || is_power_of(group, 5) || is_power_of(group, 7)
    }

    /// 判断是否有meta_bg特性（与Linux ext4_has_feature_meta_bg一致）
    pub fn ext4_has_feature_meta_bg(&self) -> bool {
        // EXT4_FEATURE_INCOMPAT_META_BG = 0x0010
        const EXT4_FEATURE_INCOMPAT_META_BG: u32 = 0x0010;
        (self.super_block.incompat_features() & EXT4_FEATURE_INCOMPAT_META_BG) != 0
    }

    /// 返回该group的GDT blocks数（与Linux ext4_bg_num_gdb一致）
    pub fn ext4_bg_num_gdb(&self, group: u32) -> u32 {
        let sb = &self.super_block;
        let group_count = sb.block_group_count();
        let block_size = sb.block_size();
        let desc_size = sb.desc_size() as u32;
        let reserved_gdt_blocks = sb.reserved_gdt_blocks() as u32;
        let desc_blocks = ((group_count as u64 * desc_size as u64 + block_size as u64 - 1) / block_size as u64) as u32;

        if !self.ext4_bg_has_super(group) {
            return 0;
        }
        if group == 0 {
            return desc_blocks + reserved_gdt_blocks;
        }
        if self.ext4_has_feature_meta_bg() {
            1
        } else {
            desc_blocks + reserved_gdt_blocks
        }
    }
}
