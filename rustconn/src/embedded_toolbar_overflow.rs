//! Adaptive toolbar overflow for embedded protocol viewers.
//!
//! Embedded RDP/VNC/SPICE widgets build a horizontal `embedded-toolbar`
//! [`gtk4::Box`]; the embedded Web viewer builds an equivalent navigation
//! toolbar. In a narrow split panel — or a small/narrow application window —
//! that toolbar clips. [`ToolbarOverflow`] watches the viewer's width and, once
//! the toolbar no longer fits, folds the *secondary* actions into a "⋯" overflow
//! [`gtk4::MenuButton`] popover while the *primary* actions (Fit resolution,
//! Ctrl+Alt+Del; Back/Forward/Reload and the URL bar for Web) stay directly
//! reachable.
//!
//! "No longer fits" is *measured*, not guessed. Each viewer used to pass a
//! hand-tuned pixel breakpoint, and all three were stale: `ToolbarAutoHide`
//! applies the GNOME HIG 44×44 minimum to every toolbar button, which the
//! eyeballed numbers predated, so the Web toolbar in particular expanded at a
//! width it still could not draw at. The controller now asks the toolbar what it
//! needs ([`gtk4::prelude::WidgetExt::measure`]) and compares that with what it
//! has, so adding or removing a button — or a theme with different button
//! metrics — needs no constant retuned anywhere.
//!
//! The existing button widgets are **reparented** between the toolbar and the
//! popover — never rebuilt — so every signal handler stays bound and every
//! action remains reachable at any width (R12.4). This behaviour is a property
//! of the widget itself, so it works identically in a split panel and in a
//! shrunk single-tab window.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::{
    Box as GtkBox, Button, DrawingArea, EventControllerFocus, EventControllerMotion, MenuButton,
    Orientation, Overlay, Popover, Revealer, Widget, glib,
};

use crate::i18n::i18n;

/// Width margin above the fitting width required before expanding again.
///
/// Two thresholds — collapse below the toolbar's natural width, expand at/above
/// natural + this margin — stop the overflow button flapping when a resize drag
/// settles right on the breakpoint.
const OVERFLOW_HYSTERESIS_PX: i32 = 48;

/// Adaptive overflow controller for an `embedded-toolbar` box.
///
/// Construct with [`ToolbarOverflow::new`] once the toolbar is fully assembled,
/// then wire the width watch with [`ToolbarOverflow::attach`]. The returned
/// `Rc` does not need to be stored: [`attach`](Self::attach) moves a clone into
/// the resize closure, which lives as long as the monitored drawing area.
pub struct ToolbarOverflow {
    /// The "⋯" button appended to the toolbar; hidden while everything fits.
    overflow_button: MenuButton,
    /// Vertical box inside the overflow popover holding the collapsed actions.
    overflow_box: GtkBox,
    /// The toolbar the secondary actions live in when expanded.
    toolbar: GtkBox,
    /// Secondary actions paired with the sibling they sit *after* when expanded
    /// (captured from the assembled toolbar so [`expand`](Self::expand) restores
    /// the original order). `None` means "first child".
    secondary: Vec<(Widget, Option<Widget>)>,
    /// Whether the secondary actions currently live in the overflow popover.
    collapsed: Cell<bool>,
}

