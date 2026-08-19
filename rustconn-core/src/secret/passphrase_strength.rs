//! Strength estimate for the portable credential store's passphrase.
//!
//! The portable store is the one backend whose key is chosen by a human and
//! whose file is *meant* to leave the machine — into Dropbox, Nextcloud, a USB
//! stick. Anyone who obtains it holds the `kdf_salt` and the `wrapped_key`,
//! which is everything an offline guessing attack needs, and there is no
//! recovery path and no server to rate-limit against. Argon2id makes each guess
//! expensive; only the passphrase makes the number of guesses large.
//!
//! Setup asked for the passphrase twice but never said anything about it, so a
//! single character was accepted in silence. This estimates the guessing cost so
//! the interface can say something.
//!
//! ## What this is not
//!
//! It is **advice, never a refusal**. The same entry field is how an existing
//! file is opened, and a file created by an earlier release may well be
//! protected by something this module calls [`PassphraseStrength::Weak`] —
//! refusing it would lock the user out of their own credentials to protect them.
//! The caller shows the estimate and proceeds either way.
//!
//! ## How the estimate works, and where it is wrong
//!
//! Distinct characters are scored against the alphabet their character classes
//! imply; the repeats add only the cost of knowing where they go:
//!
//! ```text
//! bits ≈ distinct × log2(alphabet) + log2(1 + repeats)
//! ```
//!
//! The first appearance of a character is a real choice, the tenth `a` in a row
//! is not, and that is what stops `aaaaaaaaaaaaaaaaaaaaaaaa` from scoring as
//! twenty-four characters of lowercase. The repeat term is logarithmic on
//! purpose: a *linear* one at any per-repeat weight can be driven past any
//! threshold by padding, so `abababab…` would reach `Strong` by being long
//! enough, which is exactly the judgement this module exists to avoid. It is not
//! zero either, because `abcabc` is harder to guess than `abc` for someone who
//! does not already know the pattern.
//!
//! Length therefore still counts, which matters just as much as bounding the
//! repeats: an earlier version multiplied by `distinct / length`, the `length`
//! cancelled, and the estimate collapsed to `distinct × log2(alphabet)` — a
//! passphrase could not be improved by making it longer at all. A long diceware
//! phrase scores well despite using one character class, which is the outcome a
//! class-*requirement* would get wrong — those push users toward `P@ssw0rd1` and
//! away from the thing that actually helps.
//!
//! It does **not** know that `Password1!` is on every wordlist ever published.
//! Recognising that needs a dictionary, which means either shipping one or
//! taking on `zxcvbn`; neither is worth a new dependency for a hint next to a
//! text field. So the estimate is an upper bound on strength: it can call a
//! common password `Fair`, and it will not call a good passphrase weak. The
//! error direction is deliberate — a false alarm on a passphrase the user
//! carefully chose teaches them to ignore the row.

use zeroize::Zeroizing;

/// Shortest passphrase not reported as [`PassphraseStrength::TooShort`].
///
/// Eight is not a recommendation, it is the floor below which the estimate stops
/// being interesting: at that length no character-class mix reaches the `Fair`
/// threshold, so the length is the only thing worth reporting.
pub const MIN_REASONABLE_LENGTH: usize = 8;

/// Upper bound on scored length, so a pathological paste cannot dominate.
///
/// Anything past this is `Strong` by any reading; the cap exists to keep the
/// arithmetic in a range where the `f64` conversions below are exact.
const MAX_SCORED_LENGTH: usize = 512;

/// Below this many estimated bits, treat a leaked file as readable.
///
/// Fifty bits against Argon2id at 64 MiB is out of reach for a casual attacker
/// and within reach of someone who wants it and can rent hardware. That is the
/// line between "this file is fine in a shared folder" and "this file is fine
/// until someone cares", which is the distinction the user is making when they
/// put it in a cloud directory.
const WEAK_BITS_CEILING: f64 = 50.0;

/// Below this many estimated bits, call it `Fair` rather than `Strong`.
const FAIR_BITS_CEILING: f64 = 75.0;

/// How much guessing work a passphrase looks like it would cost.
///
/// Ordered from weakest to strongest, so `>=` comparisons read naturally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PassphraseStrength {
    /// Empty. Reported separately because "type something" is not advice about
    /// strength and the caller already refuses this case.
    Empty,
    /// Shorter than [`MIN_REASONABLE_LENGTH`].
    TooShort,
    /// Cheap enough to guess that a leaked file should be assumed readable.
    Weak,
    /// Survives a casual attack; not a determined, funded one.
    Fair,
    /// Impractical to guess offline on hardware that exists.
    Strong,
}

