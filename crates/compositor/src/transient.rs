//! Transient UI: the menus and panels of the shell (`silica_shell_manager_v1.set_transient`,
//! `set_owned_transient`) and the app popups with a grab (`xdg_popup.grab`).
//!
//! While one is open the other surfaces keep the pointer: the shell no longer covers the
//! screen with its input region (#149). Outside a transient there are two targets, the
//! surfaces of its owner app and every other surface, each with a [`Rule`]. The measured model
//! (the reference's INPUT-01): a menu is modal only for its own app (no hover, no wheel, a
//! click there only closes it) and lets a click elsewhere through; system panels never block
//! other windows; the Dock's icon menu gives the app that was active only an activation; the
//! wheel never closes anything. Over every surface outside a transient the compositor shows the arrow.

use silica_protocols::shell::server::silica_shell_manager_v1::{Outside, SilicaShellManagerV1};
use smithay::desktop::{PopupKind, PopupManager, WindowSurfaceType, find_popup_root_surface};
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{IsAlive, Logical, Point, Rectangle};
use smithay::wayland::compositor::get_parent;

use crate::state::State;

/// What a button press outside a transient does after ending it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Press {
    /// It reaches the surface under the pointer, as with nothing open.
    Pass,
    /// It only activates the window under the pointer; the surface does not see it.
    Activate,
    /// Nothing else happens.
    Close,
}

/// What the surfaces of one target receive while a transient is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rule {
    /// Pointer enter and motion (hover feedback, tooltips).
    pub hover: bool,
    /// Wheel and touchpad scrolling.
    pub scroll: bool,
    /// The primary (left) button.
    pub primary: Press,
    /// The other buttons (the secondary click opens that surface's own menu).
    pub secondary: Press,
}

impl Rule {
    /// The owner app of a menu: it gets nothing, a click there only closes the menu.
    pub const MODAL: Self = Self {
        hover: false,
        scroll: false,
        primary: Press::Close,
        secondary: Press::Close,
    };
    /// Everything goes through, a click too (it also ends the transient).
    pub const PASS: Self = Self {
        hover: true,
        scroll: true,
        primary: Press::Pass,
        secondary: Press::Pass,
    };
    /// Like a click on an inactive text window: the left click only activates.
    pub const ACTIVATE: Self = Self {
        hover: true,
        scroll: true,
        primary: Press::Activate,
        secondary: Press::Pass,
    };

    /// The rule of `silica_shell_manager_v1.rule` (version 7). An unknown value is modal.
    pub fn from_protocol(value: u32) -> Self {
        match value {
            1 => Self::PASS,
            2 => Self::ACTIVATE,
            _ => Self::MODAL,
        }
    }

    /// The version 6 rule, the same for every target: hover always, press and wheel each
    /// consumed or delivered.
    pub fn from_outside(press: Option<Outside>, scroll: Option<Outside>) -> Self {
        let press = match press {
            Some(Outside::PassThrough) => Press::Pass,
            _ => Press::Close,
        };
        Self {
            hover: true,
            scroll: matches!(scroll, Some(Outside::PassThrough)),
            primary: press,
            secondary: press,
        }
    }

    fn press(&self, primary: bool) -> Press {
        if primary {
            self.primary
        } else {
            self.secondary
        }
    }
}

/// Who opened the transient.
#[derive(Debug, Clone)]
pub enum Source {
    /// A shell surface (menu bar, Dock, Faro…), told with `dismissed`.
    Shell {
        surface: WlSurface,
        manager: SilicaShellManagerV1,
    },
    /// An app popup with a grab, closed with `popup_done`.
    Popup { popup: PopupKind, root: WlSurface },
    /// A context menu or pop-up button of an app, drawn by the shell (`popup_menu.rs`); the
    /// shell's surface is known once it reports where the menu is.
    PopupMenu {
        serial: u32,
        object: silica_protocols::menu::server::silica_popup_menu_v1::SilicaPopupMenuV1,
        surface: Option<WlSurface>,
    },
}

/// The app whose surfaces follow the owner rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnerApp {
    /// The system: every surface follows the other rule.
    None,
    /// The windows with this app id and the desktop surface `desktop-icons:<app id>`.
    AppId(String),
    /// Whichever app owns the desktop surface (`desktop-icons`), resolved at each event.
    Desktop,
    /// Every surface of this client (an app popup's own app).
    Client(ClientId),
}

