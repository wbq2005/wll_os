//!
//! 链接脚本路径：ld 在 target/deps 下运行时相对路径不可靠，使用 crate 根目录绝对路径。

use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use ext4_view::Ext4;

const MAX_PRELOAD_FILES: usize = 512;
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

    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_DEV_PRELOAD");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_LIBCTEST");
    println!("cargo:rerun-if-env-changed=LIBCTEST_FILTER");
    println!("cargo:rerun-if-env-changed=WLL_HARNESS_GROUPS");
    println!("cargo:rerun-if-env-changed=WLL_TRACE_TEST_COMMANDS");
    println!("cargo:rerun-if-env-changed=WLL_TRACE_TEST_GROUPS");
    let dev_preload = env::var_os("CARGO_FEATURE_DEV_PRELOAD").is_some();

    let mut code = String::from("fn preload_generated_programs() {\n");

    if dev_preload {
        emit_dev_preload(&mut code, manifest_dir, target);
    }
    emit_libctest_runtime_libs(&mut code, target);
    emit_libctest_extra(&mut code, manifest_dir, target, &out_dir);

    code.push_str("}\n");
    fs::write(&generated, code).expect("write generated preload source");
}

fn emit_dev_preload(code: &mut String, manifest_dir: &PathBuf, target: &str) {
    let image_name = if target.contains("riscv64") {
        "sdcard-rv.img"
    } else if target.contains("loongarch") {
        "sdcard-la.img"
    } else {
        return;
    };

    let image_path = manifest_dir
        .parent()
        .unwrap_or(manifest_dir)
        .join(image_name);
    println!("cargo:rerun-if-changed={}", image_path.display());

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
                script_paths.sort_by(|a, b| {
                    script_preload_rank(a)
                        .cmp(&script_preload_rank(b))
                        .then_with(|| normalize_image_path(a).cmp(&normalize_image_path(b)))
                });
                let mut basic_files = Vec::new();
                if target.contains("loongarch") {
                    collect_basic_files(&fs, "/", 6, &mut basic_files);
                    basic_files
                        .sort_by(|a, b| normalize_image_path(a).cmp(&normalize_image_path(b)));
                }

                for script in script_paths
                    .iter()
                    .filter(|path| script_preload_rank(path) <= 1)
                {
                    push_script_and_execs(&fs, &mut selected, script);
                }
                for file in basic_files {
                    selected.push((file.clone(), basename(&file).to_string()));
                }
                for script in script_paths
                    .iter()
                    .filter(|path| script_preload_rank(path) > 1)
                {
                    push_script_and_execs(&fs, &mut selected, script);
                }

                let mut seen_actual = BTreeSet::new();
                let mut seen_install = BTreeSet::new();
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
                    let install_path = normalize_image_path(&actual_path);
                    if !seen_install.insert(install_path.clone()) {
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
                                install_path
                            ));
                            // Only add basename alias for non-script files (like /init)
                            // Skip for _testcode.sh and run-all.sh to avoid path shadowing
                            let base = basename(&install_path);
                            let is_script = base.ends_with("_testcode.sh") || base == "run-all.sh";
                            if !is_script
                                && seen_alias.insert(alias.clone())
                                && install_path != alias
                            {
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
}

struct LibcTestExtraSpec {
    libc: &'static str,
    install_path: &'static str,
    entry_name: &'static str,
    static_link: bool,
    musl_pleval: bool,
}

fn emit_libctest_runtime_libs(code: &mut String, target: &str) {
    let specs = libctest_runtime_lib_specs(target);
    for (install_path, candidates) in specs {
        let Some(path) = candidates
            .iter()
            .map(PathBuf::from)
            .find(|path| path.is_file())
        else {
            println!(
                "cargo:warning=skip libc-test runtime lib {}: no candidate found",
                install_path
            );
            continue;
        };
        println!("cargo:rerun-if-changed={}", path.display());
        code.push_str(&format!(
            "    crate::fs::add_user_program({:?}, include_bytes!({:?}));\n",
            install_path,
            path.to_string_lossy()
        ));
    }
}

