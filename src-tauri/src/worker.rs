//! The reviewing side of the app. The window process starts this program again as a child with an internal
//! argument; nothing here is meant to be run by hand.
//!
//! - `--worker`: poll GitHub, review, notify (the former daemon).
//! - `--dry-run-review <pr>`: review one PR without writing to GitHub, print the result as one JSON line.
//!
//! Both exit as soon as stdin closes, i.e. when the window process is gone, so no orphan keeps reviewing.

use std::io::Read;
use std::sync::Arc;

use anyhow::Result;

use crate::config::{self, Config};
use crate::engine::Store;
use crate::forge::resolve;
use crate::{daemon, notify, paths, util};

pub const ARG_WORKER: &str = "--worker";
pub const ARG_DRY_RUN: &str = "--dry-run-review";

/// Exit when the parent closes our stdin, stopping any review subprocess first.
fn exit_with_parent() {
    std::thread::Builder::new()
        .name("parent-watch".into())
        .spawn(|| {
            let mut sink = [0u8; 64];
            let mut stdin = std::io::stdin();
            while matches!(stdin.read(&mut sink), Ok(n) if n > 0) {}
            util::abort_all();
            std::process::exit(0);
        })
        .ok();
}

/// Runs the worker; returns the process exit code.
pub fn main(args: &[String]) -> i32 {
    exit_with_parent();
    let r = match args.first().map(String::as_str) {
        Some(ARG_WORKER) => {
            util::setup_logging(false, true);
            run_worker()
        }
        Some(ARG_DRY_RUN) => {
            util::setup_logging(false, true);
            dry_run(args.get(1).map(String::as_str).unwrap_or_default())
        }
        _ => return 2,
    };
    match r {
        Ok(()) => 0,
        Err(e) => {
            crate::error!("{e:#}");
            daemon::write_health("setup", Some(&format!("{e:#}")));
            eprintln!("{e:#}");
            1
        }
    }
}

fn run_worker() -> Result<()> {
    let mut cfg = config::load()?;
    // Development aid (see CONTRIBUTING.md): review but never write to GitHub or send notifications.
    cfg.dry_run = std::env::var("NOD_DRY_RUN").is_ok_and(|v| v == "1");
    daemon::run(cfg)
}

/// Review one PR as the worker would, but write nothing to GitHub and keep the result apart from the real state.
fn dry_run(pr: &str) -> Result<()> {
    let mut cfg: Config = config::load()?;
    cfg.dry_run = true;
    let (acc, repo, n) = resolve(&cfg, pr)?;
    let mut engine = daemon::build_engine(
        cfg,
        &acc,
        Store::open(Some(paths::data_dir().join("dry-run").join("state.json")))?,
    )?;
    engine.notifier = Arc::new(notify::NoNotify);
    let forge = engine.forge.clone();
    let p = forge.fetch_pr(&repo, n)?;
    engine.store.lock().prs.remove(&p.key());
    engine.review_one(&p);
    engine.store.save();
    let e = engine.store.get(&p.key()).unwrap_or_default();
    println!(
        "{}",
        serde_json::json!({"type": "result", "key": p.key(), "entry": e})
    );
    Ok(())
}
