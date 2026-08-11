//! Isolated FFI helper for RustConn's startup locale.
//!
//! `setlocale(3)` is the one part of the gettext API that cannot be wrapped
//! safely. It non-atomically replaces process-global locale state and reads the
//! environment without synchronisation, so it is only sound while the process
//! is still single-threaded — the unsoundness reported as
//! [RUSTSEC-2026-0244](https://rustsec.org/advisories/RUSTSEC-2026-0244).
//! `gettext-rs` 0.8 marks it `unsafe` for exactly that reason, and every other
//! crate in this workspace sets `unsafe_code = "forbid"`. Hence this crate.
//!
//! It exposes one operation, [`init_locale`], and one way to close the window
//! in which that operation is allowed, [`seal_locale`]. The rest of the gettext
//! API — `bindtextdomain`, `bind_textdomain_codeset`, `textdomain`, `gettext`,
//! `ngettext` — is safe and is called directly from `rustconn`; deliberately
//! none of it is re-exported here, so this crate stays a wrapper around the one
//! call that needs a contract.
//!
//! # The contract
//!
//! [`init_locale`] may be called only:
//!
//! * from `main()`, before any thread is spawned — that means before a tokio
//!   runtime, before GTK/GIO, and before the tracing subscriber;
//! * from the same thread every time;
//! * before any POSIX signal handler is installed. This is the second half of
//!   the safety contract `gettext-rs` 0.8 states for `setlocale`: call it as
//!   early as possible, before starting more threads *or enabling any POSIX
//!   signals*. A handler that fires mid-call runs on the calling thread and can
//!   observe — or worse, act on — half-replaced locale state;
//! * before [`seal_locale`], which the caller invokes once the startup locale
//!   is final.
//!
//! A violation of a *checked* clause panics rather than silently corrupting
//! locale state. A panic here is a programming bug (M-PANIC-ON-BUG), not a
//! runtime condition: it means a `setlocale` call moved out of the startup
//! window, which no user input can cause.
//!
//! # What is checked, and what is not
//!
//! Not all four clauses are enforceable, so this section says exactly which are.
//!
//! Enforced on every platform:
//!
//! * **Thread identity** — a call from a thread other than the first caller is
//!   refused. This is the clause that still catches a regression off Linux: any
//!   caller that appears after startup necessarily runs on a GTK, GIO or tokio
//!   thread.
//! * **Sealing** — a call after [`seal_locale`] is refused.
//!
//! Enforced on Linux only:
//!
//! * **Thread count** — `/proc/self/task` is counted, and more than one live
//!   thread is refused. No other platform is asked, macOS included, even though
//!   RustConn ships macOS builds: obtaining the count there means
//!   `proc_pidinfo(PROC_PIDTASKINFO)` or `task_threads()`, which would add a
//!   second `unsafe` block and a hand-transcribed C struct layout to the crate
//!   whose entire purpose is to keep the unsafe surface small — and no CI job
//!   builds macOS, so neither would ever be compiled, let alone tested. The
//!   check is defence-in-depth; what makes the call sound is the call site, and
//!   the thread-identity clause above still guards a regression.
//!
//! Not enforced anywhere:
//!
//! * **No signal handlers.** There is no cheap check, and no honest one: the
//!   Rust runtime installs its own `SIGSEGV`/`SIGBUS` handler for
//!   stack-overflow detection and ignores `SIGPIPE` before `main()` is entered,
//!   so "no POSIX signals enabled" is already false in every Rust process. On
//!   Linux the `SigCgt` mask in `/proc/self/status` is therefore non-zero from
//!   the start and cannot distinguish the runtime's handlers from an
//!   application's; a check would refuse every call. RustConn satisfies this
//!   clause by ordering instead: `i18n::init()` is the first statement of
//!   `main()`, ahead of every handler the application or GTK installs.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::ThreadId;

/// The locale category to change, re-exported so callers need not depend on
/// `gettext-rs` just to name one.
pub use gettextrs::LocaleCategory;

// ============================================================================
// Startup guard
// ============================================================================

/// Why a [`init_locale`] call was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    /// The startup window was closed by [`seal_locale`].
    Sealed,
    /// Another thread already exists, so the call would race it.
    MultiThreaded(usize),
    /// An earlier call came from a different thread.
    ForeignThread,
}