fn libctest_runtime_lib_specs(target: &str) -> Vec<(&'static str, Vec<&'static str>)> {
    if target.contains("loongarch") {
        vec![(
            "/glibc/lib/libgcc_s.so.1",
            vec![
                "/opt/gcc-13.2.0-loongarch64-linux-gnu/loongarch64-linux-gnu/lib64/libgcc_s.so.1",
                "/opt/toolchain-loongarch64-linux-gnu-gcc8-host-x86_64-2022-07-18/sysroot/usr/lib64/libgcc_s.so.1",
            ],
        )]
    } else if target.contains("riscv64") {
        vec![(
            "/glibc/lib/libgcc_s.so.1",
            vec![
                "/usr/riscv64-linux-gnu/lib/libgcc_s.so.1",
                "/usr/lib/gcc-cross/riscv64-linux-gnu/13/libgcc_s.so",
            ],
        )]
    } else {
        Vec::new()
    }
}

fn emit_libctest_extra(code: &mut String, manifest_dir: &PathBuf, target: &str, out_dir: &PathBuf) {
    let source = manifest_dir.join("user").join("libctest_extra.c");
    println!("cargo:rerun-if-changed={}", source.display());
    if !source.is_file() {
        println!(
            "cargo:warning=skip libc-test extra runners: source missing at {}",
            source.display()
        );
        return;
    }

    let specs = [
        LibcTestExtraSpec {
            libc: "musl",
            install_path: "/musl/libctest-extra-static.exe",
            entry_name: "entry-static.exe",
            static_link: true,
            musl_pleval: true,
        },
        LibcTestExtraSpec {
            libc: "musl",
            install_path: "/musl/libctest-extra-dynamic.exe",
            entry_name: "entry-dynamic.exe",
            static_link: false,
            musl_pleval: false,
        },
    ];

    for spec in specs {
        let Some(cc) = find_libctest_extra_cc(target, spec.libc) else {
            println!(
                "cargo:warning=skip {}: no {} compiler for target {}",
                spec.install_path, spec.libc, target
            );
            continue;
        };
        match compile_libctest_extra(&cc, &source, out_dir, target, &spec) {
            Ok(binary) => {
                code.push_str(&format!(
                    "    crate::fs::add_user_program({:?}, include_bytes!({:?}));\n",
                    spec.install_path,
                    binary.to_string_lossy()
                ));
            }
            Err(err) => {
                println!("cargo:warning=skip {}: {}", spec.install_path, err);
            }
        }
    }
}

fn compile_libctest_extra(
    cc: &str,
    source: &PathBuf,
    out_dir: &PathBuf,
    target: &str,
    spec: &LibcTestExtraSpec,
) -> Result<PathBuf, String> {
    let arch = if target.contains("riscv64") {
        "riscv64"
    } else if target.contains("loongarch") {
        "loongarch64"
    } else {
        return Err(format!("unsupported target {target}"));
    };
    let link_kind = if spec.static_link {
        "static"
    } else {
        "dynamic"
    };
    let out = out_dir
        .join("libctest-extra")
        .join(arch)
        .join(spec.libc)
        .join(format!("libctest-extra-{link_kind}.exe"));
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent).map_err(|err| format!("create {}: {err}", parent.display()))?;
    }

    let mut args = vec![
        "-Os".to_string(),
        "-s".to_string(),
        "-std=c99".to_string(),
        "-D_POSIX_C_SOURCE=200809L".to_string(),
        "-DHAVE_CRYPT=1".to_string(),
        format!("-DENTRY_NAME=\"{}\"", spec.entry_name),
    ];
    if spec.static_link {
        args.push("-static".to_string());
    }
    if spec.musl_pleval {
        args.push("-DHAVE_MUSL_PLEVAL=1".to_string());
    }
    args.push(source.to_string_lossy().into_owned());
    args.push("-o".to_string());
    args.push(out.to_string_lossy().into_owned());
    if spec.static_link {
        args.push("-lcrypt".to_string());
    } else {
        args.push("-Wl,-Bstatic".to_string());
        args.push("-lcrypt".to_string());
        args.push("-Wl,-Bdynamic".to_string());
    }

    let output = Command::new(cc)
        .args(&args)
        .output()
        .map_err(|err| format!("launch {cc}: {err}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(format!(
            "{cc} failed with status {} stdout={} stderr={}",
            output.status, stdout, stderr
        ));
    }

    Ok(out)
}

fn find_libctest_extra_cc(target: &str, libc: &str) -> Option<String> {
    let arch_key = if target.contains("riscv64") {
        "RISCV64"
    } else if target.contains("loongarch") {
        "LOONGARCH64"
    } else {
        return None;
    };
    let env_key = format!(
        "LIBCTEST_EXTRA_{}_{}_CC",
        arch_key,
        libc.to_ascii_uppercase()
    );
    if let Ok(cc) = env::var(&env_key) {
        return Some(cc);
    }

    for candidate in libctest_extra_cc_candidates(target, libc) {
        if command_is_available(candidate) {
            return Some((*candidate).to_string());
        }
    }
    None
}

