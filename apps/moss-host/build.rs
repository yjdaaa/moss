// Windows MSVC 下静态链接 VC 运行库（libvcruntime / libcmt），消除干净机器缺少
// VCRUNTIME140.dll / VCRUNTIME140_1.dll 导致独立 moss-host 启动失败的问题。

use std::{env, fs, io::Write, path::Path};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let env_abi = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if os == "windows" && env_abi == "msvc" {
        link_static_vcruntime();
    }
}

fn link_static_vcruntime() {
    override_msvcrt_lib();

    println!("cargo:rustc-link-arg=/NODEFAULTLIB:libvcruntimed.lib");
    println!("cargo:rustc-link-arg=/NODEFAULTLIB:vcruntime.lib");
    println!("cargo:rustc-link-arg=/NODEFAULTLIB:vcruntimed.lib");
    println!("cargo:rustc-link-arg=/NODEFAULTLIB:libcmtd.lib");
    println!("cargo:rustc-link-arg=/NODEFAULTLIB:msvcrt.lib");
    println!("cargo:rustc-link-arg=/NODEFAULTLIB:msvcrtd.lib");
    println!("cargo:rustc-link-arg=/NODEFAULTLIB:libucrt.lib");
    println!("cargo:rustc-link-arg=/NODEFAULTLIB:libucrtd.lib");

    println!("cargo:rustc-link-arg=/DEFAULTLIB:libcmt.lib");
    println!("cargo:rustc-link-arg=/DEFAULTLIB:libvcruntime.lib");
    println!("cargo:rustc-link-arg=/DEFAULTLIB:ucrt.lib");
}

fn override_msvcrt_lib() {
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let machine: &[u8] = match arch.as_str() {
        "x86_64" => &[0x64, 0x86],
        "x86" => &[0x4C, 0x01],
        "aarch64" => &[0x64, 0xAA],
        _ => return,
    };
    let bytes: &[u8] = &[
        1, 0, 94, 3, 96, 98, 60, 0, 0, 0, 1, 0, 0, 0, 0, 0, 132, 1, 46, 100, 114, 101, 99, 116,
        118, 101, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 60, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 10, 16, 0, 46, 100, 114, 101, 99, 116, 118, 101, 0, 0, 0, 0, 1, 0, 0, 0, 3, 0, 4, 0,
        0, 0,
    ];

    let out_dir = env::var("OUT_DIR").expect("OUT_DIR");
    let path = Path::new(&out_dir).join("msvcrt.lib");
    if let Ok(mut f) = fs::File::create(&path) {
        let _ = f.write_all(machine);
        let _ = f.write_all(bytes);
    }
    println!("cargo:rustc-link-search=native={out_dir}");
}
