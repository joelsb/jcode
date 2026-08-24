//! Direct Kitty-graphics capability query.
//!
//! `ratatui-image`'s `from_query_stdio` refuses to even ask about Kitty when
//! `WEZTERM_EXECUTABLE` is set, because WezTerm implements the Unicode
//! placeholder part of the protocol incorrectly. That blacklist is
//! unconditional and not reachable through `QueryStdioOptions`, so the probe
//! reports `Halfblocks` for every WezTerm user.
//!
//! Inside a multiplexer the blacklist's premise no longer holds. Herdr with
//! `experimental.kitty_graphics = true` answers the Kitty query itself and
//! composites the image, so the outer terminal's own placeholder handling is
//! not what draws the diagram. Believing the blacklist there costs the user a
//! real image protocol and leaves them with halfblocks, which is two pixels
//! per cell and visibly blurry.
//!
//! So ask the terminal the one question that matters. The query is the same one
//! `ratatui-image` would send (`_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA`), paired with
//! a Device Status Report so a terminal that does not implement Kitty still
//! produces a reply and the read cannot hang waiting for silence.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Cached answer, so the query runs at most once per process.
///
/// This exists for correctness, not speed. The query is only answerable before
/// the TUI puts the terminal in raw mode and attaches its own event reader:
/// afterwards the reply is consumed as ordinary keyboard input and the probe
/// times out. So the answer is captured early (see [`prewarm`]) and read back
/// later, exactly as `theme_detect` does for its OSC 11 background query.
static CACHED: OnceLock<Option<bool>> = OnceLock::new();

/// Ask the terminal about Kitty graphics now and remember the answer.
///
/// Call before raw mode, while nothing else is reading stdin. Idempotent.
pub fn prewarm() {
    let answer = *CACHED.get_or_init(|| query_terminal(QueryWindow::BeforeRawMode));
    crate::log_info(&format!("kitty query prewarm result: {answer:?}"));
}

/// Ask the terminal during an `exec` handoff, where it is already in raw mode.
///
/// A reload/rebuild/update `exec`s onto a terminal the previous process
/// deliberately left raw, so [`prewarm`] refuses to ask and the new process
/// inherits no answer. Seeding from the previous process cannot fix this on its
/// own: the answer has to originate somewhere, and every process in a reload
/// chain is in exactly this position, so the whole chain stays blind and
/// mermaid renders as halfblocks forever.
///
/// Raw mode was never the actual hazard. The hazard is a *competing reader*:
/// once the TUI's event reader is attached it wins the race for the reply and
/// types `_Gi=31;OK\` into the user's composer. This window is before that
/// reader exists, so the query is safe here even though the terminal is raw.
///
/// Call only from the exec-handoff path, before the TUI attaches its reader.
pub fn prewarm_in_exec_window() {
    let answer = *CACHED.get_or_init(|| query_terminal(QueryWindow::ExecHandoff));
    crate::log_info(&format!("kitty query exec-window result: {answer:?}"));
}

/// Install an answer obtained by a previous process instead of querying.
///
/// A reload/rebuild/update `exec`s a new binary onto a terminal that is still
/// in raw mode with the alternate screen active. [`prewarm`] refuses to query
/// there (correctly: the reply would land in the user's composer), so the new
/// process starts with no answer and every mermaid diagram falls back to
/// halfblocks. The terminal did not change across the `exec`, so the previous
/// process's answer is still true and is handed over the same way the detected
/// theme is.
///
/// Idempotent and never overrides an answer this process obtained itself.
pub fn seed_answer(answer: bool) {
    let installed = *CACHED.get_or_init(|| Some(answer));
    crate::log_info(&format!(
        "kitty query seeded from exec handoff: {answer} (effective {installed:?})"
    ));
}

/// The answer this process holds, for handing to a process we `exec` into.
///
/// `None` when the question was never definitively answered, in which case
/// there is nothing worth passing on.
pub fn cached_answer() -> Option<bool> {
    CACHED.get().copied().flatten()
}