impl ToolbarOverflow {
    /// Appends a hidden overflow button to `toolbar` and records the secondary actions.
    ///
    /// `secondary` is the ordered list of actions to fold into the popover once
    /// the toolbar stops fitting; pass them in their toolbar order. Primary
    /// actions are simply left out of `secondary`. An empty `secondary` list
    /// makes the controller a no-op (the overflow button never appears).
    #[must_use]
    pub fn new(toolbar: &GtkBox, secondary: Vec<Widget>) -> Rc<Self> {
        let overflow_box = GtkBox::new(Orientation::Vertical, 4);
        overflow_box.set_margin_start(6);
        overflow_box.set_margin_end(6);
        overflow_box.set_margin_top(6);
        overflow_box.set_margin_bottom(6);

        let popover = Popover::new();
        popover.set_child(Some(&overflow_box));

        let overflow_button = MenuButton::new();
        overflow_button.set_icon_name("view-more-symbolic");
        overflow_button.add_css_class("flat");
        overflow_button.set_tooltip_text(Some(&i18n("More actions")));
        overflow_button
            .update_property(&[gtk4::accessible::Property::Label(&i18n("More actions"))]);
        overflow_button.set_popover(Some(&popover));
        overflow_button.set_visible(false);
        toolbar.append(&overflow_button);

        // Capture each secondary widget's anchor (its preceding sibling) from the
        // fully-assembled toolbar. Because expand() processes the list in order,
        // an anchor that is itself a secondary widget is already back in place by
        // the time it is needed, so the original layout is restored exactly.
        let secondary = secondary
            .into_iter()
            .map(|w| {
                let anchor = w.prev_sibling();
                (w, anchor)
            })
            .collect();

        Rc::new(Self {
            overflow_button,
            overflow_box,
            toolbar: toolbar.clone(),
            secondary,
            collapsed: Cell::new(false),
        })
    }

    /// Wires the width watch to `resize_source` (the viewer's drawing area).
    ///
    /// The drawing area fills the panel/window, so its width is a reliable proxy
    /// for the available toolbar width. A clone of `self` is moved into the
    /// resize closure, keeping the controller alive for the widget's lifetime.
    pub fn attach(self: &Rc<Self>, resize_source: &DrawingArea) {
        let this = Rc::clone(self);
        resize_source.connect_resize(move |_, width, _| {
            this.update(width);
        });
    }

    /// Wires the width watch to any widget, for viewers with no drawing area.
    ///
    /// `GtkDrawingArea::resize` is the cheap way to learn about a width change,
    /// but GTK4 exposes no equivalent signal on a plain widget — there is no
    /// `size-allocate` to connect to and no `width` property to watch. So this
    /// reads the allocated width from a tick callback and calls
    /// [`update`](Self::update) only when the number actually changed, which
    /// makes the per-frame cost an integer comparison and leaves the reparenting
    /// work driven by real resizes.
    ///
    /// Used by the embedded Web viewer, whose content is a `WebView`.
    pub fn attach_to_widget(self: &Rc<Self>, resize_source: &impl IsA<Widget>) {
        let this = Rc::clone(self);
        let last_width = Cell::new(-1_i32);
        resize_source.as_ref().add_tick_callback(move |widget, _| {
            let width = widget.width();
            // Width is 0 until the widget is first allocated; feeding that
            // to update() would collapse the toolbar before it is on screen.
            if width > 0 && width != last_width.get() {
                last_width.set(width);
                this.update(width);
            }
            glib::ControlFlow::Continue
        });
    }

    /// Collapses or expands the secondary actions for the viewer's current `width`.
    fn update(&self, width: i32) {
        if self.secondary.is_empty() {
            return;
        }

        // `width` is the viewer's width, which the toolbar spans; its own
        // margins are inside that number and unavailable to the children.
        let available_px = width - self.toolbar.margin_start() - self.toolbar.margin_end();
        let needed_px = self.expanded_natural_px();

        if self.collapsed.get() {
            if available_px >= needed_px + OVERFLOW_HYSTERESIS_PX {
                self.expand();
            }
        } else if available_px < needed_px {
            self.collapse();
        }
    }

    /// Width the toolbar needs with every secondary action back in place.
    ///
    /// Answers the same number in both states, which is what lets [`update`] use
    /// one comparison for collapsing and expanding. While expanded that is just
    /// the toolbar's natural width — the overflow button is hidden then, and GTK
    /// does not measure a hidden child. While collapsed the secondary actions are
    /// parked in the popover and the "⋯" button stands in for them, so their
    /// widths are added back and its width is taken off.
    ///
    /// Recomputed on every resize rather than cached on first sight: a
    /// measurement taken before the theme's button metrics are resolved would
    /// otherwise be believed forever, and a wrong cached value in the collapsed
    /// state is unrecoverable — the toolbar would never expand again.
    fn expanded_natural_px(&self) -> i32 {
        let (_, mut natural, _, _) = self.toolbar.measure(Orientation::Horizontal, -1);

        if self.collapsed.get() {
            let spacing = self.toolbar.spacing();
            let (_, overflow_natural, _, _) =
                self.overflow_button.measure(Orientation::Horizontal, -1);
            natural -= overflow_natural + spacing;
            for (widget, _) in &self.secondary {
                let (_, widget_natural, _, _) = widget.measure(Orientation::Horizontal, -1);
                natural += widget_natural + spacing;
            }
        }

        natural
    }

