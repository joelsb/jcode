// One-shot helper: install the already-built target/release binary as the
// active build, exactly the way the in-session auto-update path does.
//
// Why this exists: `install_local_release` is only reachable from `hot_exec`
// (the auto-update path) and from a self-dev session's rebuild flow. Neither is
// available from a plain session, and doing the four symlink updates by hand in
// shell would risk drifting from the real function - in particular the
// shared-server promotion and its version file, which is what makes a running
// daemon notice the new binary at all.
//
// This promotes onto the shared-server channel, so a running daemon will report
// an update and (with `display.auto_server_reload = true`) reload itself.
fn main() -> anyhow::Result<()> {
    let repo = std::env::args().nth(1).unwrap_or_else(|| ".".to_string());
    let repo = std::path::PathBuf::from(repo);
    let version = jcode_build_support::repo_build_version(&repo)?;
    println!("label={version}");
    let installed = jcode_build_support::install_local_release(&repo)?;
    println!("installed={}", installed.display());
    jcode_build_support::smoke_test_server_binary(&installed)?;
    println!("smoke_test=ok");
    Ok(())
}