impl OwnerApp {
    /// From the protocol's `owner` string.
    pub fn parse(owner: &str) -> Self {
        match crate::layers::desktop_owner(owner) {
            Some("") => Self::Desktop,
            Some(app) => Self::AppId(app.to_owned()),
            None if owner.is_empty() => Self::None,
            None => Self::AppId(owner.to_owned()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Transient {
    pub source: Source,
    /// Areas that count as inside even under another surface (global, logical).
    pub region: Vec<Rectangle<i32, Logical>>,
    pub owner: OwnerApp,
    pub owner_rule: Rule,
    pub other_rule: Rule,
}

impl Transient {
    /// The surface the transient belongs to (for replacing and clearing).
    pub fn surface(&self) -> Option<&WlSurface> {
        match &self.source {
            Source::Shell { surface, .. } => Some(surface),
            Source::Popup { root, .. } => Some(root),
            Source::PopupMenu { surface, .. } => surface.as_ref(),
        }
    }

    fn alive(&self) -> bool {
        match &self.source {
            Source::Shell { surface, manager } => surface.is_alive() && manager.is_alive(),
            Source::Popup { popup, .. } => popup.alive(),
            Source::PopupMenu { object, .. } => object.is_alive(),
        }
    }
}

/// Where a point lies for one open transient.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    /// On the transient (its surface, its popups, its region).
    Inside,
    /// On a surface of the owner app.
    Owner,
    /// Anywhere else, the desktop included.
    Other,
}

/// The rule of a transient for a place outside it (None inside).
fn rule_at(place: Place, owner: &Rule, other: &Rule) -> Option<Rule> {
    match place {
        Place::Inside => None,
        Place::Owner => Some(*owner),
        Place::Other => Some(*other),
    }
}

/// The outcome of a press with transients open: which ones end, and what the press does.
/// `places[i]` and `rules[i]` (owner, other) belong to transient `i`; the strictest outcome
/// of the transients that end wins, and a press inside all of them goes through.
pub fn press_outcome(
    places: &[Place],
    rules: &[(Rule, Rule)],
    primary: bool,
) -> (Vec<usize>, Press) {
    let mut ended = Vec::new();
    let mut outcome = Press::Pass;
    for (index, (place, (owner, other))) in places.iter().zip(rules).enumerate() {
        if let Some(rule) = rule_at(*place, owner, other) {
            ended.push(index);
            outcome = outcome.max(rule.press(primary));
        }
    }
    (ended, outcome)
}

/// Whether the surface at these places is cut off from the pointer (`hover`) or the wheel:
/// outside every transient and modal for one of them.
fn blocked(places: &[Place], rules: &[(Rule, Rule)], allowed: impl Fn(&Rule) -> bool) -> bool {
    !places.contains(&Place::Inside)
        && places
            .iter()
            .zip(rules)
            .filter_map(|(place, (owner, other))| rule_at(*place, owner, other))
            .any(|rule| !allowed(&rule))
}

/// Whether the wheel is dropped at these places.
pub fn scroll_dropped(places: &[Place], rules: &[(Rule, Rule)]) -> bool {
    blocked(places, rules, |rule| rule.scroll)
}

/// Whether the pointer must leave the surface at these places.
pub fn hover_blocked(places: &[Place], rules: &[(Rule, Rule)]) -> bool {
    blocked(places, rules, |rule| rule.hover)
}

/// The top surface of a tree: up through subsurfaces and popups.
fn root_of(popups: &PopupManager, surface: &WlSurface) -> WlSurface {
    let mut current = surface.clone();
    for _ in 0..64 {
        if let Some(parent) = get_parent(&current) {
            current = parent;
            continue;
        }
        match popups.find_popup(&current) {
            Some(popup) => match find_popup_root_surface(&popup) {
                Ok(root) if root != current => current = root,
                _ => break,
            },
            None => break,
        }
    }
    current
}

/// The surfaces of the popups `popup` hangs from, nearest first.
fn popup_ancestors(popups: &PopupManager, popup: &PopupKind) -> Vec<WlSurface> {
    let mut found = Vec::new();
    let parent_of = |popup: &PopupKind| match popup {
        PopupKind::Xdg(xdg) => xdg.get_parent_surface(),
        _ => None,
    };
    let mut current = parent_of(popup);
    while let Some(surface) = current {
        let Some(parent) = popups.find_popup(&surface) else {
            break;
        };
        current = parent_of(&parent);
        found.push(surface);
    }
    found
}

/// The popup a surface belongs to (up through its subsurfaces), with that popup's root.
fn popup_root_of(popups: &PopupManager, surface: &WlSurface) -> Option<WlSurface> {
    let mut current = surface.clone();
    for _ in 0..64 {
        match get_parent(&current) {
            Some(parent) => current = parent,
            None => break,
        }
    }
    popups
        .find_popup(&current)
        .and_then(|popup| find_popup_root_surface(&popup).ok())
}

/// What the pointer is on, for the rules.
struct Under {
    surface: Option<WlSurface>,
    root: Option<WlSurface>,
    /// The app the surface belongs to: the window's app id, or the desktop surface's owner.
    app: Option<String>,
}

impl State {
    fn under_for_transients(&self, location: Point<f64, Logical>) -> Under {
        let surface = self.surface_under(location).map(|(surface, _)| surface);
        let root = surface.as_ref().map(|s| root_of(&self.popups, s));
        let app = root.as_ref().and_then(|root| {
            let window = self
                .space
                .elements()
                .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == root));
            match window {
                Some(window) => crate::panorama::window_app(window),
                None => {
                    let map = smithay::desktop::layer_map_for_output(&self.output);
                    map.layer_for_surface(root, WindowSurfaceType::TOPLEVEL)
                        .and_then(|layer| {
                            crate::layers::desktop_owner(layer.namespace()).map(str::to_owned)
                        })
                        .filter(|app| !app.is_empty())
                }
            }
        });
        Under { surface, root, app }
    }

    /// Where a point with this surface under it lies for each open transient.
    fn places(&self, under: &Under, location: Point<f64, Logical>) -> Vec<Place> {
        let desktop_owner = self
            .desktop_layer()
            .map(|(_, owner)| owner)
            .filter(|owner| !owner.is_empty());
        self.transients
            .iter()
            .map(|transient| {
                let in_region = transient
                    .region
                    .iter()
                    .any(|rect| rect.to_f64().contains(location));
                let on_source = match &transient.source {
                    Source::Shell { surface, .. } => under.root.as_ref() == Some(surface),
                    Source::PopupMenu { surface, .. } => {
                        surface.is_some() && under.root.as_ref() == surface.as_ref()
                    }
                    // The popup and the popups next to it (submenus) on the same window.
                    Source::Popup { root, .. } => under
                        .surface
                        .as_ref()
                        .and_then(|s| popup_root_of(&self.popups, s))
                        .is_some_and(|popup_root| popup_root == *root),
                };
                if in_region || on_source {
                    return Place::Inside;
                }
                let owner = match &transient.owner {
                    OwnerApp::None => false,
                    OwnerApp::AppId(app) => under.app.as_ref() == Some(app),
                    OwnerApp::Desktop => {
                        desktop_owner.is_some() && under.app.as_ref() == desktop_owner.as_ref()
                    }
                    OwnerApp::Client(client) => under
                        .surface
                        .as_ref()
                        .and_then(Resource::client)
                        .is_some_and(|c| c.id() == *client),
                };
                if owner { Place::Owner } else { Place::Other }
            })
            .collect()
    }

    fn rules(&self) -> Vec<(Rule, Rule)> {
        self.transients
            .iter()
            .map(|t| (t.owner_rule, t.other_rule))
            .collect()
    }

    fn drop_dead_transients(&mut self) {
        self.transients.retain(Transient::alive);
    }

    /// A press: ends the transients it is outside of, and says what the press does next.
    pub fn press_on_transients(&mut self, location: Point<f64, Logical>, primary: bool) -> Press {
        self.drop_dead_transients();
        if self.transients.is_empty() {
            return Press::Pass;
        }
        let under = self.under_for_transients(location);
        let places = self.places(&under, location);
        let (dismissed, outcome) = press_outcome(&places, &self.rules(), primary);
        let mut ended = Vec::new();
        for index in dismissed.into_iter().rev() {
            if index < self.transients.len() {
                ended.push(self.transients.remove(index));
            }
        }
        for transient in ended {
            self.end_transient(transient);
        }
        outcome
    }

    /// A scroll: true if it must not reach the surface under the pointer. It never ends a
    /// transient.
    pub fn scroll_on_transients(&self, location: Point<f64, Logical>) -> bool {
        if self.transients.is_empty() {
            return false;
        }
        let under = self.under_for_transients(location);
        scroll_dropped(&self.places(&under, location), &self.rules())
    }

    /// The pointer moves with transients open: (whether the surface under it loses the
    /// pointer, whether it is outside every transient and gets the arrow).
    pub fn hover_on_transients(&self, location: Point<f64, Logical>) -> (bool, bool) {
        if self.transients.is_empty() {
            return (false, false);
        }
        let under = self.under_for_transients(location);
        let places = self.places(&under, location);
        (
            hover_blocked(&places, &self.rules()),
            !places.contains(&Place::Inside),
        )
    }

    /// A press that only activates (the Dock menu's left click on a window): the window
    /// under the pointer takes the focus and comes to the top, the desktop makes its owner
    /// frontmost; the client does not see the press.
    pub fn activate_under(&mut self, location: Point<f64, Logical>) {
        use smithay::wayland::shell::wlr_layer::Layer;
        if let Some((layer, _)) = self.layer_under(&[Layer::Overlay, Layer::Top], location) {
            if layer.can_receive_keyboard_focus() {
                self.focus_layer(&layer);
            }
            return;
        }
        match self.window_hit(location) {
            Some(crate::decorations::Hit::Titlebar(window, _, _))
            | Some(crate::decorations::Hit::Content(window)) => self.focus_window(Some(window)),
            None => self.desktop_pressed(),
        }
    }

    /// A change of context (another window activated, the desktop, a space switch, Panorama):
    /// every open menu, popup and panel ends, the shell with `dismissed`, the apps' popups
    /// with `popup_done` (#219).
    pub fn dismiss_transients(&mut self) {
        self.drop_dead_transients();
        if self.transients.is_empty() {
            return;
        }
        for transient in std::mem::take(&mut self.transients) {
            self.end_transient(transient);
        }
        self.refresh_pointer();
    }

    fn end_transient(&mut self, transient: Transient) {
        match transient.source {
            Source::Shell { surface, manager } => {
                if manager.version() >= 6 {
                    manager.dismissed(&surface);
                }
            }
            Source::PopupMenu { serial, .. } => {
                self.finish_popup_menu(serial, crate::popup_menu::Ended::Dismissed);
            }
            Source::Popup { popup, root } => {
                if let Err(err) = PopupManager::dismiss_popup(&root, &popup) {
                    tracing::warn!("popup dismiss: {err}");
                }
                // The keyboard goes back to the popup's window.
                if let Some(keyboard) = self.seat.get_keyboard()
                    && keyboard
                        .current_focus()
                        .is_some_and(|focus| focus == *popup.wl_surface())
                {
                    keyboard.set_focus(
                        self,
                        Some(root),
                        smithay::utils::SERIAL_COUNTER.next_serial(),
                    );
                }
            }
        }
    }

    /// The transients changed: the pointer's target and shape are worked out again where it
    /// is (the owner's window loses or gets back the hover without waiting for a motion).
    pub fn refresh_pointer(&mut self) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        if pointer.is_grabbed() {
            return;
        }
        let location = pointer.current_location();
        let time = self.now_msec();
        self.pointer_move_to(location, time);
    }