fn libctest_extra_cc_candidates(target: &str, libc: &str) -> &'static [&'static str] {
    match (target.contains("riscv64"), target.contains("loongarch"), libc) {
        (true, _, "glibc") => &["riscv64-linux-gnu-gcc", "/usr/bin/riscv64-linux-gnu-gcc"],
        (true, _, "musl") => &[
            "/opt/riscv64-linux-musl-cross/bin/riscv64-linux-musl-gcc",
            "riscv64-linux-musl-gcc",
        ],
        (_, true, "glibc") => &[
            "/opt/gcc-13.2.0-loongarch64-linux-gnu/bin/loongarch64-linux-gnu-gcc",
            "/opt/toolchain-loongarch64-linux-gnu-gcc8-host-x86_64-2022-07-18/bin/loongarch64-linux-gnu-gcc",
            "loongarch64-linux-gnu-gcc",
        ],
        (_, true, "musl") => &[
            "/opt/loongarch64-linux-musl-cross/bin/loongarch64-linux-musl-gcc",
            "loongarch64-linux-musl-gcc",
        ],
        _ => &[],
    }
}

fn command_is_available(command: &str) -> bool {
    Command::new(command)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn candidate_paths(target: &str) -> &'static [(&'static str, &'static str)] {
    let _ = target;
    &[("/init", "init")]
}

fn basename(path: &str) -> &str {
    path.rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or(path)
}

fn normalize_image_path(path: &str) -> String {
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    if parts.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", parts.join("/"))
    }
}

fn script_preload_rank(path: &str) -> usize {
    match normalize_image_path(path).as_str() {
        "/glibc/basic_testcode.sh" => 0,
        "/musl/basic_testcode.sh" => 1,
        path if path.starts_with("/glibc/") => 10,
        path if path.starts_with("/musl/") => 20,
        _ => 30,
    }
}

fn push_script_and_execs(fs: &Ext4, selected: &mut Vec<(String, String)>, script: &str) {
    selected.push((script.to_string(), basename(script).to_string()));
    if let Ok(bytes) = fs.read(script) {
        if let Ok(text) = core::str::from_utf8(&bytes) {
            for name in extract_exec_names(text) {
                if let Some(actual) = find_by_basename(fs, "/", &name, 6) {
                    selected.push((actual, name));
                }
            }
        }
    }
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
        if should_skip_walk_path(&path) {
            continue;
        }
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

fn collect_basic_files(fs: &Ext4, dir: &str, depth: usize, out: &mut Vec<String>) {
    if depth == 0 {
        return;
    }
    if let Ok(entries) = fs.read_dir(dir) {
        for entry in entries.flatten() {
            if let Ok(path) = entry.path().to_str().map(|s| s.to_string()) {
                if should_skip_walk_path(&path) {
                    continue;
                }
                if let Ok(meta) = entry.metadata() {
                    if meta.is_dir() {
                        collect_basic_files(fs, &path, depth - 1, out);
                    } else {
                        let normalized = normalize_image_path(&path);
                        if normalized.starts_with("/glibc/basic/")
                            || normalized.starts_with("/musl/basic/")
                        {
                            out.push(path);
                        }
                    }
                }
            }
        }
    }
}

fn collect_test_scripts(fs: &Ext4, dir: &str, depth: usize, out: &mut Vec<String>) {
    if depth == 0 {
        return;
    }
    if let Ok(entries) = fs.read_dir(dir) {
        for entry in entries.flatten() {
            if let Ok(path) = entry.path().to_str().map(|s| s.to_string()) {
                if should_skip_walk_path(&path) {
                    continue;
                }
                if let Ok(meta) = entry.metadata() {
                    if meta.is_dir() {
                        // Recurse into subdirectories to find *_testcode.sh files
                        collect_test_scripts(fs, &path, depth - 1, out);
                    } else {
                        // Only collect *_testcode.sh files, not run-all.sh
                        // run-all.sh is handled by parse_basic_script when it finds ./run-all.sh
                        let base = basename(&path);
                        if base.ends_with("_testcode.sh") {
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

fn should_skip_walk_path(path: &str) -> bool {
    let base = basename(path);
    base == "." || base == ".."
}
