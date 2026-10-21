// Keep the console window off release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // A second, elevated Dusk started to install a driver does only that.
    if let Some(code) = dusk_lib::run_helper_if_asked() {
        std::process::exit(code);
    }
    // A Dusk run by the host to resize the screen around a stream does only
    // that, and opens nothing.
    if let Some(code) = dusk_lib::run_screen_hook_if_asked() {
        std::process::exit(code);
    }
    // `dusk setup` and `dusk status` need no window, and must not create one:
    // the machines that want them have no display to put it on.
    if let Some(code) = dusk_lib::run_cli_if_asked() {
        std::process::exit(code);
    }
    dusk_lib::run()
}