/// The cached Kitty-graphics answer.
///
/// `None` means the question was never answerable: no prewarm ran before raw
/// mode, there is no tty, or the terminal did not reply in time. That is
/// different from a definite `Some(false)`.
///
/// This deliberately never queries. By the time a picker is built the terminal
/// is normally already in raw mode with an event reader attached, where the
/// reply is stolen and shows up as typed text in the user's composer. The
/// query belongs in [`prewarm`], before raw mode.
pub(super) fn terminal_speaks_kitty_graphics() -> Option<bool> {
    CACHED.get().copied().flatten()
}

/// When the query is being attempted, which decides what makes it safe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum QueryWindow {
    /// Normal startup: the terminal is still cooked and no reader is attached.
    BeforeRawMode,
    /// Straight after an `exec` handoff: the terminal is raw (inherited from
    /// the previous process) but no reader is attached yet.
    ExecHandoff,
}

/// Whether it is safe to write the query to the terminal and read the reply.
///
/// The hazard is a competing reader, not raw mode itself. Once the TUI's event
/// reader is attached it wins the race for the reply, decodes it as keystrokes,
/// and the user watches `_Gi=31;OK\` get typed into their composer. That
/// happened in a live session.
///
/// On the normal startup path a raw terminal is the best available proxy for
/// "someone else got here first", so it is still refused: `raw_mode` of `None`
/// or `Some(true)` means do not ask. A missed detection costs a blurry diagram,
/// while a leak corrupts the user's input.
///
/// The exec-handoff window is the documented exception. There the terminal is
/// raw *because the previous process left it that way*, and jcode has not yet
/// attached its reader, so the raw flag carries no information about a
/// competing reader and refusing on it just makes the answer unobtainable for
/// the entire life of a reload chain.
fn safe_to_query(raw_mode: Option<bool>, window: QueryWindow) -> bool {
    match window {
        QueryWindow::BeforeRawMode => raw_mode == Some(false),
        QueryWindow::ExecHandoff => true,
    }
}

/// How long to wait for the terminal to answer. The DSR reply normally arrives
/// in single-digit milliseconds; this bound only matters for a terminal that
/// answers nothing at all, where it is the whole cost of the probe.
const REPLY_TIMEOUT: Duration = Duration::from_millis(400);

/// Ask the controlling terminal whether it speaks the Kitty graphics protocol.
///
/// Returns `None` when the question could not be asked or answered (no tty,
/// raw mode unavailable, no reply within [`REPLY_TIMEOUT`]), which is different
/// from a definite `Some(false)` and lets the caller keep its prior decision.
#[cfg(unix)]
fn query_terminal(window: QueryWindow) -> Option<bool> {
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;

    // Refuse to ask when another reader may already own the reply. See
    // [`safe_to_query`] for why the exec-handoff window is exempt.
    if !safe_to_query(crossterm::terminal::is_raw_mode_enabled().ok(), window) {
        crate::log_info(&format!(
            "kitty query: skipped, window={:?} raw_mode={:?}",
            window,
            crossterm::terminal::is_raw_mode_enabled()
        ));
        return None;
    }

    let mut tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .map_err(|e| crate::log_info(&format!("kitty query: cannot open /dev/tty: {e}")))
        .ok()?;
    let fd = tty.as_raw_fd();

    // Raw mode: the replies are escape sequences, and a cooked tty would both
    // echo them into the user's screen and withhold them until a newline that
    // never comes.
    let saved = match raw_mode(fd) {
        Some(saved) => saved,
        None => {
            crate::log_info("kitty query: cannot set raw mode on /dev/tty");
            return None;
        }
    };
    let result = (|| {
        if let Err(e) = tty.write_all(b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[5n") {
            crate::log_info(&format!("kitty query: write failed: {e}"));
            return None;
        }
        if let Err(e) = tty.flush() {
            crate::log_info(&format!("kitty query: flush failed: {e}"));
            return None;
        }

        let deadline = Instant::now() + REPLY_TIMEOUT;
        let mut reply = Vec::with_capacity(64);
        loop {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                crate::log_info("kitty query: deadline elapsed");
                return None;
            };
            if !readable(fd, remaining) {
                crate::log_info(&format!(
                    "kitty query: no reply within timeout, got {:?}",
                    String::from_utf8_lossy(&reply)
                ));
                return None;
            }
            let mut chunk = [0u8; 64];
            match tty.read(&mut chunk) {
                Ok(0) => {
                    crate::log_info("kitty query: tty read returned EOF");
                    return None;
                }
                Ok(n) => reply.extend_from_slice(&chunk[..n]),
                Err(e) => {
                    crate::log_info(&format!("kitty query: tty read failed: {e}"));
                    return None;
                }
            }
            // A Kitty-capable terminal answers `_Gi=31;OK` before the DSR. The
            // DSR reply (`\x1b[0n`) is the terminator: it proves the terminal
            // processed the whole query, so its absence of a Kitty reply is
            // a real "no" rather than a read that finished too early.
            let text = String::from_utf8_lossy(&reply);
            if text.contains("_Gi=31;OK") {
                return Some(true);
            }
            if text.contains("\x1b[0n") {
                return Some(false);
            }
            if reply.len() > 512 {
                return None;
            }
        }
    })();
    restore_mode(fd, saved);
    result
}

