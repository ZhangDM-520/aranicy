// Minimal libbpf-rs 0.27 loader for the falsification probe.
// Usage: rust-loader <obj> {load-only|test} [N]
use libbpf_rs::{MapCore, MapFlags, ObjectBuilder, PrintLevel};
use std::ffi::OsStr;
use std::process::Command;

fn print_cb(level: PrintLevel, msg: String) {
    eprint!("[libbpf:{:?}] {}", level, msg);
}

fn do_execs(n: u32) -> u32 {
    let mut ok = 0u32;
    for _ in 0..n {
        if let Ok(st) = Command::new("/bin/true").status() {
            if st.success() {
                ok += 1;
            }
        }
    }
    ok
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: {} <obj> {{load-only|test}} [N]", args[0]);
        std::process::exit(2);
    }
    let mode = args[2].clone();
    let n: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(100);

    libbpf_rs::set_print(Some((PrintLevel::Debug, print_cb)));

    let mut builder = ObjectBuilder::default();
    let open_obj = match builder.open_file(&args[1]) {
        Ok(o) => {
            eprintln!("OPEN-OK");
            o
        }
        Err(e) => {
            eprintln!("OPEN-FAIL: {}", e);
            std::process::exit(1);
        }
    };

    let obj = match open_obj.load() {
        Ok(o) => {
            eprintln!("LOAD-OK");
            o
        }
        Err(e) => {
            eprintln!("LOAD-FAIL: {}", e);
            std::process::exit(1);
        }
    };

    if mode == "load-only" {
        eprintln!("RESULT: load-only OK");
        return;
    }

    let mut links = Vec::new();
    let mut attach_failed = false;
    for prog in obj.progs_mut() {
        let name = prog.name().to_string_lossy().into_owned();
        match prog.attach() {
            Ok(l) => {
                eprintln!("ATTACH-OK prog={}", name);
                links.push(l);
            }
            Err(e) => {
                eprintln!("ATTACH-FAIL prog={}: {}", name, e);
                attach_failed = true;
            }
        }
    }

    let ok = do_execs(n);
    eprintln!("EXECS-DONE ok={} requested={}", ok, n);

    // Read the counter map (percpu array "exec_count"), sum across CPUs.
    for map in obj.maps() {
        if map.name() != OsStr::new("exec_count") {
            continue;
        }
        let key = 0u32.to_ne_bytes();
        match map.lookup_percpu(&key, MapFlags::ANY) {
            Ok(Some(vals)) => {
                let total: u64 = vals
                    .iter()
                    .map(|v| {
                        if v.len() >= 8 {
                            u64::from_ne_bytes(v[0..8].try_into().unwrap())
                        } else {
                            0
                        }
                    })
                    .sum();
                eprintln!("MAP exec_count TOTAL={}", total);
            }
            Ok(None) => eprintln!("MAP-LOOKUP-EMPTY"),
            Err(e) => eprintln!("MAP-LOOKUP-FAIL: {}", e),
        }
    }

    drop(links); // detach
    std::process::exit(if attach_failed { 1 } else { 0 });
}
