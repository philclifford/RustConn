//! Pure session-placement decisions (GUI-free).
//!
//! A live session's widget can live in a tab, inside another tab's split
//! layout, or in its own detached window. [`detach_verdict`] is the single
//! shared predicate that decides whether a session may move into a detached
//! window, so the tab context menu, the keyboard action, and sidebar routing
//! all agree on the same answer for the same session state.
//!
//! This module is deliberately free of GTK/libadwaita/VTE so the logic stays
//! testable and the `rustconn-core` crate boundary holds (see project rules).

/// Facts about a live session that decide whether it can be detached.
///
/// Every field describes the session's **current** placement, not the stored
/// connection, so the verdict follows the session as it moves around.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent observed fact about the live session, not a state machine; the verdict enum is the state machine"
)]
pub struct DetachContext {
    /// The session renders inside `RustConn` (a VTE terminal or an embedded
    /// viewer). `false` marks an external-viewer placeholder tab (SPICE,
    /// RDP/VNC in external mode), whose display already is its own window.
    pub renders_in_process: bool,
    /// The session's tab hosts a split layout.
    pub is_split_owner: bool,
    /// The session's widget currently lives in another tab's split layout.
    pub is_split_guest: bool,
    /// The session already has a detached window.
    pub is_detached: bool,
}

/// Why a session may or may not be moved into a detached window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DetachVerdict {
    /// The session can be detached.
    Allowed,
    /// The session already lives in a detached window.
    AlreadyDetached,
    /// The session's display is delegated to an external viewer process.
    ExternalViewer,
    /// The session's tab hosts a split layout, which must be removed first.
    SplitOwner,
    /// The session's widget lives in another tab's split layout.
    SplitGuest,
}

impl DetachVerdict {
    /// Reports whether the verdict permits a detach operation.
    #[must_use]
    pub const fn is_allowed(self) -> bool {
        matches!(self, Self::Allowed)
    }

    /// Returns the stable key the GUI maps to a translated explanation.
    ///
    /// The key is non-empty and distinct for every variant, including
    /// [`Self::Allowed`], so a caller can log or test it uniformly.
    #[must_use]
    pub const fn reason_key(self) -> &'static str {
        match self {
            Self::Allowed => "detach-allowed",
            Self::AlreadyDetached => "detach-already-detached",
            Self::ExternalViewer => "detach-external-viewer",
            Self::SplitOwner => "detach-split-owner",
            Self::SplitGuest => "detach-split-guest",
        }
    }
}

/// Decides whether the session described by `ctx` may be moved into a split.
///
/// The inverse question of [`detach_verdict`], and the one the split "Select
/// Tab" picker asks: a session whose widget lives in a detached window must not
/// be pulled into another tab's split layout, because that would empty the
/// window while the session stayed marked as detached. A split guest already
/// sits inside a split layout, and a session rendered by an external viewer has
/// no widget to place, so neither may be placed either.
///
/// A split owner is deliberately allowed: its own tab hosts the layout, which is
/// where a placed session ends up anyway.
#[must_use]
pub const fn may_place_in_split(ctx: &DetachContext) -> bool {
    ctx.renders_in_process && !ctx.is_detached && !ctx.is_split_guest
}

/// Decides whether the session described by `ctx` may be detached.
///
/// The decision is pure: the same context always yields the same verdict.
/// When several blocking conditions hold at once, the most specific one wins,
/// in the order [`DetachVerdict::AlreadyDetached`],
/// [`DetachVerdict::ExternalViewer`], [`DetachVerdict::SplitOwner`],
/// [`DetachVerdict::SplitGuest`], so the user is told about the state that
/// actually needs their attention.
///
/// The Welcome tab has no session, so it never reaches this predicate; callers
/// treat "no session for this page" as not detachable.
#[must_use]
pub const fn detach_verdict(ctx: &DetachContext) -> DetachVerdict {
    if ctx.is_detached {
        DetachVerdict::AlreadyDetached
    } else if !ctx.renders_in_process {
        DetachVerdict::ExternalViewer
    } else if ctx.is_split_owner {
        DetachVerdict::SplitOwner
    } else if ctx.is_split_guest {
        DetachVerdict::SplitGuest
    } else {
        DetachVerdict::Allowed
    }
}
