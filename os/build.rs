//!
//! 链接脚本路径：ld 在 target/deps 下运行时相对路径不可靠，使用 crate 根目录绝对路径。

use std::env;
use std::fs;
use std::path::PathBuf;
use std::collections::BTreeSet;

use ext4_view::Ext4;

const MAX_PRELOAD_FILES: usize = 8;
const MAX_PRELOAD_FILE_SIZE: usize = 2 * 1024 * 1024;

fn main() {
    let manifest_dir = env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let target = env::var("TARGET").expect("TARGET");
    let manifest_dir = PathBuf::from(manifest_dir);

    let linker_name = if target.contains("riscv64") {
        "linker_riscv64.lds"
    } else if target.contains("loongarch") {
        "linker_loongarch64.lds"
    } else {
        panic!("unsupported TARGET for linker script: {target}");
    };

    let linker = manifest_dir.join("src").join(linker_name);
    if !linker.is_file() {
        panic!("linker script missing: {}", linker.display());
    }

    println!("cargo:rerun-if-changed={}", linker.display());
    println!("cargo:rustc-link-arg=-T{}", linker.display());
    emit_preloaded_apps(&manifest_dir, &target);
}

fn emit_preloaded_apps(manifest_dir: &PathBuf, target: &str) {
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let generated = out_dir.join("preloaded_apps.rs");

    let image_name = if target.contains("riscv64") {
        "sdcard-rv.img"
    } else if target.contains("loongarch") {
        "sdcard-la.img"
    } else {
        let _ = fs::write(&generated, "fn preload_generated_programs() {}\n");
        return;
    };

    let image_path = manifest_dir.parent().unwrap_or(manifest_dir).join(image_name);
    println!("cargo:rerun-if-changed={}", image_path.display());

    let mut code = String::from("fn preload_generated_programs() {\n");

    if image_path.is_file() {
        match fs::read(&image_path)
            .map(|data| Box::new(data) as Box<dyn ext4_view::Ext4Read>)
            .map_err(|err| err.to_string())
            .and_then(|reader| Ext4::load(reader).map_err(|err| err.to_string()))
        {
            Ok(fs) => {
                let mut selected: Vec<(String, String)> = Vec::new();
                for (path, alias) in candidate_paths(target) {
                    let actual_path = if fs.exists(*path).unwrap_or(false) {
                        Some((*path).to_string())
                    } else {
                        find_by_basename(&fs, "/", basename(path), 6)
                    };

                    let Some(actual_path) = actual_path else {
                        println!(
                            "cargo:warning=skip preload {} from {}: file not found",
                            path,
                            image_path.display()
                        );
                        continue;
                    };
                    selected.push((actual_path, (*alias).to_string()));
                }

                let mut script_paths = Vec::new();
                collect_test_scripts(&fs, "/", 6, &mut script_paths);
                for script in script_paths {
                    selected.push((script.clone(), basename(&script).to_string()));
                    if let Ok(bytes) = fs.read(&script) {
                        if let Ok(text) = core::str::from_utf8(&bytes) {
                            for name in extract_exec_names(text) {
                                if let Some(actual) = find_by_basename(&fs, "/", &name, 6) {
                                    selected.push((actual, name));
                                }
                            }
                        }
                    }
                }

                let mut seen_actual = BTreeSet::new();
                let mut seen_alias = BTreeSet::new();
                let mut preload_count = 0usize;
                for (actual_path, alias) in selected {
                    if preload_count >= MAX_PRELOAD_FILES {
                        println!(
                            "cargo:warning=preload file count reached limit {}, skip remaining",
                            MAX_PRELOAD_FILES
                        );
                        break;
                    }
                    if !seen_actual.insert(actual_path.clone()) {
                        continue;
                    }
                    match fs.read(actual_path.as_str()) {
                        Ok(data) => {
                            if data.len() > MAX_PRELOAD_FILE_SIZE {
                                println!(
                                    "cargo:warning=skip preload {}: file too large ({} bytes)",
                                    actual_path,
                                    data.len()
                                );
                                continue;
                            }
                            preload_count += 1;
                            code.push_str(&format!(
                                "    crate::fs::add_user_program({:?}, &{data:?});\n",
                                actual_path
                            ));
                            if seen_alias.insert(alias.clone()) && actual_path != alias {
                                code.push_str(&format!(
                                    "    crate::fs::add_user_program({:?}, &{data:?});\n",
                                    alias
                                ));
                            }
                        }
                        Err(err) => {
                            println!(
                                "cargo:warning=skip preload {} from {}: {}",
                                actual_path,
                                image_path.display(),
                                err
                            );
                        }
                    }
                }
            }
            Err(err) => {
                println!(
                    "cargo:warning=failed to parse {} for preload: {}",
                    image_path.display(),
                    err
                );
            }
        }
    } else {
        println!(
            "cargo:warning={} not found, build will use empty MemFS preload",
            image_path.display()
        );
    }

    code.push_str("}\n");
    fs::write(&generated, code).expect("write generated preload source");
}

fn candidate_paths(target: &str) -> &'static [(&'static str, &'static str)] {
    let _ = target;
    &[
        ("/init", "init"),
    ]
}

fn basename(path: &str) -> &str {
    path.rsplit('/').find(|part| !part.is_empty()).unwrap_or(path)
}

fn find_by_basename(fs: &Ext4, dir: &str, target: &str, depth: usize) -> Option<String> {
    if depth == 0 {
        return None;
    }

    let entries = fs.read_dir(dir).ok()?;
    for entry in entries {
        let entry = entry.ok()?;
        let path = entry.path();
        let path = path.to_str().ok()?.to_string();
        let metadata = entry.metadata().ok()?;
        if metadata.is_dir() {
            if let Some(found) = find_by_basename(fs, &path, target, depth - 1) {
                return Some(found);
            }
        } else if basename(&path) == target {
            return Some(path);
        }
    }
    None
}

fn collect_test_scripts(fs: &Ext4, dir: &str, depth: usize, out: &mut Vec<String>) {
    if depth == 0 {
        return;
    }
    if let Ok(entries) = fs.read_dir(dir) {
        for entry in entries.flatten() {
            if let Ok(path) = entry.path().to_str().map(|s| s.to_string()) {
                if let Ok(meta) = entry.metadata() {
                    if meta.is_dir() {
                        collect_test_scripts(fs, &path, depth - 1, out);
                    } else {
                        let base = basename(&path);
                        if base.ends_with("_testcode.sh") || base == "run-all.sh" {
                            out.push(path);
                        }
                    }
                }
            }
        }
    }
}

fn extract_exec_names(script: &str) -> Vec<String> {
    let mut names = BTreeSet::new();
    let mut in_tests = false;

    for line in script.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if t.starts_with("tests=\"") {
            in_tests = true;
            let rest = t.trim_start_matches("tests=\"").trim();
            if !rest.is_empty() && rest != "\"" {
                names.insert(rest.to_string());
            }
            continue;
        }
        if in_tests {
            if t == "\"" {
                in_tests = false;
                continue;
            }
            names.insert(t.trim_matches('"').to_string());
            continue;
        }

        if let Some(cmd) = t.split_whitespace().next() {
            if let Some(name) = cmd.strip_prefix("./") {
                if !name.is_empty() {
                    names.insert(name.to_string());
                }
            }
        }
    }

    names.into_iter().collect()
}
