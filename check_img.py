import struct

with open('/workspace/sdcard-rv.img', 'rb') as f:
    data = f.read(4096)
    magic_at_438 = data[0x438:0x43A]
    magic_at_38 = data[0x38:0x3A]
    print(f"Magic at 0x438: {magic_at_438.hex()} = {struct.unpack('<H', magic_at_438)[0]:#06x}")
    print(f"Magic at 0x38: {magic_at_38.hex()} = {struct.unpack('<H', magic_at_38)[0]:#06x}")
    log_bs = struct.unpack('<I', data[0x418:0x41C])[0]
    print(f"log_block_size at 0x418: {log_bs}, block_size={1024 << log_bs}")
    first_db = struct.unpack('<I', data[0x400:0x404])[0]
    print(f"s_first_data_block: {first_db}")
    s_blocks_lo = struct.unpack('<I', data[0x404:0x408])[0]
    print(f"s_blocks_lo: {s_blocks_lo}")
