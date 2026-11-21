// made by CasualArclamp
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
//! The desktop program: everything is in the library (`lib.rs`), which the
//! Android app uses too.

fn main() -> eframe::Result {
    decaero_gui::run(
        std::env::args().skip(1).collect(),
        decaero_gui::Platform::default(),
    )
}