    /// Where a layer surface (or its tree) sits on the screen, logical; the output origin if
    /// it is not a layer surface.
    pub fn surface_origin(&self, surface: &WlSurface) -> Point<i32, Logical> {
        let origin = self
            .space
            .output_geometry(&self.output)
            .map(|g| g.loc)
            .unwrap_or_default();
        let map = smithay::desktop::layer_map_for_output(&self.output);
        map.layer_for_surface(surface, WindowSurfaceType::ALL)
            .and_then(|layer| map.layer_geometry(layer))
            .map_or(origin, |geometry| geometry.loc + origin)
    }

    /// A shell surface opens (or replaces) its transient.
    pub fn set_shell_transient(
        &mut self,
        manager: SilicaShellManagerV1,
        surface: WlSurface,
        region: Vec<Rectangle<i32, Logical>>,
        owner: OwnerApp,
        (owner_rule, other_rule): (Rule, Rule),
    ) {
        self.transients.retain(|t| t.surface() != Some(&surface));
        self.transients.push(Transient {
            source: Source::Shell { surface, manager },
            region,
            owner,
            owner_rule,
            other_rule,
        });
        self.refresh_pointer();
    }

    /// The shell closed its transient itself.
    pub fn clear_shell_transient(&mut self, surface: &WlSurface) {
        let before = self.transients.len();
        self.transients
            .retain(|t| !matches!(&t.source, Source::Shell { surface: s, .. } if s == surface));
        if self.transients.len() != before {
            self.refresh_pointer();
        }
    }

