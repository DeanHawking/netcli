//! Thin binary entry point. Application composition lives in `app.rs`.
mod app;
mod commands;
#[path = "core/mod.rs"]
mod cli_core;

fn main() {
    app::run();
}
