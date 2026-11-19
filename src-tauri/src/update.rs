//! Updates: the newest release on GitHub (its `latest.json`), checked at start and every few hours. Nothing is
//! installed until the user asks (window or tray): an update restarts Nod, which would cut off a running review.
//! Packages are verified against the public key in tauri.conf.json before they are installed.

use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tauri::AppHandle;
use tauri_plugin_updater::UpdaterExt;

use crate::util::now;

const EVERY: Duration = Duration::from_secs(6 * 3600);

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct State {
    /// This build's version.
    pub current: String,
    /// A newer version on GitHub, if one was found.
    pub available: Option<String>,
    /// Its release notes (from the CHANGELOG).
    pub notes: Option<String>,
    /// Last successful check (unix seconds; 0 = not yet).
    pub checked_at: f64,
    /// The last check's error, if it failed (offline, GitHub unreachable).
    pub error: Option<String>,
    /// Downloading / installing right now.
    pub installing: bool,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(|| State {
        current: env!("CARGO_PKG_VERSION").into(),
        ..Default::default()
    }))
}

pub fn state() -> State {
    with(|s| s.clone())
}

/// Ask GitHub for the newest release.
pub async fn check(app: &AppHandle) -> State {
    let res = async {
        let u = app.updater().map_err(|e| e.to_string())?;
        u.check().await.map_err(|e| e.to_string())
    }
    .await;
    with(|s| {
        match res {
            Ok(found) => {
                s.available = found.as_ref().map(|u| u.version.clone());
                s.notes = found.and_then(|u| u.body);
                s.checked_at = now();
                s.error = None;
            }
            Err(e) => {
                crate::warn!("update check failed: {e}");
                s.error = Some(e);
            }
        }
        s.clone()
    })
}

/// Download, verify and install the newest release, then restart into it.
pub async fn install(app: &AppHandle) -> Result<(), String> {
    if with(|s| std::mem::replace(&mut s.installing, true)) {
        return Ok(()); // already on it
    }
    let stopped = std::sync::atomic::AtomicBool::new(false);
    let res = async {
        let u = app.updater().map_err(|e| e.to_string())?;
        let Some(up) = u.check().await.map_err(|e| e.to_string())? else {
            return Err("no update".to_string());
        };
        crate::info!("downloading Nod {}", up.version);
        let bytes = up
            .download(|_, _| {}, || {})
            .await
            .map_err(|e| e.to_string())?;
        // The worker is a second nod.exe: stop it before the installer replaces the program (on Windows the
        // installer also ends this process). Its running reviews are re-queued when the new version starts.
        crate::app::stop_worker(app);
        stopped.store(true, std::sync::atomic::Ordering::SeqCst);
        crate::info!("installing Nod {}", up.version);
        up.install(bytes).map_err(|e| e.to_string())
    }
    .await;
    match res {
        Ok(()) => {
            crate::info!("update installed; restarting");
            app.restart();
        }
        Err(e) => {
            crate::warn!("update failed: {e}");
            if stopped.load(std::sync::atomic::Ordering::SeqCst) {
                crate::app::start_worker(app); // keep reviewing on the current version
            }
            with(|s| s.installing = false);
            Err(e)
        }
    }
}

/// Check shortly after start, then every few hours.
pub fn start(app: AppHandle) {
    std::thread::Builder::new()
        .name("update".into())
        .spawn(move || {
            std::thread::sleep(Duration::from_secs(20));
            loop {
                let s = tauri::async_runtime::block_on(check(&app));
                if let Some(v) = &s.available {
                    crate::info!("Nod {v} is available");
                }
                std::thread::sleep(EVERY);
            }
        })
        .expect("update thread");
}
