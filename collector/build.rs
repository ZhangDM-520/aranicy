// Compiles the CO-RE BPF object (bpf/collector.bpf.c) with clang -g into
// OUT_DIR so the daemon embeds it (include_bytes! + ObjectBuilder::open_memory,
// no runtime files outside the binary). clang -g embeds BTF; kernel BTF is
// resolved at load time, so no kernel headers are needed here. Same shape as
// crates/aranicy-events/build.rs.
use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=bpf/collector.bpf.c");
    println!("cargo:rerun-if-env-changed=CLANG");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR set by cargo"));
    let src = PathBuf::from("bpf/collector.bpf.c");
    let dst = out_dir.join("collector.bpf.o");
    let clang = env::var("CLANG").unwrap_or_else(|_| "clang".to_owned());

    let out = Command::new(&clang)
        .args(["-g", "-O2", "-Wall", "-target", "bpf"])
        .arg(format!(
            "-D__TARGET_ARCH_{}",
            match env::var("CARGO_CFG_TARGET_ARCH")
                .unwrap_or_default()
                .as_str()
            {
                "x86_64" => "x86",
                "aarch64" => "arm64",
                other => other,
            }
        ))
        .arg("-c")
        .arg(&src)
        .arg("-o")
        .arg(&dst)
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn {clang} (is clang installed?): {e}"));

    if !out.status.success() {
        panic!(
            "failed to compile {} with {clang}:\n{}",
            src.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