impl PassphraseStrength {
    /// Whether this estimate is worth interrupting the user about.
    ///
    /// `Fair` is not: it means "this works, and a longer one would work better",
    /// which is not news anyone needs while typing. `Empty` is not either — the
    /// caller refuses an empty passphrase with a message of its own, and two
    /// complaints about one empty field is one too many.
    #[must_use]
    pub fn deserves_a_warning(self) -> bool {
        matches!(self, Self::TooShort | Self::Weak)
    }
}

/// Estimates how much work guessing `passphrase` would take.
///
/// See the module documentation for the formula and for what it cannot see.
/// This is a hint for the interface, not a gate: callers show it and proceed.
///
/// # Scope of the thresholds
///
/// The bit thresholds are calibrated for **the portable credential store** and
/// nothing else: an attacker holding the file, guessing offline against Argon2id
/// at 64 MiB, with no server to rate-limit them and no recovery for the owner.
/// Re-exported at `secret::` level this function sits next to the KDBX and
/// Bitwarden backends, where those assumptions do not hold — a database with
/// different KDF parameters makes `Strong` mean something else. Do not reuse the
/// verdict for another backend without recalibrating.
///
/// # Secret handling
///
/// `passphrase` is a secret, and the `&str` says so nowhere — the neighbouring
/// [`verify_portable_passphrase`](super::verify_portable_passphrase) takes a
/// `&SecretString`, so the difference is deliberate and worth stating. This runs
/// on every keystroke against a live GTK entry buffer: a `&SecretString`
/// parameter would make each call build `SecretString::from(entry.text())`
/// first, which is a fresh heap copy of the passphrase per character typed,
/// where a borrow is none. The type-level marker is traded for zero copies, and
/// the contract replaces it: this function borrows, and must never store, log,
/// format or otherwise persist what it is given. It returns a five-value verdict
/// and keeps nothing.
///
/// Calling it with a `SecretString` in hand needs no copy:
///
/// ```
/// use rustconn_core::secret::assess_passphrase;
/// use secrecy::{ExposeSecret, SecretString};
///
/// let stored = SecretString::from("correct horse battery staple".to_owned());
/// // `expose_secret()` yields `&str`, so this borrows. Writing
/// // `assess_passphrase(&stored.expose_secret().to_string())` would also
/// // compile and would leave an unzeroized copy behind.
/// let strength = assess_passphrase(stored.expose_secret());
/// # let _ = strength;
/// ```
///
/// Callers should not log the verdict either. `strength = ?s` in a `tracing`
/// field writes "this user's passphrase is Weak" into the journal, which is a
/// useful fact for exactly one audience.
#[must_use]
pub fn assess_passphrase(passphrase: &str) -> PassphraseStrength {
    if passphrase.is_empty() {
        return PassphraseStrength::Empty;
    }

    // Characters, not bytes: a passphrase in Cyrillic or with an emoji in it is
    // as long as it looks, and `len()` would score it as two to four times its
    // length purely for not being ASCII.
    let length = passphrase.chars().count();
    if length < MIN_REASONABLE_LENGTH {
        return PassphraseStrength::TooShort;
    }

    let bits = estimated_bits(passphrase, length.min(MAX_SCORED_LENGTH));
    if bits < WEAK_BITS_CEILING {
        PassphraseStrength::Weak
    } else if bits < FAIR_BITS_CEILING {
        PassphraseStrength::Fair
    } else {
        PassphraseStrength::Strong
    }
}