impl Refusal {
    /// Renders the refusal as a panic message that names the fix.
    fn describe(self) -> String {
        match self {
            Self::Sealed => "setlocale after seal_locale(): the startup locale is final. \
                 Applying a locale later is unsound (RUSTSEC-2026-0244) — persist the \
                 setting and let it take effect on the next start instead."
                .to_string(),
            Self::MultiThreaded(count) => format!(
                "setlocale in a process with {count} threads: it writes global locale \
                 state without synchronisation, which is unsound once a second thread \
                 exists (RUSTSEC-2026-0244). Move this call earlier in main(), before \
                 GTK, tokio and the tracing subscriber start."
            ),
            Self::ForeignThread => "setlocale from a different thread than the earlier calls: \
                 the startup locale must be applied entirely from the main thread \
                 (RUSTSEC-2026-0244)."
                .to_string(),
        }
    }
}

/// Enforces the startup window in which `setlocale` is sound.
///
/// Kept as a struct rather than three loose statics so the contract can be
/// exercised against a local instance in tests — calling the real
/// [`init_locale`] from a test is impossible by design, since the test harness
/// is itself multi-threaded.
struct StartupGuard {
    sealed: AtomicBool,
    first_caller: OnceLock<ThreadId>,
}

impl StartupGuard {
    const fn new() -> Self {
        Self {
            sealed: AtomicBool::new(false),
            first_caller: OnceLock::new(),
        }
    }

    /// Admits one `setlocale` call, or reports why it must not happen.
    ///
    /// `live_threads` is the caller's thread count if the platform can report
    /// one; `None` skips that check rather than assuming the process is
    /// single-threaded.
    fn admit(&self, current: ThreadId, live_threads: Option<usize>) -> Result<(), Refusal> {
        if self.sealed.load(Ordering::Acquire) {
            return Err(Refusal::Sealed);
        }
        // Checked before the thread identity, because "a second thread exists"
        // is the precondition the advisory is actually about: the first caller
        // spawning a thread and then calling again is just as unsound as a
        // second thread calling.
        if let Some(count) = live_threads
            && count > 1
        {
            return Err(Refusal::MultiThreaded(count));
        }
        if *self.first_caller.get_or_init(|| current) != current {
            return Err(Refusal::ForeignThread);
        }
        Ok(())
    }

    fn seal(&self) {
        self.sealed.store(true, Ordering::Release);
    }

    /// Test-only: nothing in the crate's API needs to read the seal, and there
    /// is no public accessor for it either (see [`seal_locale`]). The tests do,
    /// to check that `seal` and `admit` agree about the same bit.
    #[cfg(test)]
    fn is_sealed(&self) -> bool {
        self.sealed.load(Ordering::Acquire)
    }
}

static GUARD: StartupGuard = StartupGuard::new();