#[cfg(not(unix))]
fn query_terminal(_window: QueryWindow) -> Option<bool> {
    None
}

/// Put the tty in raw mode, returning the previous settings to restore.
#[cfg(unix)]
fn raw_mode(fd: std::os::fd::RawFd) -> Option<libc::termios> {
    let mut saved: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, &mut saved) } != 0 {
        return None;
    }
    let mut raw = saved;
    unsafe { libc::cfmakeraw(&mut raw) };
    // Non-blocking reads; the timeout is enforced by `readable` via poll.
    raw.c_cc[libc::VMIN] = 0;
    raw.c_cc[libc::VTIME] = 0;
    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
        return None;
    }
    Some(saved)
}

#[cfg(unix)]
fn restore_mode(fd: std::os::fd::RawFd, saved: libc::termios) {
    unsafe { libc::tcsetattr(fd, libc::TCSADRAIN, &saved) };
}

/// Wait for the tty to have bytes to read, or the timeout to expire.
#[cfg(unix)]
fn readable(fd: std::os::fd::RawFd, timeout: Duration) -> bool {
    let mut poll_fd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let millis = timeout.as_millis().min(i32::MAX as u128) as i32;
    unsafe { libc::poll(&mut poll_fd, 1, millis) > 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The query writes an escape sequence to the tty and reads the reply back.
    /// Once the TUI holds the terminal in raw mode its own event reader wins
    /// that race, decodes the reply as keystrokes, and the user watches
    /// `_Gi=31;OK\` get typed into their composer. That happened in a live
    /// session, so refusing to ask is the actual fix.
    #[test]
    fn query_is_refused_unless_the_terminal_is_known_to_be_cooked() {
        assert!(
            safe_to_query(Some(false)),
            "a cooked terminal is the one safe case"
        );
        assert!(
            !safe_to_query(Some(true)),
            "raw mode means a reader owns stdin and would eat the reply"
        );
        assert!(
            !safe_to_query(None),
            "unknown terminal state must not be treated as safe"
        );
    }

    /// The accessor must never trigger a query of its own: it is called while
    /// building the picker, which happens after raw mode is established.
    #[test]
    fn accessor_reports_unknown_rather_than_querying() {
        // Nothing prewarmed in this test process, so the answer is unknown and
        // the caller keeps whatever decision it already had.
        assert_eq!(terminal_speaks_kitty_graphics(), None);
    }
}
