//! Remembers which connections the vault had no password for.
//!
//! Issue [#307](https://github.com/totoshko88/RustConn/issues/307): a user whose
//! connections all authenticate by key, with no stored passwords, paid a full
//! vault round trip before *every* connection — several seconds of it against
//! the Bitwarden CLI, which decrypts on each read — to be told each time exactly
//! what the previous attempt had already established. Caching the negative
//! answer turns that into once per connection per five minutes.
//!
//! # Why this is not a field on `AppState`
//!
//! `AppState` is where the session's *positive* password cache lives, so that
//! would be the obvious home. It is the wrong one here, because the thing that
//! has to invalidate this cache is [`crate::vault_ops::save_password_to_vault`],
//! a free function that takes `&AppSettings` and deliberately not the
//! application state. Reaching `AppState` from there would mean threading it
//! through seven call sites, and the invalidation would then be seven things a
//! future call site can forget — the same shape as the `script` probe that was
//! applied to one launch path and not the other (#306), and as the POT gate that
//! kept one copy of a keyword list. Keeping the store here lets the save path
//! itself be the choke point, so a new caller cannot get it wrong.
//!
//! # What it holds
//!
//! No secret. A connection id, the backend that was asked, and when. The whole
//! record is the *absence* of a credential.
//!
//! # Why a global lock and not a `thread_local`
//!
//! The first version was `thread_local!`, on the reasoning that every accessor
//! sits on the GTK main thread. That reasoning was wrong, and wrong in the
//! quiet direction: `migrate_vault_credential_for_edit` is called from inside
//! the *operation* closure of [`crate::utils::spawn_blocking_with_callback`] at
//! all five of its call sites, which is a plain `std::thread::spawn`. A
//! `forget` there allocated a fresh empty map on the worker, removed nothing,
//! and dropped it, while the stale record on the main thread survived — so a
//! rename or a group move would leave the connect path skipping a lookup for a
//! credential that had just been written. Bounded by the TTL and never a wrong
//! credential, but a bug, and one that no call site could see.
//!
//! Chasing thread affinity across five call sites would put the invariant back
//! in the same shape that produced the bug. A process-wide lock removes the
//! question instead: the record holds no secret, the critical sections are a
//! single map operation, and the map is touched a handful of times per
//! connection, so there is nothing here worth optimising by thread-locality.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use rustconn_core::config::SecretBackendType;
use uuid::Uuid;

/// How long a "the vault had no entry" answer is trusted, in seconds.
///
/// Five minutes. Deliberately not borrowed from the credential cache's TTL: that
/// one bounds how long a *secret* sits in memory, and this record holds none.
/// What it bounds is staleness from changes the invalidation hooks cannot see — a
/// password added in the Bitwarden web vault, or with `secret-tool`, or by
/// another RustConn instance. Without an expiry the only way to pick those up
/// would be restarting the application.
///
/// It also covers the inputs the record does *not* key on. A miss is filed
/// against the preferred backend, while whether the lookup finds anything also
/// depends on `enable_fallback` and on the backend being reachable at all. So
/// toggling fallback, or an unavailable backend coming back up, is not an
/// invalidation hook and the old answer stands until it expires. Left that way on
/// purpose rather than keyed on the whole of the secret settings: the cost is one
/// repeated lookup, and a key that wide would be invalidated by settings changes
/// that have nothing to do with this connection.
const TTL_SECONDS: i64 = 300;

/// A remembered "no password here" answer.
struct Miss {
    /// Backend that was asked. Recorded so switching backends does not consult
    /// an answer that was true of a different vault, which also means changing
    /// the preferred backend needs no explicit invalidation.
    backend: SecretBackendType,
    /// When the lookup came back empty.
    recorded_at: chrono::DateTime<chrono::Utc>,
}

impl Miss {
    /// Whether this answer is too old to trust.
    fn is_expired(&self) -> bool {
        let elapsed = chrono::Utc::now() - self.recorded_at;
        elapsed.num_seconds().max(0) > TTL_SECONDS
    }
}

static MISSES: LazyLock<Mutex<HashMap<Uuid, Miss>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Records that `backend` held no password for `connection_id`.
///
/// A poisoned lock is dropped on the floor rather than propagated: failing to
/// record a miss costs one repeated lookup, which is the same thing that happens
/// without this cache at all.
pub fn note_missing(connection_id: Uuid, backend: SecretBackendType) {
    if let Ok(mut misses) = MISSES.lock() {
        misses.insert(
            connection_id,
            Miss {
                backend,
                recorded_at: chrono::Utc::now(),
            },
        );
    }
}

/// Whether `backend` is already known to hold no password for `connection_id`.
///
/// `false` whenever there is any doubt — no record, an expired one, one taken
/// against a different backend, or a poisoned lock. A wrong `true` costs a
/// connection the password it would have found, so every uncertainty resolves
/// towards doing the work again.
#[must_use]
pub fn known_missing(connection_id: Uuid, backend: SecretBackendType) -> bool {
    MISSES.lock().is_ok_and(|misses| {
        misses
            .get(&connection_id)
            .is_some_and(|miss| miss.backend == backend && !miss.is_expired())
    })
}