    /// Moves the secondary actions into the popover and reveals the overflow button.
    fn collapse(&self) {
        for (widget, _) in &self.secondary {
            self.toolbar.remove(widget);
            self.overflow_box.append(widget);
        }
        self.overflow_button.set_visible(true);
        self.collapsed.set(true);
    }

    /// Moves the secondary actions back into the toolbar and hides the overflow button.
    fn expand(&self) {
        for (widget, anchor) in &self.secondary {
            self.overflow_box.remove(widget);
            self.toolbar.insert_child_after(widget, anchor.as_ref());
        }
        self.overflow_button.set_visible(false);
        self.collapsed.set(false);
    }
}

/// Delay before an inactive floating toolbar is hidden.
const TOOLBAR_HIDE_DELAY: std::time::Duration = std::time::Duration::from_secs(2);

/// GNOME HIG minimum pointer/touch target, in pixels.
const TOUCH_TARGET_PX: i32 = 44;

/// Where the reveal handle sits along the top edge of the viewer.
///
/// The handle is the one piece of the auto-hide toolbar that stays on screen
/// while the toolbar is away, so it always covers a little of what is underneath.
/// Which part of the edge hurts least depends on what that content is, which is
/// why this is the caller's decision rather than a constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevealHandle {
    /// Top centre — RDP, VNC and SPICE.
    ///
    /// The surface underneath is a remote desktop, whose own top edge is a menu
    /// bar or panel the user is not aiming at mid-session, and the centre keeps
    /// the handle clear of the window controls and split-view buttons that live
    /// in the corners.
    TopCentre,
    /// Top trailing corner — the embedded Web viewer.
    ///
    /// A web page puts its logo, primary navigation and search across the top
    /// centre, which is exactly where a 44×44 button would swallow clicks meant
    /// for the page. The trailing corner is also where the toolbar's own menu
    /// button appears once revealed, so the handle sits where its controls are
    /// about to be. Trailing rather than right: GTK flips `Align::End` in an RTL
    /// locale, and so does the page.
    TopTrailing,
}

impl RevealHandle {
    /// Returns the `halign` that places the handle.
    const fn halign(self) -> gtk4::Align {
        match self {
            Self::TopCentre => gtk4::Align::Center,
            Self::TopTrailing => gtk4::Align::End,
        }
    }
}

/// Controls reveal and focus-safe auto-hide for a floating embedded toolbar.
///
/// The controller uses a narrow arrow indicator at the top center of the view as
/// the sole trigger for revealing the toolbar. This avoids blocking interaction
/// with window controls or other elements at the top edge. The arrow can be
/// focused with Tab or activated by touch/click. The toolbar is never hidden
/// while its controls have focus, the pointer is over it, or one of its menus
/// is open.
pub struct ToolbarAutoHide {
    revealer: Revealer,
    toolbar: GtkBox,
    reveal_button: Button,
    hide_timer: RefCell<Option<glib::SourceId>>,
    /// Whether this connection wants a floating toolbar at all.
    ///
    /// Two gates rather than one because they answer different questions and
    /// change on different schedules. `available` tracks the *session*: the
    /// viewers switch it off when disconnected and back on for every
    /// connecting/connected transition, which is why it cannot carry a user
    /// preference — [`show_briefly`](Self::show_briefly) sets it to `true` and
    /// every state change goes through there. `enabled` is the per-connection
    /// choice from `hide_floating_toolbar`, set once before the session starts
    /// and never flipped by a state change (issue #260).
    enabled: Cell<bool>,
    available: Cell<bool>,
    pointer_in_reveal_button: Cell<bool>,
    pointer_in_toolbar: Cell<bool>,
    focus_in_toolbar: Cell<bool>,
}

