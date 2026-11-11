//! The ADR 0003 replay gate ships its own stdlib test suite; run it from the
//! Rust gate so it cannot rot. Skipped, with a message, where `python3` is
//! absent: the replay is a development tool, not part of the daemon.

#[test]
fn the_replay_gate_passes_its_own_suite() {
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root");
    let tool = repo.join("tools/replay-bias");
    if !tool.join("replay.py").exists() {
        eprintln!("skipping: {} is not in this tree", tool.display());
        return;
    }
    let out = match std::process::Command::new("python3")
        .args(["-m", "unittest", "discover", "-s"])
        .arg(&tool)
        .args(["-p", "test_*.py"])
        .current_dir(repo)
        .output()
    {
        Ok(out) => out,
        Err(e) => {
            eprintln!("skipping: python3 unavailable ({e})");
            return;
        }
    };
    assert!(
        out.status.success(),
        "replay-bias tests failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
