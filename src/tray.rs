//! The menu-bar (macOS) / tray (Windows) / StatusNotifier (Linux, e.g. the
//! Waybar tray) icon, with Open, Settings and Quit.

/// Small RGBA icon from the app icon.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn app_icon(size: u32) -> image::RgbaImage {
    let img = image::load_from_memory(include_bytes!("../assets/clipr-256.png"))
        .map(|i| i.into_rgba8())
        .unwrap_or_default();
    image::imageops::resize(&img, size, size, image::imageops::FilterType::Lanczos3)
}

#[cfg(any(target_os = "macos", windows))]
mod imp {
    use anyhow::Result;
    use std::cell::RefCell;
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

    thread_local! {
        static TRAY: RefCell<Option<TrayIcon>> = const { RefCell::new(None) };
    }

    /// Shows or hides the icon. Main thread only, with the event loop running.
    pub fn sync(show: bool, hotkey: &str) {
        TRAY.with(|tray| {
            let mut tray = tray.borrow_mut();
            if show && tray.is_none() {
                install_handlers();
                match build(hotkey) {
                    Ok(icon) => *tray = Some(icon),
                    Err(e) => eprintln!("clipr: tray icon: {e:#}"),
                }
            } else if !show {
                *tray = None; // dropping it removes the icon
            }
        });
    }

    fn build(hotkey: &str) -> Result<TrayIcon> {
        let menu = Menu::new();
        let open = MenuItem::with_id("open", format!("Open clipr    {}", crate::hotkey::display(hotkey)), true, None);
        let settings = MenuItem::with_id("settings", "Settings…", true, None);
        let quit = MenuItem::with_id("quit", "Quit clipr", true, None);
        menu.append_items(&[&open, &settings, &PredefinedMenuItem::separator(), &quit])?;
        let builder = TrayIconBuilder::new().with_menu(Box::new(menu)).with_tooltip("clipr");
        // macOS: a black-and-white "template" image, tinted by the menu bar.
        #[cfg(target_os = "macos")]
        let builder = builder.with_icon_templated(icon()?);
        // Windows: left click opens clipr, right click shows the menu.
        #[cfg(windows)]
        let builder = builder.with_icon(icon()?).with_menu_on_left_click(false);
        Ok(builder.build()?)
    }

    fn icon() -> Result<Icon> {
        #[cfg(target_os = "macos")]
        let img = image::load_from_memory(include_bytes!("../assets/tray-template.png"))?.into_rgba8();
        #[cfg(windows)]
        let img = super::app_icon(32);
        let (w, h) = img.dimensions();
        Ok(Icon::from_rgba(img.into_raw(), w, h)?)
    }

    fn install_handlers() {
        MenuEvent::set_event_handler(Some(|event: MenuEvent| match event.id.0.as_str() {
            "open" => crate::daemon::open_popup(false),
            "settings" => crate::daemon::open_popup(true),
            "quit" => crate::ui::request_quit(),
            _ => {}
        }));
        #[cfg(windows)]
        tray_icon::TrayIconEvent::set_event_handler(Some(|event| {
            use tray_icon::{MouseButton, MouseButtonState, TrayIconEvent};
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                crate::daemon::open_popup(false);
            }
        }));
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use ksni::blocking::{Handle, TrayMethods};
    use std::sync::Mutex;

    struct Tray {
        hotkey_hint: String,
    }

    impl ksni::Tray for Tray {
        fn id(&self) -> String {
            "clipr".into()
        }
        fn title(&self) -> String {
            "clipr".into()
        }
        fn icon_name(&self) -> String {
            "clipr".into()
        }
        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            [32, 64]
                .into_iter()
                .map(|size| {
                    let img = super::app_icon(size);
                    // ARGB32, network byte order.
                    let data = img.pixels().flat_map(|p| [p[3], p[0], p[1], p[2]]).collect();
                    ksni::Icon { width: size as i32, height: size as i32, data }
                })
                .collect()
        }
        fn tool_tip(&self) -> ksni::ToolTip {
            ksni::ToolTip { title: "clipr".into(), ..Default::default() }
        }
        fn activate(&mut self, _x: i32, _y: i32) {
            crate::daemon::open_popup(false);
        }
        fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
            use ksni::menu::StandardItem;
            let open_label = if self.hotkey_hint.is_empty() {
                "Open clipr".to_owned()
            } else {
                format!("Open clipr    {}", self.hotkey_hint)
            };
            vec![
                StandardItem {
                    label: open_label,
                    activate: Box::new(|_: &mut Self| crate::daemon::open_popup(false)),
                    ..Default::default()
                }
                .into(),
                StandardItem {
                    label: "Settings…".into(),
                    activate: Box::new(|_: &mut Self| crate::daemon::open_popup(true)),
                    ..Default::default()
                }
                .into(),
                ksni::MenuItem::Separator,
                StandardItem {
                    label: "Quit clipr".into(),
                    activate: Box::new(|_: &mut Self| crate::daemon::quit()),
                    ..Default::default()
                }
                .into(),
            ]
        }
    }

    static TRAY: Mutex<Option<Handle<Tray>>> = Mutex::new(None);
    static FAILED: Mutex<bool> = Mutex::new(false);

    /// Shows or hides the icon (cheap to call often).
    pub fn sync(show: bool, hotkey_hint: &str) {
        let mut tray = TRAY.lock().unwrap();
        if show && tray.is_none() {
            let mut failed = FAILED.lock().unwrap();
            if *failed {
                return; // no tray host (no Waybar tray…): don't retry every tick
            }
            match (Tray { hotkey_hint: hotkey_hint.to_owned() }).spawn() {
                Ok(handle) => *tray = Some(handle),
                Err(e) => {
                    *failed = true;
                    eprintln!("clipr: tray icon: {e}");
                }
            }
        } else if !show {
            if let Some(handle) = tray.take() {
                handle.shutdown().wait();
            }
        }
    }
}

pub use imp::sync;