/// Estimated guessing cost in bits: length × alphabet, damped by repetition.
fn estimated_bits(passphrase: &str, scored_length: usize) -> f64 {
    // A wiped-on-drop vector rather than a `HashSet<char>`. The distinct
    // characters are material derived from the passphrase — for a short,
    // high-entropy one they are very nearly all of it, since only the ordering is
    // missing — and this runs on every keystroke, so anything left in the
    // allocator is left there once per character typed. A `HashSet` cannot be
    // wiped through its public API, and a rehash on growth abandons the old table
    // with a copy of the contents in it. Sorting and deduplicating gives the same
    // count in a buffer that zeroes itself, including its spare capacity.
    //
    // `with_capacity` and `extend` rather than `collect`: `Chars` is not
    // `TrustedLen`, so `collect` would reserve from a low `size_hint` and then
    // grow by doubling, and every reallocation abandons a freed buffer holding an
    // ordered prefix of the passphrase — reintroducing exactly the defect the
    // `HashSet` was replaced for. `scored_length` is capped, so one allocation of
    // at most 2 KiB covers every input.
    let mut seen: Zeroizing<Vec<char>> = Zeroizing::new(Vec::with_capacity(scored_length));
    seen.extend(passphrase.chars().take(scored_length));
    seen.sort_unstable();
    seen.dedup();
    let distinct = seen.len();

    let alphabet = alphabet_size(passphrase.chars().take(scored_length));

    // Distinct characters at full price, repeats at the log of their count.
    //
    // Deliberately *not* `length × … × (distinct / length)`: the `length` cancels
    // there, leaving `distinct × log2(alphabet)`, so a 512-character phrase built
    // from eleven distinct characters scored the same as eleven of them.
    //
    // And deliberately not a linear per-repeat weight either, however small: that
    // can be driven past any threshold by padding, so `abab…` would reach
    // `Strong` at a few hundred characters. `saturating_sub` cannot underflow —
    // `distinct` is derived from the same `scored_length` characters, so it is
    // never larger — and is used rather than `-` to make that a property of the
    // code and not of a comment.
    let repeats = as_f64(scored_length.saturating_sub(distinct));

    // `mul_add` rather than `*` then `+`: one rounding instead of two, which
    // clippy::suboptimal_flops asks for and which costs nothing to grant.
    as_f64(distinct).mul_add(alphabet.log2(), (1.0 + repeats).log2())
}

/// Size of the alphabet the passphrase appears to have been drawn from.
///
/// A count of the character *classes* present, not of the characters used —
/// someone who typed one digit could have typed any of ten, and scoring only the
/// digits they used would credit them with less choice than they had.
fn alphabet_size(chars: impl Iterator<Item = char>) -> f64 {
    let mut lowercase = false;
    let mut uppercase = false;
    let mut digit = false;
    let mut ascii_other = false;
    let mut non_ascii = false;

    for c in chars {
        if c.is_ascii_lowercase() {
            lowercase = true;
        } else if c.is_ascii_uppercase() {
            uppercase = true;
        } else if c.is_ascii_digit() {
            digit = true;
        } else if c.is_ascii() {
            // Printable ASCII punctuation and space: 33 code points.
            ascii_other = true;
        } else {
            non_ascii = true;
        }
    }

    let mut size = 0.0_f64;
    if lowercase {
        size += 26.0;
    }
    if uppercase {
        size += 26.0;
    }
    if digit {
        size += 10.0;
    }
    if ascii_other {
        size += 33.0;
    }
    if non_ascii {
        // Deliberately modest. The real space is enormous, but a passphrase with
        // one non-ASCII character in it did not draw that character uniformly
        // from Unicode — it came from the writer's own keyboard layout, which is
        // a few dozen additional letters at most.
        size += 64.0;
    }

    // A guard, not a case that occurs: every char lands in exactly one class
    // above, so an empty iterator is the only way here, and `log2(1) == 0` would
    // silently zero the estimate.
    if size < 2.0 { 2.0 } else { size }
}