    /// A popup goes: its grab ends (it is not told, it is gone), and the keyboard it had goes
    /// to `parent`, the popup or window it hung from.
    pub fn end_popup_grab(&mut self, gone: &WlSurface, parent: Option<WlSurface>) {
        let before = self.transients.len();
        self.transients.retain(
            |t| !matches!(&t.source, Source::Popup { popup, .. } if popup.wl_surface() == gone),
        );
        let had_keyboard = self
            .seat
            .get_keyboard()
            .is_some_and(|keyboard| keyboard.current_focus().is_none_or(|focus| focus == *gone));
        if had_keyboard
            && let (Some(keyboard), Some(parent)) = (self.seat.get_keyboard(), parent)
            && parent.is_alive()
        {
            keyboard.set_focus(
                self,
                Some(parent),
                smithay::utils::SERIAL_COUNTER.next_serial(),
            );
        }
        if self.transients.len() != before {
            self.refresh_pointer();
        }
    }

    /// An app popup asks for a grab: it is that app's menu (INPUT-01 (a)), modal for the app
    /// and letting clicks through elsewhere; it has the keyboard while open.
    pub fn grab_popup(&mut self, popup: PopupKind) {
        let Ok(root) = find_popup_root_surface(&popup) else {
            return;
        };
        let surface = popup.wl_surface().clone();
        let owner = match surface.client() {
            Some(client) => OwnerApp::Client(client.id()),
            None => OwnerApp::None,
        };
        // A popup replaces the other grabs of its window, except the popups it hangs from (a
        // submenu opens over its menu, which stays open and grabbed). The replaced ones are
        // dismissed, not forgotten: a popup that lost its grab would never get its
        // `popup_done` from a press outside, and stay on the screen for good.
        let ancestors = popup_ancestors(&self.popups, &popup);
        let (keep, replaced): (Vec<_>, Vec<_>) = std::mem::take(&mut self.transients)
            .into_iter()
            .partition(|t| match &t.source {
                Source::Popup { popup: p, root: r } => {
                    r != &root || ancestors.contains(p.wl_surface())
                }
                Source::Shell { .. } | Source::PopupMenu { .. } => true,
            });
        self.transients = keep;
        for transient in replaced {
            if let Source::Popup {
                popup: old,
                root: old_root,
            } = &transient.source
                && let Err(err) = PopupManager::dismiss_popup(old_root, old)
            {
                tracing::warn!("popup dismiss: {err}");
            }
        }
        self.transients.push(Transient {
            source: Source::Popup { popup, root },
            region: Vec::new(),
            owner,
            owner_rule: Rule::MODAL,
            other_rule: Rule::PASS,
        });
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(
                self,
                Some(surface),
                smithay::utils::SERIAL_COUNTER.next_serial(),
            );
        }
        self.refresh_pointer();
    }
}

