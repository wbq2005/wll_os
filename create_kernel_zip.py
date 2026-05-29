import zipfile
import os

kernel_dir = 'autotest_repo/kernel'
output_zip = 'kernel.zip'

# Remove existing zip
if os.path.exists(output_zip):
    os.remove(output_zip)

with zipfile.ZipFile(output_zip, 'w', zipfile.ZIP_DEFLATED) as zf:
    for root, dirs, files in os.walk(kernel_dir):
        for file in files:
            abs_path = os.path.join(root, file)
            # Use relative path from kernel_dir (not starting with 'kernel/')
            # This makes the zip structure match: kernel/... at zip root
            rel_path = os.path.relpath(abs_path, kernel_dir)
            zf.write(abs_path, rel_path)

print(f'Created {output_zip}')
# Verify structure
with zipfile.ZipFile(output_zip, 'r') as zf:
    names = zf.namelist()[:10]
    print(f'First entries: {names}')

