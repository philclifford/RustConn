//! Detects that the local machine was suspended, and reacts to the resume.
//!
//! A suspend leaves every open TCP connection half-open. The kernel needs up to
//! the keepalive detection window to confirm that, and until it does an embedded
//! RDP or VNC session still reports itself as connected while showing a picture
//! that may be minutes old — the freeze reported in issue #248. This module
//! supplies the missing "the machine just woke up" signal so the UI can say so
//! immediately instead of waiting for the timeout.
//!
//! # Why wall-clock time, not `Instant`
//!
//! On Linux `Instant` reads `CLOCK_MONOTONIC`, which does *not* advance while
//! the system is suspended (`CLOCK_BOOTTIME` does). A monotonic timer therefore
//! sees an ordinary one-second tick across a two-hour sleep and cannot detect it
//! at all. `SystemTime` reads `CLOCK_REALTIME`, which keeps counting, so the gap
//! is visible there. The trade-off is that a large clock step — an NTP jump, or
//! the user changing the clock — looks the same. That is acceptable here: the
//! consequence of a false positive is a dimmed frame that the next arriving
//! frame clears, never a dropped session.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use gtk4::glib;

use super::SharedToastOverlay;
use super::types::SharedNotebook;
use crate::i18n::i18n;
use crate::state::SharedAppState;

/// How often the detector samples the wall clock.
///
/// One second is frequent enough to react before the user has finished looking
/// at the screen, and cheap enough to ignore: one clock read per second.
const TICK: Duration = Duration::from_secs(1);

/// Extra wall-clock time beyond [`TICK`] that means the machine was asleep.
///
/// Well above ordinary scheduler delay and main-loop congestion (which cost
/// milliseconds), well below any suspend a user would perform deliberately.
const SUSPEND_THRESHOLD: Duration = Duration::from_secs(10);

/// Delay before the reconnect sweep that follows a resume.
///
/// The network stack needs a moment to bring interfaces back and re-acquire a
/// lease; probing a host the instant the screen lights up reports it as
/// unreachable when it is merely not routable yet.
const RECONNECT_SWEEP_DELAY: Duration = Duration::from_secs(5);

/// Returns how long the machine slept, if the observed gap can only be suspend.
///
/// `observed` is the wall-clock time that actually elapsed between two ticks of
/// a `tick`-interval timer. A gap that exceeds `tick` by at least `threshold` is
/// reported; anything smaller is ordinary timer jitter and yields `None`.
///
/// Pure so the arithmetic can be tested without suspending anything.
#[must_use]
pub fn suspend_gap(tick: Duration, observed: Duration, threshold: Duration) -> Option<Duration> {
    let overshoot = observed.checked_sub(tick)?;
    (overshoot >= threshold).then_some(observed)
}

