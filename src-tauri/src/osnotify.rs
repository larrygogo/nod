//! Desktop notifications whose click opens a link (the pull request or the review).

/// The app id (Windows toast identity, macOS bundle id); Linux notifications go by app name instead.
#[cfg(any(windows, target_os = "macos"))]
pub const APP_ID: &str = "io.github.nod";

fn open(url: &str) {
    if !url.is_empty() {
        let _ = tauri_plugin_opener::open_url(url, None::<&str>);
    }
}

/// Whether this copy runs from a build directory rather than an installation (toasts then borrow PowerShell's
/// identity, because Windows only shows toasts for registered app ids).
#[cfg(windows)]
fn unpackaged() -> bool {
    std::env::current_exe()
        .map(|p| {
            let s = p.to_string_lossy().to_lowercase();
            s.contains("\\target\\debug\\") || s.contains("\\target\\release\\")
        })
        .unwrap_or(true)
}

#[cfg(windows)]
pub fn show(title: &str, body: &str, url: &str) {
    use tauri_winrt_notification::Toast;
    let id = if unpackaged() {
        Toast::POWERSHELL_APP_ID
    } else {
        APP_ID
    };
    let link = url.to_string();
    let r = Toast::new(id)
        .title(title)
        .text1(body)
        .on_activated(move |_| {
            open(&link);
            Ok(())
        })
        .show();
    if let Err(e) = r {
        crate::warn!("notification failed: {e}");
    }
}

#[cfg(target_os = "macos")]
pub fn show(title: &str, body: &str, url: &str) {
    use mac_notification_sys::{
        send_notification, set_application, Notification, NotificationResponse,
    };
    let (title, body, url) = (title.to_string(), body.to_string(), url.to_string());
    std::thread::spawn(move || {
        let _ = set_application(APP_ID);
        // blocks until the notification is clicked or dismissed
        match send_notification(
            &title,
            None,
            &body,
            Some(Notification::new().wait_for_click(true)),
        ) {
            Ok(NotificationResponse::Click) | Ok(NotificationResponse::ActionButton(_)) => {
                open(&url)
            }
            Ok(_) => {}
            Err(e) => crate::warn!("notification failed: {e}"),
        }
    });
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn show(title: &str, body: &str, url: &str) {
    let (title, body, url) = (title.to_string(), body.to_string(), url.to_string());
    std::thread::spawn(move || {
        let r = notify_rust::Notification::new()
            .appname("nod")
            .summary(&title)
            .body(&body)
            .action("default", "Open")
            .show();
        match r {
            Ok(h) => h.wait_for_action(|a| {
                if a == "default" {
                    open(&url)
                }
            }),
            Err(e) => crate::warn!("notification failed: {e}"),
        }
    });
}
