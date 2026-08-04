//! Prompt-driven credential auto-fill for VTE sessions.
//!
//! SSH authenticates before the terminal exists, so its only interactive step
//! is a password prompt. Telnet and a serial console have no authentication
//! protocol at all: the device prints a prompt and expects the account name
//! and the password to be typed, in that order, in the terminal itself.
//!
//! Both cases are the same job — watch the line under the cursor, recognize a
//! prompt, type the matching credential exactly once — so they share this
//! module. Recognition is delegated to
//! [`LoginPromptMatcher`](rustconn_core::LoginPromptMatcher), which is
//! GUI-free and unit-tested, and can be overridden per connection or per
//! group for devices with unusual wording (issue #254).
//!
//! Nothing is ever typed twice: each stage of the sequence fires once. A
//! device that re-prompts after a rejected credential is left to the user, so
//! a wrong stored password cannot walk an account into a lockout.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk4::glib;
use rustconn_core::{LoginPrompt, LoginPromptMatcher};
use secrecy::SecretString;
use uuid::Uuid;

use super::protocols::SharedNotebook;

/// How often the terminal is re-examined while waiting for a prompt.
///
/// VTE does not reliably emit `contents-changed` for a no-echo prompt written
/// with cursor-positioning escapes and no trailing newline (issue #194), so the
/// signals alone can miss it. 150 ms balances responsiveness against wake-ups.
const POLL_INTERVAL: Duration = Duration::from_millis(150);

/// How long to keep watching for a prompt before giving up.
///
/// Covers a slow SSH handshake (DNS + key exchange + banner) and a network
/// device that spends seconds on its login banner. After this the user types
/// the credentials manually. Overridden by
/// [`AutomationConfig::login_timeout_secs`] when set.
const DEFAULT_AUTOFILL_DEADLINE: Duration = Duration::from_secs(10);

/// Credentials to type, and how to recognize the prompts asking for them.
pub(crate) struct LoginAutofill {
    /// Account name to send at the username prompt, if the device asks for one.
    pub username: Option<String>,
    /// Password to send at the password prompt.
    pub password: Option<SecretString>,
    /// Prompt recognition, possibly customized for the device.
    pub matcher: LoginPromptMatcher,
    /// Protocol name for log fields (`"ssh"`, `"telnet"`, `"serial"`).
    pub protocol: &'static str,
    /// How long to wait for a prompt before giving up.
    ///
    /// `None` means use [`DEFAULT_AUTOFILL_DEADLINE`].
    pub deadline_secs: Option<u32>,
}

impl LoginAutofill {
    /// Returns `true` when there is nothing to type.
    pub(crate) const fn is_empty(&self) -> bool {
        self.username.is_none() && self.password.is_none()
    }
}

/// Which credential the watcher is still waiting to send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// Waiting for the username prompt.
    Username,
    /// Waiting for the password prompt.
    Password,
    /// Everything that could be sent has been sent.
    Done,
}

/// Starts watching `session_id` for login prompts and types the credentials.
///
/// Subscribes to both `contents-changed` and `cursor-moved` and additionally
/// polls every [`POLL_INTERVAL`], because either signal can fire before the
/// prompt glyphs reach the grid and a no-echo prompt may not produce another
/// signal afterwards (issue #194). All three paths run the same one-shot step,
/// so nothing is sent twice regardless of which observes the prompt first.
///
/// Does nothing when `spec` carries no credentials.
pub(crate) fn install_login_autofill(
    notebook: &SharedNotebook,
    session_id: Uuid,
    spec: LoginAutofill,
) {
    if spec.is_empty() {
        return;
    }

    let LoginAutofill {
        username,
        password,
        matcher,
        protocol,
        deadline_secs,
    } = spec;

    // Skip straight to the password stage when there is no username to send:
    // that is the SSH case, and also a device that only asks for a password.
    let initial = if username.is_some() {
        Stage::Username
    } else {
        Stage::Password
    };
    let stage = Rc::new(Cell::new(initial));

    let deadline = deadline_secs
        .map(|s| Duration::from_secs(u64::from(s)))
        .unwrap_or(DEFAULT_AUTOFILL_DEADLINE);

    tracing::info!(
        protocol,
        %session_id,
        send_username = username.is_some(),
        send_password = password.is_some(),
        custom_prompts = !matcher.is_default(),
        "Credentials available; will auto-fill on login prompt"
    );

    // One detect-and-send step, shared by the two signals and the timer. The
    // stage is read first, so a stage can never fire twice no matter who calls.
    let step = {
        let notebook = notebook.clone();
        let stage = stage.clone();
        Rc::new(move || {
            if stage.get() == Stage::Done {
                return;
            }
            let Some(line) = notebook.get_cursor_line_text(session_id) else {
                return;
            };
            let Some(prompt) = matcher.classify(&line) else {
                return;
            };

            match (stage.get(), prompt) {
                (Stage::Username, LoginPrompt::Username) => {
                    if let Some(ref user) = username {
                        notebook.send_text_to_session(session_id, &format!("{user}\n"));
                        tracing::info!(protocol, "Username prompt detected; account name sent");
                    }
                    stage.set(if password.is_some() {
                        Stage::Password
                    } else {
                        Stage::Done
                    });
                }
                // The device went straight to the password (or the account name
                // travelled with the command line, as `telnet -l user` does).
                (Stage::Username | Stage::Password, LoginPrompt::Password) => {
                    if let Some(ref secret) = password {
                        use secrecy::ExposeSecret;
                        // Zeroizing so the plaintext is wiped as soon as VTE has
                        // it, rather than lingering in a dropped String.
                        let input =
                            zeroize::Zeroizing::new(format!("{}\n", secret.expose_secret()));
                        notebook.send_text_to_session(session_id, &input);
                        tracing::info!(protocol, "Password prompt detected; password sent");
                    }
                    stage.set(Stage::Done);
                }
                // A username prompt while waiting for the password means the
                // device rejected the credentials and started over. Stop here
                // and let the user decide — retrying automatically is how an
                // account gets locked out.
                (Stage::Password, LoginPrompt::Username) => {
                    tracing::info!(
                        protocol,
                        "Login prompt repeated after credentials were sent; auto-fill stopped"
                    );
                    stage.set(Stage::Done);
                }
                (Stage::Done, _) => {}
            }
        })
    };

    // Polling safety net. Self-cancels on completion, on session close, and at
    // the deadline. `Instant` is fine for a 10 s in-process window; it does not
    // advance across suspend, which for a login window in flight is moot.
    {
        let step = step.clone();
        let stage = stage.clone();
        let notebook = notebook.clone();
        let started = Instant::now();
        glib::timeout_add_local(POLL_INTERVAL, move || {
            if stage.get() == Stage::Done {
                return glib::ControlFlow::Break;
            }
            // The tab was closed or the child exited — stop burning wake-ups.
            if notebook.get_terminal(session_id).is_none() {
                return glib::ControlFlow::Break;
            }
            if started.elapsed() >= deadline {
                tracing::debug!(
                    protocol,
                    %session_id,
                    "No login prompt within the auto-fill window; giving up"
                );
                stage.set(Stage::Done);
                return glib::ControlFlow::Break;
            }
            step();
            if stage.get() == Stage::Done {
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
    }

    // contents-changed: fires for most terminal output.
    let on_contents_changed = step.clone();
    notebook.connect_contents_changed(session_id, move || on_contents_changed());

    // cursor-moved: fires for prompts drawn with cursor-positioning escapes and
    // no trailing newline, which is exactly what a password prompt looks like.
    notebook.connect_cursor_moved(session_id, move || step());
}
