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
    actions: std::sync::Mutex<std::sync::mpsc::Receiver<Action>>,
}

#[cfg(windows)]
impl Tray {
    /// The icon and its menu. A handler set on the icon's events takes them
    /// all (the library's own receivers then get nothing), and the window is
    /// hidden, so its frames may not run: the clicks act where they arrive.
    /// Show brings the window back (`title` finds it) and Close the game runs
    /// `close_game`; both are also told to the window, for its state.
    pub fn new(
        ctx: &egui::Context,
        title: &'static str,
        tooltip: &str,
        show: &str,
        close: &str,
        close_game: impl Fn() + Send + Sync + 'static,
    ) -> Option<Self> {
        use tray_icon::menu::{Menu, MenuEvent, MenuItem};
        use tray_icon::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
        let show_item = MenuItem::new(show, true, None);
        let close_item = MenuItem::new(close, true, None);
        let (tx, rx) = std::sync::mpsc::channel();
        let (wake, send) = (ctx.clone(), tx.clone());
        TrayIconEvent::set_event_handler(Some(move |e| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                crate::single::show_other(title);
                let _ = send.send(Action::Show);
                wake.request_repaint();
            }
        }));
        let (wake, show_id, close_id) = (ctx.clone(), show_item.id().clone(), close_item.id().clone());
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            if e.id == show_id {
                crate::single::show_other(title);
                let _ = tx.send(Action::Show);
            } else if e.id == close_id {
                close_game();
                let _ = tx.send(Action::CloseGame);
            }
            wake.request_repaint();
        }));
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
        Some(Self { _icon: icon, actions: std::sync::Mutex::new(rx) })
    }

    /// What the user asked for from the icon since the last call.
    pub fn poll(&self) -> Option<Action> {
        let rx = self.actions.lock().ok()?;
        let mut action = None;
        while let Ok(a) = rx.try_recv() {
            action = Some(a);
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
    pub fn new(
        _ctx: &egui::Context,
        _title: &'static str,
        _tooltip: &str,
        _show: &str,
        _close: &str,
        _close_game: impl Fn() + Send + Sync + 'static,
    ) -> Option<Self> {
        Some(Self)
    }

    pub fn poll(&self) -> Option<Action> {
        None
    }
}

/// Whether the window hides (a tray to bring it back) or only minimizes.
pub const HIDES: bool = cfg!(windows);