impl ToolbarAutoHide {
    /// Attaches auto-hide behavior and a touch/keyboard reveal control to an overlay.
    ///
    /// The reveal trigger is a small arrow indicator along the top edge —
    /// hovering or clicking it reveals the full toolbar. `handle` decides which
    /// part of that edge it sits on; see [`RevealHandle`] for why the viewers do
    /// not agree.
    #[must_use]
    pub fn attach(
        overlay: &Overlay,
        toolbar: &GtkBox,
        revealer: &Revealer,
        handle: RevealHandle,
    ) -> Rc<Self> {
        ensure_touch_targets(toolbar.upcast_ref());

        let reveal_button = Button::from_icon_name("pan-down-symbolic");
        reveal_button.add_css_class("flat");
        reveal_button.add_css_class("circular");
        reveal_button.add_css_class("toolbar-reveal-handle");
        // The GNOME HIG minimum tap target, stated here rather than in CSS so
        // there is one number to find. The stylesheet only paints the handle.
        reveal_button.set_size_request(TOUCH_TARGET_PX, TOUCH_TARGET_PX);
        reveal_button.set_halign(handle.halign());
        reveal_button.set_valign(gtk4::Align::Start);
        reveal_button.set_tooltip_text(Some(&i18n("Show session toolbar")));
        reveal_button.update_property(&[gtk4::accessible::Property::Label(&i18n(
            "Show session toolbar",
        ))]);
        reveal_button.set_visible(false);
        overlay.add_overlay(&reveal_button);

        let controller = Rc::new(Self {
            revealer: revealer.clone(),
            toolbar: toolbar.clone(),
            reveal_button: reveal_button.clone(),
            hide_timer: RefCell::new(None),
            enabled: Cell::new(true),
            available: Cell::new(false),
            pointer_in_reveal_button: Cell::new(false),
            pointer_in_toolbar: Cell::new(false),
            focus_in_toolbar: Cell::new(false),
        });

        // Click on the arrow reveals toolbar and moves focus into it.
        let controller_weak = Rc::downgrade(&controller);
        let toolbar_for_focus = toolbar.clone();
        reveal_button.connect_clicked(move |_| {
            if let Some(controller) = controller_weak.upgrade() {
                controller.show();
                toolbar_for_focus.child_focus(gtk4::DirectionType::TabForward);
            }
        });

        // Hovering over the arrow reveals the toolbar without a click.
        let reveal_motion = EventControllerMotion::new();
        let controller_weak = Rc::downgrade(&controller);
        reveal_motion.connect_enter(move |_, _, _| {
            if let Some(controller) = controller_weak.upgrade() {
                controller.pointer_in_reveal_button.set(true);
                controller.show();
            }
        });
        let controller_weak = Rc::downgrade(&controller);
        reveal_motion.connect_leave(move |_| {
            if let Some(controller) = controller_weak.upgrade() {
                controller.pointer_in_reveal_button.set(false);
                controller.schedule_hide();
            }
        });
        reveal_button.add_controller(reveal_motion);

        let controller_weak = Rc::downgrade(&controller);
        revealer.connect_notify_local(Some("reveal-child"), move |_, _| {
            if let Some(controller) = controller_weak.upgrade() {
                controller.update_reveal_button();
            }
        });

        // Pointer over the revealed toolbar keeps it open.
        let toolbar_motion = EventControllerMotion::new();
        let controller_weak = Rc::downgrade(&controller);
        toolbar_motion.connect_enter(move |_, _, _| {
            if let Some(controller) = controller_weak.upgrade() {
                controller.pointer_in_toolbar.set(true);
                controller.cancel_hide();
            }
        });
        let controller_weak = Rc::downgrade(&controller);
        toolbar_motion.connect_leave(move |_| {
            if let Some(controller) = controller_weak.upgrade() {
                controller.pointer_in_toolbar.set(false);
                controller.schedule_hide();
            }
        });
        toolbar.add_controller(toolbar_motion);

        // Keyboard focus in toolbar keeps it open.
        let focus = EventControllerFocus::new();
        let controller_weak = Rc::downgrade(&controller);
        focus.connect_enter(move |_| {
            if let Some(controller) = controller_weak.upgrade() {
                controller.focus_in_toolbar.set(true);
                controller.show();
            }
        });
        let controller_weak = Rc::downgrade(&controller);
        focus.connect_leave(move |_| {
            if let Some(controller) = controller_weak.upgrade() {
                controller.focus_in_toolbar.set(false);
                controller.schedule_hide();
            }
        });
        toolbar.add_controller(focus);

        controller
    }

