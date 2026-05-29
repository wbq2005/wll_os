#!/usr/bin/env python3
"""Debug ext4 image reading step by step."""

import struct
import sys

def main():
    img_path = sys.argv[1]

    with open(img_path, 'rb') as f:
        # Read superblock
        f.seek(1024)  # Superblock at offset 1024
        sb = f.read(1024)

        magic = struct.unpack('<H', sb[0x38:0x3A])[0]
        print(f"Superblock magic: 0x{magic:04x} (expect 0x53ef)")
        if magic != 0x53ef:
            print("ERROR: Not a valid ext4 filesystem!")
            return

        block_size_log = struct.unpack('<I', sb[0x18:0x1C])[0]
        block_size = 1024 << block_size_log
        print(f"Block size: {block_size} bytes (log={block_size_log})")

        s_blocks_lo = struct.unpack('<I', sb[0x04:0x08])[0]
        print(f"s_blocks_lo: {s_blocks_lo}")

        # Read root inode (inode 2)
        # Inode table starts at block s_first_data_block + s_inodes_per_group * inode_size
        # For simplicity, assume inodes are at a fixed location
        # Let's find the inode table from the block group descriptor

        # Block group 0 descriptor is right after superblock (block 1 for 1KB blocks)
        # For 4KB blocks, it's at block 2
        if block_size == 4096:
            bg0_offset = block_size * 2  # Block 2 for 4KB block size
        else:
            bg0_offset = block_size  # Block 1 for 1KB block size

        f.seek(bg0_offset)
        bg0 = f.read(64)  # block group descriptor is at least 64 bytes

        # block_group_table at offset 0, inode_table_lo at offset 8
        inode_table_lo = struct.unpack('<I', bg0[8:12])[0]
        inode_table_hi = struct.unpack('<H', bg0[28:30])[0] if len(bg0) >= 30 else 0
        inode_table = (inode_table_hi << 32) | inode_table_lo
        inodes_per_group = struct.unpack('<I', sb[0x028:0x02C])[0]

        print(f"Inode table block: {inode_table}")
        print(f"Inodes per group: {inodes_per_group}")

        # Inode 2 is in block group 0, first inode
        inode_size = 256  # Standard for ext4
        inode_offset = inode_table * block_size + (2 - 1) * inode_size
        print(f"Root inode (2) offset: {inode_offset} (block {inode_table})")

        f.seek(inode_offset)
        inode = f.read(inode_size)

        # i_mode
        mode = struct.unpack('<H', inode[0:2])[0]
        print(f"Inode mode: 0x{mode:04x}")

        # i_size
        size = struct.unpack('<I', inode[0x04:0x08])[0]
        print(f"Inode size: {size}")

        # i_blocks (512-byte sectors)
        blocks_512 = struct.unpack('<I', inode[0x28:0x2C])[0]
        print(f"Inode blocks (512B sectors): {blocks_512}")

        # Check for extent header at offset 0x28 (i_block array for extents)
        extent_header_offset = 0x28
        eh = inode[extent_header_offset:extent_header_offset+12]
        eh_magic = struct.unpack('<H', eh[0:2])[0]
        eh_entries = struct.unpack('<H', eh[2:4])[0]
        eh_max = struct.unpack('<H', eh[4:6])[0]
        eh_depth = struct.unpack('<H', eh[6:8])[0]

        print(f"\nExtent header in inode:")
        print(f"  Magic: 0x{eh_magic:04x} (expect 0xF30A)")
        print(f"  Entries: {eh_entries}")
        print(f"  Max: {eh_max}")
        print(f"  Depth: {eh_depth}")

        if eh_magic == 0xF30A:
            if eh_depth == 0:
                # Leaf node - parse extent directly
                # First extent at offset 0x34 (after header)
                ext_off = extent_header_offset + 12
                ext = inode[ext_off:ext_off+12]
                first_block = struct.unpack('<I', ext[0:4])[0]
                block_count = struct.unpack('<H', ext[4:6])[0]
                start_hi = struct.unpack('<H', ext[6:8])[0]
                start_lo = struct.unpack('<I', ext[8:12])[0]
                start = (start_hi << 32) | start_lo

                print(f"\nExtent (leaf, depth=0):")
                print(f"  first_block: {first_block}")
                print(f"  block_count: {block_count}")
                print(f"  start: {start} (0x{start:08x})")
                print(f"  start_hi: {start_hi}, start_lo: {start_lo}")

                # Read the directory block
                dir_block_offset = start * block_size
                print(f"\nReading directory block at offset {dir_block_offset}")

                f.seek(dir_block_offset)
                dir_block = f.read(block_size)

                # Parse directory entries
                offset = 0
                entries = []
                while offset + 8 <= block_size:  # minimum dir entry size is 8
                    inode_num = struct.unpack('<I', dir_block[offset:offset+4])[0]
                    rec_len = struct.unpack('<H', dir_block[offset+4:offset+6])[0]
                    name_len = struct.unpack('<B', dir_block[offset+6:offset+7])[0]
                    file_type = struct.unpack('<B', dir_block[offset+7:offset+8])[0]

                    if inode_num == 0:
                        offset += rec_len
                        continue

                    name = dir_block[offset+8:offset+8+name_len].decode('utf-8', errors='replace')
                    entries.append((inode_num, name, rec_len, name_len, file_type))

                    offset += rec_len

                print(f"\nDirectory entries found: {len(entries)}")
                for ino, name, rec_len, name_len, ftype in entries:
                    print(f"  inode={ino}, name='{name}', rec_len={rec_len}")

if __name__ == '__main__':
    main()
