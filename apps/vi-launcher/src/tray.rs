//! The launcher's icon in the tray while the game is open: the window goes
//! away and comes back when the game closes, or from the icon's menu.
use eframe::egui;

pub enum Action {
    Show,
    CloseGame,
}

#[cfg(windows)]
pub struct Tray {
    _icon: tray_icon::TrayIcon,
    show: tray_icon::menu::MenuId,
    close: tray_icon::menu::MenuId,
}

#[cfg(windows)]
impl Tray {
    /// The icon and its menu; the window is woken by any click on them.
    pub fn new(ctx: &egui::Context, tooltip: &str, show: &str, close: &str) -> Option<Self> {
        use tray_icon::menu::{Menu, MenuEvent, MenuItem};
        use tray_icon::{TrayIconBuilder, TrayIconEvent};
        let wake = ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |_| wake.request_repaint()));
        let wake = ctx.clone();
        MenuEvent::set_event_handler(Some(move |_| wake.request_repaint()));
        let show_item = MenuItem::new(show, true, None);
        let close_item = MenuItem::new(close, true, None);
        let menu = Menu::new();
        menu.append(&show_item).ok()?;
        menu.append(&close_item).ok()?;
        let icon = TrayIconBuilder::new()
            .with_tooltip(tooltip)
            .with_icon(icon()?)
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build()
            .ok()?;
        Some(Self { _icon: icon, show: show_item.id().clone(), close: close_item.id().clone() })
    }

    /// What the user asked for from the icon since the last call.
    pub fn poll(&self) -> Option<Action> {
        use tray_icon::menu::MenuEvent;
        use tray_icon::{MouseButton, MouseButtonState, TrayIconEvent};
        let mut action = None;
        while let Ok(e) = TrayIconEvent::receiver().try_recv() {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                action = Some(Action::Show);
            }
        }
        while let Ok(e) = MenuEvent::receiver().try_recv() {
            if e.id == self.show {
                action = Some(Action::Show);
            } else if e.id == self.close {
                action = Some(Action::CloseGame);
            }
        }
        action
    }
}

/// A 32 x 32 sun: the design's gold at the top to its accent at the bottom.
#[cfg(windows)]
fn icon() -> Option<tray_icon::Icon> {
    tray_icon::Icon::from_rgba(crate::mark::rgba(32), 32, 32).ok()
}

/// Elsewhere there is no tray here: the window is only minimized.
#[cfg(not(windows))]
pub struct Tray;

#[cfg(not(windows))]
impl Tray {
    pub fn new(_ctx: &egui::Context, _tooltip: &str, _show: &str, _close: &str) -> Option<Self> {
        Some(Self)
    }

    pub fn poll(&self) -> Option<Action> {
        None
    }
}

/// Whether the window hides (a tray to bring it back) or only minimizes.
pub const HIDES: bool = cfg!(windows);