/// Counts the threads in this process, where the OS makes that cheap to ask.
///
/// Linux exposes one directory entry per thread under `/proc/self/task`. On
/// macOS — and on any other platform, and in any sandbox without `/proc` — this
/// returns `None`, the thread-count check is skipped, and only the
/// thread-identity and sealing guards apply. See the crate-level
/// "What is checked, and what is not" for why macOS is not implemented.
fn live_thread_count() -> Option<usize> {
    #[cfg(target_os = "linux")]
    {
        // Counts every yielded entry, including `Err`s: a miscount must never
        // read as "fewer threads than there are".
        std::fs::read_dir("/proc/self/task")
            .ok()
            .map(Iterator::count)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

// ============================================================================
// Locale setup
// ============================================================================

/// Sets `category` to `locale`, returning the locale name that took effect.
///
/// An empty `locale` selects the locale described by the environment, which is
/// what an application wants at startup. `None` means the call failed — almost
/// always because the requested locale is not installed on the system; the C
/// API offers no further detail. On failure the previous locale is unchanged.
///
/// The `None` is worth acting on, hence `#[must_use]`: a discarded failure here
/// is a locale that silently did not apply, which surfaces much later as
/// untranslated UI.
///
/// # Panics
///
/// Panics if one of the *checked* clauses of the contract described at the
/// [crate] level is violated: the call comes from a different thread than
/// earlier calls, [`seal_locale`] has already run, or — on Linux, where the
/// count is available — the process already has more than one thread. Also
/// panics if `locale` contains an interior NUL byte, which cannot be passed to
/// C.
#[must_use = "a None means the locale did not apply; at minimum say why it is being ignored"]
pub fn init_locale(category: LocaleCategory, locale: &str) -> Option<String> {
    if let Err(refusal) = GUARD.admit(std::thread::current().id(), live_thread_count()) {
        panic!("rustconn-locale-sys: {}", refusal.describe());
    }

    // SAFETY: `setlocale` replaces process-global locale state non-atomically
    // and without synchronisation, so it is sound only while nothing else can
    // read or write that state concurrently (RUSTSEC-2026-0244). What the guard
    // immediately above has established, exactly:
    //
    //  * the startup window is still open — `seal_locale` has not run;
    //  * this is the thread that made every earlier call;
    //  * on Linux, and only there, `/proc/self/task` reported a single live
    //    thread, so no other thread exists to race. Elsewhere — macOS — that
    //    count is unavailable and the absence of a second thread is *not*
    //    verified here; it follows from the call site instead. Both callers
    //    (`rustconn::i18n::init` and `rustconn::i18n::apply_language_from_config`)
    //    run from `main()` before GTK, GIO, tokio or the tracing subscriber
    //    start, and any call that appeared later would come from one of those
    //    threads and be refused by the thread-identity check above.
    //
    // Upstream additionally asks that no POSIX signal handler be enabled yet.
    // That clause is not checked — it is not checkable, since the Rust runtime
    // installs its own handlers before `main()` (see the crate docs) — and is
    // likewise satisfied by ordering: nothing installs a handler before
    // `i18n::init()`, the first statement of `main()`.
    //
    // `locale` is copied into a NUL-terminated buffer by `gettextrs` before the
    // C call and the result is copied out of the returned pointer before this
    // returns, so no borrow outlives the call.
    let applied = unsafe { gettextrs::setlocale(category, locale) }?;

    // Locale names are ASCII in practice; lossy decoding keeps a bizarre name
    // from being reported as a failed call, which would be a different bug.
    Some(String::from_utf8_lossy(&applied).into_owned())
}

/// Closes the startup window: any later [`init_locale`] call panics.
///
/// Call this from `main()` once the startup locale is final and before starting
/// GTK, tokio or any other thread. It turns "someone added a `setlocale` call
/// to a running application" from latent memory unsoundness into an immediate,
/// obvious crash during development.
///
/// Idempotent, and safe to call from anywhere.
///
/// There is deliberately no public way to ask whether the seal is closed: the
/// guard reports a violation itself, with a message that names the fix, so no
/// caller needs to interrogate the state before calling.
pub fn seal_locale() {
    GUARD.seal();
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// The main thread of a single-threaded process is admitted, repeatedly:
    /// startup legitimately applies `LC_ALL` and then `LC_MESSAGES`.
    #[test]
    fn admits_repeated_calls_from_the_startup_thread() {
        let guard = StartupGuard::new();
        let me = std::thread::current().id();

        assert_eq!(guard.admit(me, Some(1)), Ok(()));
        assert_eq!(guard.admit(me, Some(1)), Ok(()));
        assert_eq!(guard.admit(me, Some(1)), Ok(()));
    }

    /// Contract test (M-UNSAFE): the check that makes the `unsafe` call sound.
    /// A second live thread is refused, which is precisely the situation
    /// RUSTSEC-2026-0244 describes.
    #[test]
    fn refuses_once_a_second_thread_exists() {
        let guard = StartupGuard::new();
        let me = std::thread::current().id();

        assert_eq!(guard.admit(me, Some(2)), Err(Refusal::MultiThreaded(2)));
    }

    /// A call from a thread other than the first caller is refused even if the
    /// thread count is unavailable, so a platform without `/proc` is not left
    /// unguarded.
    #[test]
    fn refuses_a_second_thread_identity_without_a_thread_count() {
        let guard = StartupGuard::new();
        let me = std::thread::current().id();
        assert_eq!(guard.admit(me, None), Ok(()));

        let other = std::thread::spawn(|| std::thread::current().id())
            .join()
            .expect("the probe thread only reads its own id");
        assert_eq!(guard.admit(other, None), Err(Refusal::ForeignThread));
        // The rejected caller must not have taken ownership of the guard.
        assert_eq!(guard.admit(me, None), Ok(()));
    }

    /// Sealing closes the window permanently, including for the thread that
    /// legitimately used it.
    #[test]
    fn refuses_everything_after_sealing() {
        let guard = StartupGuard::new();
        let me = std::thread::current().id();
        assert_eq!(guard.admit(me, Some(1)), Ok(()));

        assert!(!guard.is_sealed());
        guard.seal();
        assert!(guard.is_sealed());

        assert_eq!(guard.admit(me, Some(1)), Err(Refusal::Sealed));
        // Idempotent.
        guard.seal();
        assert_eq!(guard.admit(me, Some(1)), Err(Refusal::Sealed));
    }

    /// How many threads the count test holds alive while it samples. Three
    /// rather than one so that an implementation answering some small constant
    /// is caught as well as one answering nothing.
    #[cfg(target_os = "linux")]
    const HELD_PROBE_THREADS: usize = 3;

    /// The smallest honest answer while those probes are parked: the probes,
    /// plus the thread sampling them.
    #[cfg(target_os = "linux")]
    const MINIMUM_LIVE_THREADS_WITH_PROBES: usize = HELD_PROBE_THREADS + 1;

    /// The thread count must reflect reality on Linux, otherwise the guard
    /// above is decorative.
    ///
    /// The count is measured against threads this test creates and holds alive,
    /// not against the harness's own: with `--test-threads=1` libtest may run
    /// tests inline on the main thread, and threads spawned by other tests are
    /// joined and reaped, so "the harness is multi-threaded" is not something a
    /// test may assert. This shape passes under any `--test-threads` value.
    #[cfg(target_os = "linux")]
    #[test]
    fn thread_count_reflects_threads_this_test_holds_alive() {
        use std::sync::mpsc;

        // True whatever the harness is doing: the thread asking is itself alive.
        let baseline = live_thread_count().expect("/proc/self/task is readable on Linux");
        assert!(
            baseline >= 1,
            "the calling thread must be counted, so the count cannot be {baseline}"
        );

        let mut probes = Vec::with_capacity(HELD_PROBE_THREADS);
        let mut releases = Vec::with_capacity(HELD_PROBE_THREADS);
        for _ in 0..HELD_PROBE_THREADS {
            let (started_tx, started_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel::<()>();
            probes.push(std::thread::spawn(move || {
                // Announce, then park until released, so the thread is
                // certainly present in /proc/self/task while we sample.
                started_tx
                    .send(())
                    .expect("the sampling thread is still waiting for this");
                let _ = release_rx.recv();
            }));
            started_rx
                .recv()
                .expect("each probe thread announces itself before parking");
            releases.push(release_tx);
        }

        let with_probes = live_thread_count().expect("/proc/self/task is readable on Linux");
        // Deliberately a floor, not `with_probes > baseline`: the other tests in
        // this binary run concurrently by default, and one of their threads
        // exiting between the two samples could defeat a strict rise — the same
        // dependence on the harness this test exists to remove. The floor holds
        // unconditionally, because these threads are alive right now, so no
        // honest count can be below it.
        assert!(
            with_probes >= MINIMUM_LIVE_THREADS_WITH_PROBES,
            "{HELD_PROBE_THREADS} probe threads plus this one are alive, \
             so a working count cannot be {with_probes}"
        );

        // Dropping the senders wakes every probe out of `recv`.
        drop(releases);
        for probe in probes {
            probe.join().expect("a probe thread only parks and returns");
        }

        // Still answering once they are gone.
        let after = live_thread_count();
        assert!(
            after.is_some_and(|count| count >= 1),
            "the count must survive the probe threads exiting, got {after:?}"
        );
    }

    /// Off Linux the count is unavailable by design, not by accident: `admit`
    /// then falls back to the identity and sealing checks. Pinning it keeps the
    /// crate's "What is checked, and what is not" section honest.
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn thread_count_is_unavailable_off_linux() {
        assert_eq!(
            live_thread_count(),
            None,
            "no non-Linux count is implemented; if one is added, update the crate docs \
             and the SAFETY comment in init_locale"
        );
    }

    /// Every refusal explains itself and names the advisory, so a developer who
    /// trips the guard does not have to read this crate to understand why.
    #[test]
    fn refusals_explain_themselves() {
        for refusal in [
            Refusal::Sealed,
            Refusal::MultiThreaded(4),
            Refusal::ForeignThread,
        ] {
            let text = refusal.describe();
            assert!(text.contains("RUSTSEC-2026-0244"), "{text}");
            assert!(text.contains("setlocale"), "{text}");
        }
    }
}