#[cfg(test)]
mod tests {
    use super::{OwnerApp, Place, Press, Rule, hover_blocked, press_outcome, scroll_dropped};
    use Place::{Inside, Other, Owner};

    /// A menu: modal for its app, through elsewhere (INPUT-01 (a), (b)).
    fn menu() -> (Rule, Rule) {
        (Rule::MODAL, Rule::PASS)
    }
    /// A system panel (Faro, Control Center, status menus, Notification Center, launcher).
    fn panel() -> (Rule, Rule) {
        (Rule::PASS, Rule::PASS)
    }
    /// The Dock's icon menu, the app that was active as owner (INPUT-01 (c)).
    fn dock_menu() -> (Rule, Rule) {
        (Rule::ACTIVATE, Rule::PASS)
    }

    #[test]
    fn a_press_inside_keeps_every_transient() {
        assert_eq!(
            press_outcome(&[Inside], &[menu()], true),
            (vec![], Press::Pass)
        );
    }

    #[test]
    fn a_menu_click_in_its_own_app_only_closes_it() {
        assert_eq!(
            press_outcome(&[Owner], &[menu()], true),
            (vec![0], Press::Close)
        );
        assert_eq!(
            press_outcome(&[Owner], &[menu()], false),
            (vec![0], Press::Close)
        );
    }

