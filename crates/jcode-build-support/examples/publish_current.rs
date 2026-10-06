// One-shot helper: publish the already-built target/selfdev binary into
// ~/.jcode/builds/versions/<label>/ using the SAME label + fingerprint logic
// the daemon uses, then print the label so `jcode server promote <label>` can
// pin it.
//
// Why this exists: `selfdev build-reload` requires an interactive self-dev
// session, and `jcode debug reload` requires server-side debug control
// (JCODE_DEBUG_CONTROL / display.debug_socket), neither of which is available
// from a plain session. Reimplementing the label in shell would risk drifting
// from `current_source_state`, so this calls the real function instead.
fn main() -> anyhow::Result<()> {
    let repo = std::env::args().nth(1).unwrap_or_else(|| ".".to_string());
    let repo = std::path::PathBuf::from(repo);
    let source = jcode_build_support::current_source_state(&repo)?;
    println!("label={}", source.version_label);
    println!("fingerprint={}", source.fingerprint);
    let published = jcode_build_support::publish_local_current_build_for_source(&repo, &source)?;
    println!("installed={}", published.versioned_path.display());
    jcode_build_support::smoke_test_server_binary(&published.versioned_path)?;
    println!("smoke_test=ok");
    Ok(())
}
