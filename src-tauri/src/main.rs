// Release builds must not open a console window behind the player.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    scrim_lib::run()
}