    #[test]
    fn a_menu_click_in_another_app_closes_it_and_goes_through() {
        assert_eq!(
            press_outcome(&[Other], &[menu()], true),
            (vec![0], Press::Pass)
        );
        // The right click too: it opens that app's own menu.
        assert_eq!(
            press_outcome(&[Other], &[menu()], false),
            (vec![0], Press::Pass)
        );
    }

    #[test]
    fn a_panel_never_blocks() {
        for place in [Owner, Other] {
            for primary in [true, false] {
                assert_eq!(
                    press_outcome(&[place], &[panel()], primary),
                    (vec![0], Press::Pass)
                );
            }
            assert!(!scroll_dropped(&[place], &[panel()]));
            assert!(!hover_blocked(&[place], &[panel()]));
        }
    }

    #[test]
    fn the_dock_menu_only_reactivates_the_app_that_was_active() {
        assert_eq!(
            press_outcome(&[Owner], &[dock_menu()], true),
            (vec![0], Press::Activate)
        );
        assert_eq!(
            press_outcome(&[Owner], &[dock_menu()], false),
            (vec![0], Press::Pass)
        );
        assert_eq!(
            press_outcome(&[Other], &[dock_menu()], true),
            (vec![0], Press::Pass)
        );
        assert!(!scroll_dropped(&[Owner], &[dock_menu()]));
        assert!(!hover_blocked(&[Owner], &[dock_menu()]));
    }

    #[test]
    fn with_two_open_the_strictest_of_those_ending_wins() {
        // Inside the first (the press lands there), outside the second: only the second ends.
        assert_eq!(
            press_outcome(&[Inside, Other], &[menu(), panel()], true),
            (vec![1], Press::Pass)
        );
        assert_eq!(
            press_outcome(&[Other, Owner], &[panel(), menu()], true),
            (vec![0, 1], Press::Close)
        );
        assert_eq!(
            press_outcome(&[Owner, Other], &[dock_menu(), panel()], true),
            (vec![0, 1], Press::Activate)
        );
    }

    #[test]
    fn the_wheel_and_the_hover_are_cut_only_in_the_menus_own_app() {
        assert!(scroll_dropped(&[Owner], &[menu()]));
        assert!(hover_blocked(&[Owner], &[menu()]));
        assert!(!scroll_dropped(&[Other], &[menu()]));
        assert!(!hover_blocked(&[Other], &[menu()]));
        // On the menu itself everything works.
        assert!(!scroll_dropped(&[Inside], &[menu()]));
        assert!(!hover_blocked(&[Inside], &[menu()]));
    }

    #[test]
    fn the_version_6_rule_keeps_its_meaning() {
        use silica_protocols::shell::server::silica_shell_manager_v1::Outside;
        let consume = Rule::from_outside(Some(Outside::Consume), Some(Outside::Consume));
        assert_eq!(
            press_outcome(&[Other], &[(consume, consume)], true).1,
            Press::Close
        );
        assert!(scroll_dropped(&[Other], &[(consume, consume)]));
        assert!(!hover_blocked(&[Other], &[(consume, consume)]));
        let through = Rule::from_outside(Some(Outside::PassThrough), Some(Outside::PassThrough));
        assert_eq!(
            press_outcome(&[Other], &[(through, through)], true).1,
            Press::Pass
        );
        assert!(!scroll_dropped(&[Other], &[(through, through)]));
    }

    #[test]
    fn owners_are_parsed() {
        assert_eq!(OwnerApp::parse(""), OwnerApp::None);
        assert_eq!(
            OwnerApp::parse("dev.frascella.Terminal"),
            OwnerApp::AppId("dev.frascella.Terminal".into())
        );
        assert_eq!(OwnerApp::parse("desktop-icons"), OwnerApp::Desktop);
        assert_eq!(
            OwnerApp::parse("desktop-icons:dev.frascella.Mustache"),
            OwnerApp::AppId("dev.frascella.Mustache".into())
        );
    }
}