/// Starts the resume detector.
///
/// On resume: every embedded RDP session that still believes it is connected has
/// its frame dimmed and its reconnect banner shown, so the user can act at once;
/// then, after [`RECONNECT_SWEEP_DELAY`], the ordinary reconnect sweep runs for
/// whatever has meanwhile been confirmed dead. Sessions that outlived the sleep
/// clear their own mark as soon as the next frame arrives, so nothing is torn
/// down on suspicion alone.
///
/// # Note
/// The timer is attached to the thread-default main context for the lifetime of
/// the process, like the network monitor's `network-changed` handler.
pub fn setup_resume_monitor(
    state: &SharedAppState,
    notebook: &SharedNotebook,
    toast_overlay: &SharedToastOverlay,
) {
    let state = state.clone();
    let notebook = notebook.clone();
    let toast_overlay = toast_overlay.clone();

    // SystemTime rather than Instant — see the module docs.
    let last_seen: Rc<Cell<SystemTime>> = Rc::new(Cell::new(SystemTime::now()));

    glib::timeout_add_local(TICK, move || {
        let now = SystemTime::now();
        let previous = last_seen.replace(now);

        // `duration_since` fails when the clock went backwards. Nothing to
        // report: re-baseline (already done by `replace`) and carry on.
        let Ok(observed) = now.duration_since(previous) else {
            tracing::debug!("Wall clock moved backwards; resume detector re-baselined");
            return glib::ControlFlow::Continue;
        };

        let Some(slept_for) = suspend_gap(TICK, observed, SUSPEND_THRESHOLD) else {
            return glib::ControlFlow::Continue;
        };

        tracing::info!(
            slept_secs = slept_for.as_secs(),
            "Machine resumed from sleep; marking embedded sessions as possibly stale"
        );

        let marked = mark_embedded_sessions_stale(&notebook);
        if marked > 0 {
            toast_overlay.show_warning(&i18n(
                "Computer resumed from sleep — checking remote sessions",
            ));
        }

        // Give the network a moment, then let the existing sweep reconnect
        // whatever has actually died. Sessions that survived are untouched.
        // Show a toast only when the sweep actually reconnects something —
        // same rule the network-change handler follows.
        let state_for_sweep = state.clone();
        let notebook_for_sweep = notebook.clone();
        let toast_for_sweep = toast_overlay.clone();
        glib::timeout_add_local_once(RECONNECT_SWEEP_DELAY, move || {
            let reconnecting = super::network_monitor::reconnect_sessions_after_outage(
                &state_for_sweep,
                &notebook_for_sweep,
            );
            if reconnecting > 0 {
                toast_for_sweep.show_toast(&i18n("Reconnecting sessions after sleep"));
            }
        });

        glib::ControlFlow::Continue
    });
}

/// Dims every embedded session that still claims to be connected.
///
/// Returns how many were marked, so the caller only reports a resume the user
/// can actually see the effect of.
fn mark_embedded_sessions_stale(notebook: &SharedNotebook) -> usize {
    let mut marked = 0;

    for info in &notebook.get_all_sessions() {
        if !info.is_embedded {
            continue;
        }
        // VNC has no equivalent marker yet; its keepalive still turns the
        // freeze into a real disconnect, which the sweep below picks up.
        if info.protocol.as_str() == "rdp"
            && let Some(widget) = notebook.get_rdp_widget(info.id)
            && !widget.is_disconnected()
        {
            widget.mark_stale();
            marked += 1;
        }
    }

    marked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ordinary_tick_is_not_a_suspend() {
        assert_eq!(
            suspend_gap(TICK, Duration::from_millis(1_020), SUSPEND_THRESHOLD),
            None
        );
    }

    #[test]
    fn a_late_tick_within_the_threshold_is_not_a_suspend() {
        // Main-loop congestion can delay a timer by seconds without any sleep.
        assert_eq!(
            suspend_gap(TICK, Duration::from_secs(9), SUSPEND_THRESHOLD),
            None
        );
    }

    #[test]
    fn a_gap_at_the_threshold_counts_as_a_suspend() {
        let observed = TICK + SUSPEND_THRESHOLD;
        assert_eq!(
            suspend_gap(TICK, observed, SUSPEND_THRESHOLD),
            Some(observed)
        );
    }

    #[test]
    fn a_long_sleep_reports_its_full_length() {
        let observed = Duration::from_hours(2);
        assert_eq!(
            suspend_gap(TICK, observed, SUSPEND_THRESHOLD),
            Some(observed)
        );
    }

    #[test]
    fn an_observed_gap_shorter_than_the_tick_is_not_a_suspend() {
        // The timer fired early; `checked_sub` must not wrap into a huge value.
        assert_eq!(
            suspend_gap(TICK, Duration::from_millis(500), SUSPEND_THRESHOLD),
            None
        );
    }

    #[test]
    fn threshold_is_larger_than_the_tick_so_jitter_cannot_trigger_it() {
        assert!(
            SUSPEND_THRESHOLD > TICK,
            "a threshold at or below the tick interval would fire on jitter"
        );
    }
}
