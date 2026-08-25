use super::*;

#[test]
fn desired_nofile_soft_limit_only_raises_when_possible() {
    assert_eq!(desired_nofile_soft_limit(1024, 524_288, 8192), Some(8192));
    assert_eq!(desired_nofile_soft_limit(8192, 524_288, 8192), None);
    assert_eq!(desired_nofile_soft_limit(1024, 4096, 8192), Some(4096));
}

#[cfg(unix)]
#[test]
fn spawn_detached_creates_new_session() {
    use tempfile::NamedTempFile;

    let output = NamedTempFile::new().expect("temp file");
    let output_path = output.path().to_string_lossy().to_string();
    let parent_sid = unsafe { libc::getsid(0) };

    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c")
        .arg("ps -o sid= -p $$ > \"$JCODE_TEST_OUTPUT\"")
        .env("JCODE_TEST_OUTPUT", &output_path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    let mut child = super::spawn_detached(&mut cmd).expect("spawn detached child");
    let status = child.wait().expect("wait for child");
    assert!(status.success(), "child should exit successfully");

    let child_sid = std::fs::read_to_string(&output_path)
        .expect("read child sid")
        .trim()
        .parse::<u32>()
        .expect("parse child sid");

    assert_eq!(
        child_sid,
        child.id(),
        "detached child should lead its own session"
    );
    assert_ne!(
        child_sid as i32, parent_sid,
        "detached child should not share parent session"
    );
}

#[cfg(windows)]
#[test]
fn is_process_running_reports_exited_children_as_stopped() {
    use std::process::{Command, Stdio};
    use std::time::Duration;

    let mut cmd = Command::new("cmd.exe");
    cmd.args(["/C", "ping -n 3 127.0.0.1 >NUL"])
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    let mut child = cmd.spawn().expect("spawn child");
    let pid = child.id();
    assert!(
        super::is_process_running(pid),
        "child should initially be running"
    );

    let status = child.wait().expect("wait for child");
    assert!(status.success(), "child should exit successfully");
    std::thread::sleep(Duration::from_millis(100));

    assert!(
        !super::is_process_running(pid),
        "exited child should not be reported as running"
    );
}

#[cfg(windows)]
#[test]
fn signal_detached_process_group_terminates_descendant_tree() {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let temp = tempfile::tempdir().expect("temp dir");
    let ready_path = temp.path().join("child-ready.txt");
    let survived_path = temp.path().join("child-survived.txt");
    let child_script_path = temp.path().join("child.cmd");
    let parent_script_path = temp.path().join("parent.cmd");
    let child_script = concat!(
        "@echo off\r\n",
        "echo ready>\"%~dp0child-ready.txt\"\r\n",
        "ping -n 6 127.0.0.1 >NUL\r\n",
        "echo survived>\"%~dp0child-survived.txt\"\r\n"
    );
    let parent_script = concat!(
        "@echo off\r\n",
        "start \"\" /B cmd.exe /D /C \"\"%~dp0child.cmd\"\"\r\n",
        "ping -n 30 127.0.0.1 >NUL\r\n"
    );
    std::fs::write(&child_script_path, child_script).expect("write child command script");
    std::fs::write(&parent_script_path, parent_script).expect("write parent command script");
    let mut cmd = Command::new("cmd.exe");
    cmd.args(["/D", "/C"])
        .arg(&parent_script_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    let mut parent = super::spawn_detached(&mut cmd).expect("spawn detached process tree");
    let parent_pid = parent.id();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready_path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(ready_path.exists(), "descendant should report ready");
    assert!(super::is_process_running(parent_pid));

    super::signal_detached_process_group(parent_pid, 0).expect("terminate process tree");
    let deadline = Instant::now() + Duration::from_secs(10);
    while super::is_process_running(parent_pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = parent.wait();

    assert!(!super::is_process_running(parent_pid), "parent should stop");
    std::thread::sleep(Duration::from_secs(6));
    assert!(
        !survived_path.exists(),
        "descendant should not survive termination of the detached process tree"
    );
}

#[cfg(windows)]
#[test]
fn spawn_replacement_process_returns_without_waiting_for_child_exit() {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let mut cmd = Command::new("cmd.exe");
    cmd.args(["/C", "ping -n 4 127.0.0.1 >NUL"])
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    let start = Instant::now();
    let mut child = super::spawn_replacement_process(&mut cmd)
        .expect("spawn replacement process should succeed");
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_secs(1),
        "replacement spawn should not block, took {:?}",
        elapsed
    );
    assert!(
        child.try_wait().expect("poll child status").is_none(),
        "replacement child should still be running immediately after spawn"
    );

    child.kill().ok();
    let _ = child.wait();
}

/// A zombie is a child that exited but whose parent never `wait()`ed for it.
/// It holds a process-table slot until the parent dies, so a long-lived server
/// that fire-and-forgets children eventually cannot `fork()` at all. Reproduced
/// on 2026-08-25: 2254 zombies under one 10h-old shared server against a
/// `kern.maxprocperuid` of 2666.
///
/// This asserts the observable OS state (state `Z` in `ps`), not that some
/// helper was called, because the whole failure mode is invisible in code shape.
#[cfg(unix)]
fn process_is_zombie(pid: u32) -> bool {
    let Ok(output) = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
    else {
        return false;
    };
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .starts_with('Z')
}

#[cfg(unix)]
#[test]
fn dropping_a_detached_child_leaks_a_zombie() {
    // Guard test: proves the defect is real and that `process_is_zombie`
    // actually detects it. Without this, the fix test below could pass because
    // the detector never fires rather than because the reaper works.
    let mut cmd = std::process::Command::new("true");
    cmd.stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    let child = super::spawn_detached(&mut cmd).expect("spawn");
    let pid = child.id();
    drop(child); // std::process::Child::drop does NOT wait.

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !process_is_zombie(pid) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    assert!(
        process_is_zombie(pid),
        "a dropped detached child must become a zombie, otherwise this test \
         cannot prove the reaper below is what removes it"
    );

    // Do not leak from the test process itself.
    super::try_reap_child_process(pid).ok();
}

#[cfg(unix)]
#[test]
fn spawn_detached_reaped_leaves_no_zombie() {
    let mut cmd = std::process::Command::new("true");
    cmd.stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    let pid = super::spawn_detached_reaped(&mut cmd).expect("spawn");

    // The reaper polls at 250ms; allow several cycles before failing.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut zombie = true;
    while std::time::Instant::now() < deadline {
        if !process_is_zombie(pid) {
            zombie = false;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    assert!(
        !zombie,
        "spawn_detached_reaped must reap the child; pid {pid} is still a zombie"
    );
}

#[cfg(unix)]
#[test]
fn reaper_handles_many_children_without_leaking() {
    // The real workload is many short-lived hook processes, not one. A reaper
    // that only tracked a single child would pass the test above and still
    // leak in production.
    let pids: Vec<u32> = (0..25)
        .map(|_| {
            let mut cmd = std::process::Command::new("true");
            cmd.stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            super::spawn_detached_reaped(&mut cmd).expect("spawn")
        })
        .collect();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let remaining: Vec<u32> = pids
            .iter()
            .copied()
            .filter(|pid| process_is_zombie(*pid))
            .collect();
        if remaining.is_empty() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{} of {} children still zombies: {:?}",
            remaining.len(),
            pids.len(),
            remaining
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}