/// Forgets the answer for one connection.
///
/// Called from the vault save and key-migration paths, so a password that has
/// just been written is visible to the next connect rather than after the TTL.
/// Correct after an edit or group move too, since the lookup key is derived from
/// the connection's name, host, protocol and group.
///
/// On a poisoned lock the stale record is *kept*, which in isolation is the
/// wrong direction. It is harmless only because [`known_missing`] also answers
/// `false` on a poisoned lock: the first panic under this mutex disables the
/// cache entirely, so there is nothing left for a stale record to suppress.
pub fn forget(connection_id: Uuid) {
    if let Ok(mut misses) = MISSES.lock() {
        misses.remove(&connection_id);
    }
}

/// Forgets every answer.
///
/// For changes too broad to attribute to one connection: a bulk password
/// transfer between backends, or a vault that has just been unlocked.
pub fn forget_all() {
    if let Ok(mut misses) = MISSES.lock() {
        misses.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh id per test.
    ///
    /// The map is process-wide now, so tests must not share keys and must not
    /// call `forget_all` — libtest runs them concurrently and a global clear
    /// would race whatever else is mid-assertion. Distinct uuids give the
    /// isolation that a per-thread map used to give for free.
    ///
    /// The cost is that [`forget_all`] has no test: it cannot be exercised
    /// without clearing another test's record, and its only caller is GUI code.
    /// Said plainly rather than left to be assumed, since the neighbouring
    /// functions are covered.
    fn id() -> Uuid {
        Uuid::new_v4()
    }

    /// Inserts a record with a chosen age, for the expiry tests.
    fn note_missing_at(
        connection_id: Uuid,
        backend: SecretBackendType,
        age_seconds: i64,
    ) -> Result<(), ()> {
        let mut misses = MISSES.lock().map_err(|_| ())?;
        misses.insert(
            connection_id,
            Miss {
                backend,
                recorded_at: chrono::Utc::now() - chrono::Duration::seconds(age_seconds),
            },
        );
        Ok(())
    }

    #[test]
    fn an_unrecorded_connection_is_not_known_missing() {
        assert!(!known_missing(id(), SecretBackendType::LibSecret));
    }

    #[test]
    fn a_recorded_connection_is_known_missing_for_that_backend() {
        let c = id();
        note_missing(c, SecretBackendType::LibSecret);
        assert!(known_missing(c, SecretBackendType::LibSecret));
    }

    #[test]
    fn a_different_backend_does_not_reuse_the_answer() {
        // The whole reason the backend is stored: an answer about libsecret says
        // nothing about what Bitwarden holds.
        let c = id();
        note_missing(c, SecretBackendType::LibSecret);
        assert!(!known_missing(c, SecretBackendType::Bitwarden));
    }

    #[test]
    fn forget_makes_the_connection_unknown_again() {
        // This is what saving a password relies on.
        let c = id();
        note_missing(c, SecretBackendType::LibSecret);
        forget(c);
        assert!(!known_missing(c, SecretBackendType::LibSecret));
    }

    #[test]
    fn forget_leaves_other_connections_alone() {
        let (a, b) = (id(), id());
        note_missing(a, SecretBackendType::LibSecret);
        note_missing(b, SecretBackendType::LibSecret);
        forget(a);
        assert!(!known_missing(a, SecretBackendType::LibSecret));
        assert!(known_missing(b, SecretBackendType::LibSecret));
    }

    #[test]
    fn an_expired_answer_is_not_trusted() {
        let c = id();
        assert!(note_missing_at(c, SecretBackendType::LibSecret, TTL_SECONDS + 1).is_ok());
        assert!(!known_missing(c, SecretBackendType::LibSecret));
    }

    #[test]
    fn an_answer_just_inside_the_ttl_is_still_trusted() {
        let c = id();
        assert!(note_missing_at(c, SecretBackendType::LibSecret, TTL_SECONDS - 1).is_ok());
        assert!(known_missing(c, SecretBackendType::LibSecret));
    }

    #[test]
    fn a_clock_that_went_backwards_does_not_expire_the_answer() {
        // `num_seconds().max(0)` guards this; without it a negative elapsed
        // could not exceed the TTL either, but the intent is worth pinning.
        let c = id();
        assert!(note_missing_at(c, SecretBackendType::LibSecret, -60).is_ok());
        assert!(known_missing(c, SecretBackendType::LibSecret));
    }

    #[test]
    fn a_record_is_reachable_from_a_worker_thread() {
        // The reason this is a global lock rather than a thread_local: the
        // key-migration path calls `forget` from inside a spawned worker, and a
        // per-thread map made that a silent no-op.
        let c = id();
        note_missing(c, SecretBackendType::LibSecret);
        let handle = std::thread::spawn(move || {
            let seen = known_missing(c, SecretBackendType::LibSecret);
            forget(c);
            seen
        });
        assert!(
            handle.join().expect("the worker must not panic"),
            "the worker must see the record"
        );
        assert!(
            !known_missing(c, SecretBackendType::LibSecret),
            "a forget on a worker must be visible here"
        );
    }
}