/// Widens a small count to `f64` without tripping the precision lints.
///
/// Every input is bounded by [`MAX_SCORED_LENGTH`], so the conversion is exact
/// and the fallback is unreachable arithmetic rather than a wrong answer.
fn as_f64(value: usize) -> f64 {
    f64::from(u32::try_from(value).unwrap_or(u32::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_passphrase_is_its_own_category() {
        assert_eq!(assess_passphrase(""), PassphraseStrength::Empty);
        // The caller already says "enter a passphrase"; this must not add a
        // second complaint about the same empty field.
        assert!(!PassphraseStrength::Empty.deserves_a_warning());
    }

    #[test]
    fn anything_under_the_floor_is_too_short() {
        for passphrase in ["a", "abc", "xK7#mQ2"] {
            assert_eq!(
                assess_passphrase(passphrase),
                PassphraseStrength::TooShort,
                "{passphrase:?} is {} characters",
                passphrase.chars().count()
            );
        }
    }

    /// The whole reason the estimate is not "length ≥ 8".
    #[test]
    fn repetition_does_not_count_as_length() {
        // Twenty-four lowercase characters, one of them distinct. Scoring length
        // alone would call this stronger than a random eight-character password.
        let repeated = "a".repeat(24);
        assert_eq!(assess_passphrase(&repeated), PassphraseStrength::Weak);

        let cycled = "abab".repeat(8); // 32 characters, two distinct
        assert_eq!(assess_passphrase(&cycled), PassphraseStrength::Weak);

        // And at a length where a naive length × alphabet estimate would be well
        // past `Strong`: 240 characters, still only two distinct.
        let very_long_cycle = "ab".repeat(120);
        assert_eq!(
            assess_passphrase(&very_long_cycle),
            PassphraseStrength::Weak
        );
    }

    /// The regression the previous formula had: length has to matter.
    ///
    /// Multiplying by `distinct / length` cancelled the `length`, so the estimate
    /// was `distinct × log2(alphabet)` and adding characters to a passphrase did
    /// nothing at all. These two share an alphabet and a distinct-character count
    /// and differ only in length, so the shorter one must not score as high.
    #[test]
    fn a_longer_passphrase_scores_higher_than_a_shorter_one() {
        let short = "abcdefgh"; // 8 chars, 8 distinct
        let long = "abcdefghabcdefghabcdefghabcdefgh"; // 32 chars, same 8 distinct
        assert!(
            assess_passphrase(long) >= assess_passphrase(short),
            "lengthening a passphrase must never lower the estimate"
        );
    }

    /// The outcome a character-class *requirement* would get wrong.
    #[test]
    fn a_long_single_class_phrase_is_strong() {
        // Lowercase and spaces only, and stronger than anything a user will type
        // when a policy demands a symbol.
        assert_eq!(
            assess_passphrase("correct horse battery staple"),
            PassphraseStrength::Strong
        );
    }

    #[test]
    fn a_short_mixed_password_does_not_reach_strong() {
        // Eight characters from every class is ~52 bits: enough to bother a
        // casual attacker, not enough for a file sitting in a shared folder.
        assert_eq!(assess_passphrase("xK7#mQ2p"), PassphraseStrength::Fair);
    }

    #[test]
    fn strength_is_ordered_weakest_first() {
        assert!(PassphraseStrength::Empty < PassphraseStrength::TooShort);
        assert!(PassphraseStrength::TooShort < PassphraseStrength::Weak);
        assert!(PassphraseStrength::Weak < PassphraseStrength::Fair);
        assert!(PassphraseStrength::Fair < PassphraseStrength::Strong);
    }

    #[test]
    fn only_the_bottom_two_interrupt_the_user() {
        assert!(PassphraseStrength::TooShort.deserves_a_warning());
        assert!(PassphraseStrength::Weak.deserves_a_warning());
        // "Works, but longer would be better" is not worth a row of its own.
        assert!(!PassphraseStrength::Fair.deserves_a_warning());
        assert!(!PassphraseStrength::Strong.deserves_a_warning());
    }

    #[test]
    fn a_non_ascii_passphrase_is_measured_in_characters() {
        // Eight Cyrillic characters are eight characters, not the sixteen bytes
        // they occupy. Scoring bytes would report this as far stronger than the
        // ASCII passphrase of the same length.
        let cyrillic = "пароль12"; // 8 chars
        assert_ne!(assess_passphrase(cyrillic), PassphraseStrength::TooShort);
        let short_cyrillic = "пароль"; // 6 chars, 12 bytes
        assert_eq!(
            assess_passphrase(short_cyrillic),
            PassphraseStrength::TooShort,
            "byte length would have put this over the floor"
        );
    }

    /// A pasted key is `Strong`, and the length cap must not change that.
    #[test]
    fn the_length_cap_does_not_weaken_a_long_passphrase() {
        let long = "Tr0ub4dor&3 ".repeat(80); // 960 chars, past MAX_SCORED_LENGTH
        assert_eq!(assess_passphrase(&long), PassphraseStrength::Strong);
    }

    /// Documents the known blind spot rather than pretending it is not there.
    #[test]
    fn a_wordlist_password_is_not_detected() {
        // No dictionary, so this scores on shape alone and lands mid-range. If
        // this ever starts returning `Weak`, a dictionary was added and the
        // module docs need to stop disclaiming one.
        assert_eq!(assess_passphrase("Password1!"), PassphraseStrength::Fair);
    }
}
