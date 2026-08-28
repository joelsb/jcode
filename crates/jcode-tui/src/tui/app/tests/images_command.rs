// `/images` is the only recoverable route back to visible inline images.
//
// The keyboard toggle is Alt+Shift+I. Several macOS terminals deliver
// Option+Shift+I as the dead key `ˆ` and swallow it, so a user whose
// persisted preference is `inline_images_visible: false` has no keyboard way
// out: every image in every session renders as a one-line `show image` stub
// until the preference file is hand-edited. That is what happened in practice.
//
// These tests pin the recovery path: an explicit `on` must produce visible
// images regardless of the starting state, and the command must be reachable
// from the remote client, since visibility is client render state the server
// knows nothing about.

use crate::tui::app::tests::create_test_app;

/// `set_inline_images_visible` persists to the real UI preferences file, so
/// every test here must be pointed at a throwaway `JCODE_HOME`. Without this
/// the suite would rewrite the developer's own preference (and, worse, could
/// leave it hidden).
struct PrefsSandbox {
    _temp: tempfile::TempDir,
    prev_home: Option<std::ffi::OsString>,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl PrefsSandbox {
    fn new() -> Self {
        let guard = crate::storage::lock_test_env();
        let temp = tempfile::tempdir().expect("tempdir");
        let prev_home = std::env::var_os("JCODE_HOME");
        crate::env::set_var("JCODE_HOME", temp.path());
        Self {
            _temp: temp,
            prev_home,
            _guard: guard,
        }
    }
}

impl Drop for PrefsSandbox {
    fn drop(&mut self) {
        match self.prev_home.take() {
            Some(prev) => crate::env::set_var("JCODE_HOME", prev),
            None => crate::env::remove_var("JCODE_HOME"),
        }
    }
}

#[test]
fn images_on_makes_inline_images_visible_from_the_hidden_state() {
    use crate::tui::TuiState as _;

    let _sandbox = PrefsSandbox::new();
    let mut app = create_test_app();
    app.set_inline_images_visible(false);
    assert!(
        !app.inline_images_visible(),
        "precondition: start from the stuck hidden state"
    );

    assert!(
        crate::tui::app::commands::handle_images_command(&mut app, "/images on"),
        "/images on must be claimed by the handler"
    );

    assert!(
        app.inline_images_visible(),
        "/images on must make inline images visible again"
    );
}

#[test]
fn images_on_is_idempotent_not_a_toggle() {
    // A blind toggle is not a recovery path: from a state the user cannot see,
    // it is a coin flip. Running `on` twice must still end up on.
    use crate::tui::TuiState as _;

    let _sandbox = PrefsSandbox::new();
    let mut app = create_test_app();
    app.set_inline_images_visible(false);

    crate::tui::app::commands::handle_images_command(&mut app, "/images on");
    crate::tui::app::commands::handle_images_command(&mut app, "/images on");

    assert!(
        app.inline_images_visible(),
        "/images on must be idempotent, not a toggle"
    );
}

#[test]
fn images_off_hides_them() {
    use crate::tui::TuiState as _;

    let _sandbox = PrefsSandbox::new();
    let mut app = create_test_app();
    app.set_inline_images_visible(true);

    assert!(crate::tui::app::commands::handle_images_command(
        &mut app,
        "/images off"
    ));

    assert!(
        !app.inline_images_visible(),
        "/images off must hide inline images"
    );
}

#[test]
fn bare_images_reports_status_without_changing_it() {
    use crate::tui::TuiState as _;

    let _sandbox = PrefsSandbox::new();
    let mut app = create_test_app();
    app.set_inline_images_visible(false);

    assert!(crate::tui::app::commands::handle_images_command(
        &mut app, "/images"
    ));

    assert!(
        !app.inline_images_visible(),
        "a bare /images must report state, not change it"
    );
}

#[test]
fn unrelated_input_is_not_claimed() {
    let mut app = create_test_app();
    assert!(
        !crate::tui::app::commands::handle_images_command(&mut app, "/imagesomething"),
        "the handler must not claim a command that merely shares a prefix"
    );
    assert!(
        !crate::tui::app::commands::handle_images_command(&mut app, "/model"),
        "the handler must not claim unrelated commands"
    );
}

/// The remote client is the path users actually hit. Inline-image visibility is
/// client-side render state, so forwarding `/images` to the server would answer
/// "unknown command" and leave the user stuck. Pin the call site by source, the
/// same way the shared-dispatch-table test does.
#[test]
fn remote_client_handles_images_locally() {
    let source = include_str!("../remote/key_handling.rs");
    assert!(
        source.contains("handle_images_command"),
        "the remote client must handle /images locally, or the recovery path \
         does not exist in the product TUI"
    );
}

/// A command missing from the registry does not autocomplete and does not
/// appear in `/help`, which for a recovery command means nobody finds it.
#[test]
fn images_is_a_registered_public_command() {
    let source = include_str!("../state_ui_input_helpers.rs");
    assert!(
        source.contains("\"/images\""),
        "/images must be registered so it autocompletes and shows in /help"
    );
}
