//! Adaptive toolbar overflow for embedded protocol viewers.
//!
//! Embedded RDP/VNC/SPICE widgets build a horizontal `embedded-toolbar`
//! [`gtk4::Box`]; the embedded Web viewer builds an equivalent navigation
//! toolbar. In a narrow split panel — or a small/narrow application window —
//! that toolbar clips. [`ToolbarOverflow`] watches the viewer's width and, below
//! a documented breakpoint, folds the *secondary* actions into a "⋯" overflow
//! [`gtk4::MenuButton`] popover while the *primary* actions (Fit resolution,
//! Ctrl+Alt+Del; Back/Forward/Reload and the URL bar for Web) stay directly
//! reachable.
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

/// Collapse breakpoint for the RDP toolbar, in drawing-area pixels.
///
/// RDP carries six secondary actions (Copy, Paste, Autotype, Scripts, Quick
/// actions, Save Files) on top of the two primary ones, so the assembled
/// toolbar is the widest; below this width the secondary set is folded away.
// ponytail: eyeballed from the natural button widths at the default font; retune
// if the RDP toolbar gains/loses buttons or a theme changes button metrics.
pub const RDP_OVERFLOW_THRESHOLD_PX: i32 = 560;

/// Collapse breakpoint for the SPICE and VNC toolbars, in drawing-area pixels.
///
/// These carry only Copy + Paste as secondary actions, so they clip much later
/// than RDP and need a smaller breakpoint.
// ponytail: eyeballed; see `RDP_OVERFLOW_THRESHOLD_PX`.
pub const SPICE_VNC_OVERFLOW_THRESHOLD_PX: i32 = 360;

/// Collapse breakpoint for the embedded Web navigation toolbar, in pixels.
///
/// Sits between the other two because the Web toolbar is the only one carrying a
/// text entry: four navigation buttons, the URL bar, then Home, Autofill, Zoom
/// In and Zoom Out as secondary, then the menu. The URL bar can shrink but stops
/// being usable long before the buttons stop fitting, so this collapses earlier
/// than the button widths alone would require.
// ponytail: eyeballed from the natural widths at the default font; retune if the
// navigation toolbar gains or loses buttons.
pub const WEB_OVERFLOW_THRESHOLD_PX: i32 = 520;

/// Width margin above the collapse breakpoint required before expanding again.
///
/// Two thresholds — collapse below `threshold`, expand at/above
/// `threshold + margin` — stop the overflow button flapping when a resize drag
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
    /// Collapse breakpoint in drawing-area pixels.
    threshold_px: i32,
    /// Whether the secondary actions currently live in the overflow popover.
    collapsed: Cell<bool>,
}

impl ToolbarOverflow {
    /// Appends a hidden overflow button to `toolbar` and records the secondary actions.
    ///
    /// `secondary` is the ordered list of actions to fold into the popover when
    /// the toolbar is narrower than `threshold_px`; pass them in their toolbar
    /// order. Primary actions are simply left out of `secondary`. An empty
    /// `secondary` list makes the controller a no-op (the overflow button never
    /// appears).
    #[must_use]
    pub fn new(toolbar: &GtkBox, secondary: Vec<Widget>, threshold_px: i32) -> Rc<Self> {
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
            threshold_px,
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

    /// Collapses or expands the secondary actions for the current `width`.
    fn update(&self, width: i32) {
        if self.secondary.is_empty() {
            return;
        }
        if self.collapsed.get() {
            if width >= self.threshold_px + OVERFLOW_HYSTERESIS_PX {
                self.expand();
            }
        } else if width < self.threshold_px {
            self.collapse();
        }
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
    available: Cell<bool>,
    pointer_in_reveal_button: Cell<bool>,
    pointer_in_toolbar: Cell<bool>,
    focus_in_toolbar: Cell<bool>,
}

impl ToolbarAutoHide {
    /// Attaches auto-hide behavior and a touch/keyboard reveal control to an overlay.
    ///
    /// The reveal trigger is a small arrow indicator at the top center — hovering
    /// or clicking it reveals the full toolbar. This keeps the rest of the top
    /// edge free for window controls and split-view buttons.
    #[must_use]
    pub fn attach(overlay: &Overlay, toolbar: &GtkBox, revealer: &Revealer) -> Rc<Self> {
        ensure_touch_targets(toolbar.upcast_ref());

        let reveal_button = Button::from_icon_name("pan-down-symbolic");
        reveal_button.add_css_class("flat");
        reveal_button.add_css_class("circular");
        reveal_button.add_css_class("toolbar-reveal-handle");
        reveal_button.set_size_request(44, 44);
        reveal_button.set_halign(gtk4::Align::Center);
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

    /// Reveals the toolbar and hides it after the standard inactivity delay.
    pub fn show_briefly(self: &Rc<Self>) {
        self.available.set(true);
        self.revealer.set_reveal_child(true);
        self.update_reveal_button();
        self.schedule_hide();
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
        if !self.available.get() {
            return;
        }
        self.cancel_hide();
        self.revealer.set_can_target(true);
        self.revealer.set_reveal_child(true);
        self.update_reveal_button();
    }

    fn update_reveal_button(&self) {
        let toolbar_visible = self.revealer.reveals_child();
        self.reveal_button
            .set_visible(self.available.get() && !toolbar_visible);
        // When the toolbar is hidden, make the revealer pass-through so it
        // does not block interaction with the remote desktop or window controls
        // beneath it in the overlay stack.
        if !toolbar_visible {
            self.revealer.set_can_target(false);
        }
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

/// Applies the GNOME HIG 44×44 minimum target to toolbar actions.
fn ensure_touch_targets(widget: &Widget) {
    if widget.is::<Button>() || widget.is::<MenuButton>() {
        widget.set_size_request(44, 44);
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
