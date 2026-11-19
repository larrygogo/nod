//! Tray icon: the app icon with a status dot, a one-line status in the menu, and open / pause / quit.

use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Wry};

use crate::supervisor::Status;

const BASE: &[u8] = include_bytes!("../icons/64x64.png");

/// Status dot colour (RGB), or none for "idle".
pub fn dot(state: &str) -> Option<[u8; 3]> {
    match state {
        "reviewing" | "starting" => Some([0x1a, 0x7f, 0x37]), // green: working
        "error" => Some([0xcf, 0x22, 0x2e]),                  // red
        "paused" | "setup" => Some([0x9a, 0x67, 0x00]),       // amber
        _ => None,
    }
}

/// The app icon with a status dot in the lower right corner (with a white ring so it reads on any taskbar).
pub fn icon_rgba(state: &str) -> (Vec<u8>, u32, u32) {
    let img = Image::from_bytes(BASE).expect("bundled tray icon");
    let (w, h) = (img.width(), img.height());
    let mut px = img.rgba().to_vec();
    if let Some(c) = dot(state) {
        let r = w as f32 * 0.22;
        let (cx, cy) = (w as f32 - r - 1.0, h as f32 - r - 1.0);
        for y in 0..h {
            for x in 0..w {
                let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
                let i = ((y * w + x) * 4) as usize;
                if d <= r - 3.0 {
                    px[i..i + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
                } else if d <= r {
                    px[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
                }
            }
        }
    }
    (px, w, h)
}

pub struct Tray {
    pub icon: TrayIcon<Wry>,
    recent: Submenu<Wry>,
    app: AppHandle,
    status: MenuItem<Wry>,
    pause: MenuItem<Wry>,
    open: MenuItem<Wry>,
    quit: MenuItem<Wry>,
    menu: Menu<Wry>,
    /// "Update to x.y.z", shown only while one is available.
    update: MenuItem<Wry>,
    update_shown: std::cell::Cell<bool>,
}

pub fn text(lang: &str, key: &str) -> &'static str {
    let en = lang == "en";
    match key {
        "open" => {
            if en {
                "Open Nod"
            } else {
                "打开 Nod"
            }
        }
        "pause" => {
            if en {
                "Pause reviewing"
            } else {
                "暂停审查"
            }
        }
        "resume" => {
            if en {
                "Resume reviewing"
            } else {
                "继续审查"
            }
        }
        "quit" => {
            if en {
                "Quit"
            } else {
                "退出"
            }
        }
        "recent" => {
            if en {
                "Latest reviews"
            } else {
                "最近审查"
            }
        }
        "none" => {
            if en {
                "No reviews yet"
            } else {
                "还没有审查"
            }
        }
        "setup" => {
            if en {
                "Not set up yet"
            } else {
                "还没完成设置"
            }
        }
        "paused" => {
            if en {
                "Paused"
            } else {
                "已暂停"
            }
        }
        "starting" => {
            if en {
                "Starting…"
            } else {
                "正在启动…"
            }
        }
        "idle" => {
            if en {
                "Watching for review requests"
            } else {
                "正在等待审查请求"
            }
        }
        "reviewing" => {
            if en {
                "Reviewing"
            } else {
                "正在审查"
            }
        }
        "error" => {
            if en {
                "Needs attention"
            } else {
                "出了问题，点开查看"
            }
        }
        _ => "",
    }
}

pub fn status_line(lang: &str, s: &Status) -> String {
    let base = text(lang, s.state);
    if let Some(t) = s.title.as_ref().filter(|_| s.state == "error") {
        return t.clone();
    }
    if s.state == "reviewing" && s.active > 0 {
        format!("{base} ({})", s.active)
    } else {
        base.to_string()
    }
}

pub fn build(app: &AppHandle, lang: &str) -> tauri::Result<Tray> {
    let status = MenuItem::with_id(app, "status", "nod", false, None::<&str>)?;
    let open = MenuItem::with_id(app, "open", text(lang, "open"), true, None::<&str>)?;
    let pause = MenuItem::with_id(app, "pause", text(lang, "pause"), true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", text(lang, "quit"), true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let none = MenuItem::with_id(app, "recent-none", text(lang, "none"), false, None::<&str>)?;
    let recent = Submenu::with_id_and_items(app, "recent", text(lang, "recent"), true, &[&none])?;
    let menu = Menu::with_items(app, &[&status, &sep, &open, &recent, &pause, &sep2, &quit])?;
    let update = MenuItem::with_id(app, "update", "", true, None::<&str>)?;
    let (px, w, h) = icon_rgba("starting");
    let icon = TrayIconBuilder::with_id("main")
        .icon(Image::new_owned(px, w, h))
        .tooltip("nod")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, ev| crate::app::on_menu(app, ev.id().as_ref()))
        .on_tray_icon_event(|t, ev| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = ev
            {
                crate::app::show_main(t.app_handle());
            }
        })
        .build(app)?;
    Ok(Tray {
        icon,
        recent,
        app: app.clone(),
        status,
        pause,
        open,
        quit,
        menu,
        update,
        update_shown: std::cell::Cell::new(false),
    })
}

