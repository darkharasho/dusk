// Keep the console window off release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // A second, elevated Dusk started to install a driver does only that.
    if let Some(code) = dusk_lib::run_helper_if_asked() {
        std::process::exit(code);
    }
    dusk_lib::run()
}
