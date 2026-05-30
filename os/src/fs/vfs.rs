//! 最薄 VFS：统一 MemFS + ext4 后端的查找与变更入口；`mount`/`umount2` 语义见 `syscall::fs`。

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;
use lazy_static::lazy_static;
use spin::Mutex;

use crate::utils::error::SysErrNo;

use super::ext4_vol;
use super::fd;
use super::{normalize_path, MEM_FS};

lazy_static! {
    static ref WHITEOUTS: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
}

pub fn is_removed(name: &str) -> bool {
    WHITEOUTS.lock().contains(&normalize_path(name))
}

fn clear_whiteout(name: &str) {
    WHITEOUTS.lock().remove(&normalize_path(name));
}

pub fn read_file(name: &str) -> Option<Vec<u8>> {
    let norm = normalize_path(name);
    if is_removed(&norm) {
        return None;
    }
    let m = MEM_FS.lock();
    if let Some(f) = m.get_file(&norm) {
        return Some(f.content.clone());
    }
    drop(m);
    ext4_vol::slurp_regular_file(&norm)
}

fn basename(path: &str) -> &str {
    path.rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or(path)
}

fn read_interpreter_logical(root: &str, logical: &str) -> Option<(String, String, Vec<u8>)> {
    let logical = normalize_path(logical);
    let host = super::apply_root(root, &logical);
    read_file(&host).map(|data| (logical, host, data))
}

fn read_interpreter_host(logical: &str, host: &str) -> Option<(String, String, Vec<u8>)> {
    let logical = normalize_path(logical);
    let host = normalize_path(host);
    read_file(&host).map(|data| (logical, host, data))
}

pub fn read_interpreter(root: &str, interp: &str) -> Option<(String, String, Vec<u8>)> {
    let interp = normalize_path(interp);
    if let Some(found) = read_interpreter_logical(root, &interp) {
        return Some(found);
    }

    let name = basename(&interp);
    let lib_candidate = alloc::format!("/lib/{}", name);
    if interp.starts_with("/lib64/") {
        if let Some(found) = read_interpreter_logical(root, &lib_candidate) {
            return Some(found);
        }
    }

    #[cfg(target_arch = "loongarch64")]
    {
        if name == "ld-linux-loongarch-lp64d.so.1" {
            let host = alloc::format!("/glibc/lib/{}", name);
            if let Some(found) = read_interpreter_host(&lib_candidate, &host) {
                return Some(found);
            }
        }
    }

    read_interpreter_logical(root, "/lib/libc.so")
}

pub fn file_exists(name: &str) -> bool {
    let norm = normalize_path(name);
    if is_removed(&norm) {
        return false;
    }
    MEM_FS.lock().exists(&norm) || ext4_vol::ext4_regular_file_exists(&norm)
}

pub fn dir_exists(name: &str) -> bool {
    let norm = normalize_path(name);
    if is_removed(&norm) {
        return false;
    }
    MEM_FS.lock().is_dir(&norm) || ext4_vol::ext4_dir_path_exists(&norm)
}

pub fn remove_file(path: &str) -> Result<(), SysErrNo> {
    let norm = normalize_path(path);
    let mut m = MEM_FS.lock();
    if m.exists(&norm) && !m.is_dir(&norm) {
        return m.remove_file(&norm);
    }
    drop(m);
    if ext4_vol::ext4_regular_file_exists(&norm) {
        WHITEOUTS.lock().insert(norm);
        return Ok(());
    }
    Err(SysErrNo::ENOENT)
}

pub fn create_dir(path: &str) -> Result<(), SysErrNo> {
    let norm = normalize_path(path);
    if file_exists(&norm) || dir_exists(&norm) {
        return Err(SysErrNo::EEXIST);
    }
    clear_whiteout(&norm);
    MEM_FS.lock().add_dir(&norm);
    Ok(())
}

pub fn list_dir(path: &str) -> Result<Vec<fd::DirEntryRecord>, SysErrNo> {
    let norm = normalize_path(path);

    let mem_dir = MEM_FS.lock().is_dir(&norm);
    let mem_entries: Vec<fd::DirEntryRecord> = if mem_dir {
        MEM_FS.lock().list_dir(&norm)?
    } else {
        Vec::new()
    };

    let ext_dir = ext4_vol::is_ext4_mounted() && ext4_vol::ext4_dir_path_exists(&norm);

    if !mem_dir && !ext_dir {
        let m = MEM_FS.lock();
        if m.get_file(&norm).is_some() {
            return Err(SysErrNo::ENOTDIR);
        }
        drop(m);
        if ext4_vol::is_ext4_mounted() && ext4_vol::ext4_regular_file_exists(&norm) {
            return Err(SysErrNo::ENOTDIR);
        }
        return Err(SysErrNo::ENOENT);
    }

    let mut map: BTreeMap<String, fd::DirEntryRecord> = BTreeMap::new();
    for e in mem_entries {
        map.insert(e.name.clone(), e);
    }
    if ext_dir {
        for (name, is_dir) in ext4_vol::ext4_list_dir(&norm)? {
            let child = if norm == "/" {
                alloc::format!("/{}", name)
            } else {
                alloc::format!("{}/{}", norm, name)
            };
            if is_removed(&child) {
                continue;
            }
            map.entry(name.clone())
                .or_insert(fd::DirEntryRecord { name, is_dir });
        }
    }

    Ok(map.into_values().collect())
}

pub fn list_files() -> Vec<String> {
    let mem_paths = MEM_FS.lock().list_file_names();
    ext4_vol::merged_list_all_file_paths(mem_paths)
        .into_iter()
        .filter(|path| !is_removed(path))
        .collect()
}
