#!/usr/bin/env python3
import struct

with open('testdata/sdcard-rv.img', 'rb') as f:
    BLOCK_SIZE = 4096

    # Root directory is typically in block 2 (after superblock + bg descriptors)
    f.seek(BLOCK_SIZE * 2)
    data = f.read(BLOCK_SIZE)

    print("Root directory entries:")
    offset = 0
    while offset < len(data):
        if offset + 8 > len(data):
            break
        inode = struct.unpack('<I', data[offset:offset+4])[0]
        rec_len = struct.unpack('<H', data[offset+4:offset+6])[0]
        name_len = struct.unpack('<B', data[offset+6:offset+7])[0]
        file_type = data[offset+7]
        name = data[offset+8:offset+8+name_len].decode('utf-8', errors='replace')

        type_names = {1: 'reg', 2: 'dir', 3: 'chr', 4: 'blk', 5: 'fifo', 6: 'sock', 7: 'lnk'}

        if inode == 0 and rec_len == 0:
            break

        print(f"  inode={inode}, rec_len={rec_len}, name_len={name_len}, type={type_names.get(file_type, '?')}, name='{name}'")
        offset += rec_len

        if inode == 0:
            break

    # Also try scanning for shell scripts by looking for #!
    print("\nScanning for shell scripts (looking for #! pattern):")
    f.seek(0)
    pos = 0
    found_scripts = []
    while True:
        chunk = f.read(BLOCK_SIZE * 16)
        if not chunk:
            break
        # Look for #! pattern in the data
        idx = chunk.find(b'#!/')
        if idx >= 0:
            line_end = chunk.find(b'\n', idx)
            if line_end < 0:
                line_end = len(chunk)
            line = chunk[idx:line_end].decode('utf-8', errors='replace')
            found_scripts.append(f"  offset {pos + idx}: {line[:60]}")
        pos += len(chunk)

    for s in found_scripts[:20]:
        print(s)
    print(f"Total shell scripts found: {len(found_scripts)}")
