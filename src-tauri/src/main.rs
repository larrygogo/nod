//! nod: review the GitHub pull requests you are asked to review — on your own machine, with Claude Code.
//!
//! A desktop app: the window and tray run in this process; reviewing runs in a child process (see `worker`).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod config;
mod daemon;
mod doctor;
mod engine;
#[cfg(test)]
mod engine_tests;
mod forge;
mod i18n;
mod legacy;
mod migrate;
mod notify;
mod osnotify;
mod paths;
mod plugins;
mod rename;
mod review;
mod reviewer;
mod secrets;
mod supervisor;
mod tray;
mod update;
mod util;
mod worker;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // The internal child processes are decided before anything desktop-related starts (single instance, tray…).
    if args
        .first()
        .is_some_and(|a| a == worker::ARG_WORKER || a == worker::ARG_DRY_RUN)
    {
        std::process::exit(worker::main(&args));
    }
    app::run(args.iter().any(|a| a == app::ARG_HIDDEN));
}