    /// Removes the toolbar and its reveal handle for the rest of the session.
    ///
    /// Called once, before the session connects, with the connection's
    /// `hide_floating_toolbar` inverted. Passing `false` is permanent in
    /// practice: nothing switches it back, because the setting cannot change
    /// while the session is open.
    ///
    /// Everything the user could touch has to go, not just the toolbar: the
    /// handle is hidden so there is no hot zone left to hit by accident, and the
    /// revealer stops being targetable so it cannot swallow a click meant for
    /// the remote desktop or the page beneath it. Both follow from
    /// [`update_reveal_button`](Self::update_reveal_button), which is why they
    /// are not repeated here.
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.set(enabled);
        if !enabled {
            self.available.set(false);
            self.cancel_hide();
            self.revealer.set_reveal_child(false);
            // A pointer or focus flag left set would keep `interaction_active`
            // true and hold a rescheduling hide timer alive for nothing.
            self.pointer_in_reveal_button.set(false);
            self.pointer_in_toolbar.set(false);
            self.focus_in_toolbar.set(false);
        }
        self.update_reveal_button();
    }

    /// Reveals the toolbar and hides it after the standard inactivity delay.
    pub fn show_briefly(self: &Rc<Self>) {
        if !self.enabled.get() {
            return;
        }
        self.available.set(true);
        if self.reveal() {
            self.schedule_hide();
        }
    }

    /// Disables the toolbar until the next connected/connecting state.
    pub fn hide(&self) {
        self.available.set(false);
        self.cancel_hide();
        self.revealer.set_reveal_child(false);
        self.pointer_in_reveal_button.set(false);
        self.update_reveal_button();
    }

    fn show(&self) {
        self.cancel_hide();
        self.reveal();
    }

    /// The one place that reveals the toolbar, and the one place both gates are
    /// checked. Returns whether it did.
    ///
    /// Four things reveal this toolbar — a click on the handle, a hover over it,
    /// keyboard focus entering the toolbar, and a session state change — and
    /// they do not share a caller: three go through [`show`](Self::show), the
    /// fourth through [`show_briefly`](Self::show_briefly). That split is how
    /// the toolbar once ended up revealed but untargetable (see
    /// [`update_reveal_button`](Self::update_reveal_button)). Funnelling the
    /// actual reveal through here means a fifth path cannot reintroduce either
    /// that bug or a toolbar the user asked not to have.
    fn reveal(&self) -> bool {
        if !self.enabled.get() || !self.available.get() {
            return false;
        }
        self.revealer.set_reveal_child(true);
        self.update_reveal_button();
        true
    }

    /// Brings the reveal arrow and the revealer's targetability in line with
    /// whether the toolbar is currently revealed.
    ///
    /// Both directions are set here, and that is the point. Until this was
    /// symmetric only the `false` branch existed, while
    /// [`show_briefly`](Self::show_briefly) — the path *every* state change
    /// takes — does not go through [`show`](Self::show). So the toolbar it
    /// revealed was visible and completely inert: `can_target` was still the
    /// `false` the viewer set at construction, clicks never reached the
    /// buttons, and the pointer-motion controller never fired either, so
    /// hovering could not hold the toolbar open. It became usable only after
    /// auto-hiding once and being re-revealed from the arrow. Setting it in one
    /// place means a new reveal path cannot reintroduce that.
    ///
    /// While hidden the revealer must be pass-through: it is an overlay child
    /// spanning the top of the viewer, and otherwise it would swallow input
    /// meant for the remote desktop or the web page beneath it.
    fn update_reveal_button(&self) {
        let toolbar_visible = self.revealer.reveals_child();
        self.reveal_button
            .set_visible(self.enabled.get() && self.available.get() && !toolbar_visible);
        self.revealer.set_can_target(toolbar_visible);
    }

    fn interaction_active(&self) -> bool {
        self.pointer_in_reveal_button.get()
            || self.pointer_in_toolbar.get()
            || self.focus_in_toolbar.get()
            || contains_active_menu(self.toolbar.upcast_ref())
    }

    fn schedule_hide(self: &Rc<Self>) {
        self.cancel_hide();
        let controller_weak = Rc::downgrade(self);
        let source_id = glib::timeout_add_local_once(TOOLBAR_HIDE_DELAY, move || {
            let Some(controller) = controller_weak.upgrade() else {
                return;
            };
            *controller.hide_timer.borrow_mut() = None;
            if controller.interaction_active() {
                controller.schedule_hide();
            } else {
                controller.revealer.set_reveal_child(false);
            }
        });
        *self.hide_timer.borrow_mut() = Some(source_id);
    }

    fn cancel_hide(&self) {
        if let Some(source_id) = self.hide_timer.borrow_mut().take() {
            source_id.remove();
        }
    }
}