impl Tray {
    /// Show "Update to x.y.z" right under the status line while a newer version is available.
    pub fn set_update(&self, lang: &str, version: Option<&str>) {
        match version {
            Some(v) => {
                let label = if lang == "en" {
                    format!("Update to {v} and restart")
                } else {
                    format!("更新到 {v} 并重启")
                };
                let _ = self.update.set_text(label);
                if !self.update_shown.get() && self.menu.insert(&self.update, 1).is_ok() {
                    self.update_shown.set(true);
                }
            }
            None => {
                if self.update_shown.get() && self.menu.remove(&self.update).is_ok() {
                    self.update_shown.set(false);
                }
            }
        }
    }

    /// Latest reviews as (label, link); clicking one opens its review (or the PR).
    pub fn set_recent(&self, lang: &str, items: &[(String, String)]) {
        let _ = self.recent.set_text(text(lang, "recent"));
        while let Ok(Some(_)) = self.recent.remove_at(0) {}
        if items.is_empty() {
            if let Ok(i) = MenuItem::with_id(
                &self.app,
                "recent-none",
                text(lang, "none"),
                false,
                None::<&str>,
            ) {
                let _ = self.recent.append(&i);
            }
            return;
        }
        for (i, (label, url)) in items.iter().enumerate() {
            let label: String = label.chars().take(60).collect();
            if let Ok(item) = MenuItem::with_id(
                &self.app,
                format!("recent:{i}:{url}"),
                label,
                true,
                None::<&str>,
            ) {
                let _ = self.recent.append(&item);
            }
        }
    }

    pub fn update(&self, lang: &str, s: &Status) {
        let (px, w, h) = icon_rgba(s.state);
        let _ = self.icon.set_icon(Some(Image::new_owned(px, w, h)));
        let line = status_line(lang, s);
        let _ = self.icon.set_tooltip(Some(format!("Nod · {line}")));
        let _ = self.status.set_text(line);
        let _ = self.pause.set_text(text(
            lang,
            if s.state == "paused" {
                "resume"
            } else {
                "pause"
            },
        ));
        let _ = self.pause.set_enabled(s.state != "setup");
        let _ = self.open.set_text(text(lang, "open"));
        let _ = self.quit.set_text(text(lang, "quit"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dot_only_when_not_idle() {
        let (idle, w, h) = icon_rgba("idle");
        let (err, ..) = icon_rgba("error");
        assert_eq!(idle.len(), (w * h * 4) as usize);
        assert_ne!(idle, err);
        // the lower-right corner of the error icon is the red dot
        let i = (((h - 12) * w + (w - 12)) * 4) as usize;
        assert_eq!(&err[i..i + 3], &[0xcf, 0x22, 0x2e]);
        assert!(dot("idle").is_none());
    }
}