/// Marks a session viewer as carrying no floating overlay chrome.
///
/// Nothing in `assets/style.css` matches this class — it is *state* the widget
/// carries, the same way `split_view::adapter` uses `pointer-in`. Naming it here
/// once, behind [`set_floating_overlays_suppressed`] and
/// [`floating_overlays_suppressed`], keeps the string out of the two modules
/// that need to agree on it.
///
/// The widget itself has to be the channel because of who reads it.
/// `SplitViewAdapter::set_panel_content` takes an opaque
/// `&impl IsA<gtk4::Widget>`, has no access to the `Connection`, does not import
/// `rustconn_core` at all, and runs again on every layout rebuild and every
/// drop — so a flag handed to it as an argument would have to be threaded
/// through five call sites and then stored somewhere for the sixth. A viewer
/// that travels into a split panel carries its own answer instead.
const NO_FLOATING_OVERLAYS_CSS_CLASS: &str = "no-floating-overlays";

/// Records whether this session's viewer must carry no floating overlay chrome.
///
/// Set on the viewer's root container — the widget the notebook hands to the
/// split view — alongside [`ToolbarAutoHide::set_enabled`]. The toolbar belongs
/// to the viewer, but the split panel's corner buttons do not, and a connection
/// that asked for an unobstructed desktop means both.
pub fn set_floating_overlays_suppressed(root: &impl IsA<Widget>, suppressed: bool) {
    if suppressed {
        root.as_ref().add_css_class(NO_FLOATING_OVERLAYS_CSS_CLASS);
    } else {
        root.as_ref()
            .remove_css_class(NO_FLOATING_OVERLAYS_CSS_CLASS);
    }
}

/// Whether `widget` opted out of floating overlay chrome.
///
/// Read by the split view before it wraps a session in its corner-button
/// overlay. The panel's context menu offers the same two actions, so suppressing
/// the buttons costs reachability, not capability.
#[must_use]
pub fn floating_overlays_suppressed(widget: &impl IsA<Widget>) -> bool {
    widget
        .as_ref()
        .has_css_class(NO_FLOATING_OVERLAYS_CSS_CLASS)
}

/// Applies the GNOME HIG minimum pointer/touch target to toolbar actions.
fn ensure_touch_targets(widget: &Widget) {
    if widget.is::<Button>() || widget.is::<MenuButton>() {
        widget.set_size_request(TOUCH_TARGET_PX, TOUCH_TARGET_PX);
    }

    let mut child = widget.first_child();
    while let Some(current) = child {
        child = current.next_sibling();
        ensure_touch_targets(&current);
    }
}

fn contains_active_menu(widget: &Widget) -> bool {
    if widget
        .downcast_ref::<MenuButton>()
        .is_some_and(MenuButton::is_active)
    {
        return true;
    }

    let mut child = widget.first_child();
    while let Some(current) = child {
        child = current.next_sibling();
        if contains_active_menu(&current) {
            return true;
        }
    }
    false
}
