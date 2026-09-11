// Windows release 构建时不弹出附带的控制台窗口（macOS/Linux 无副作用）。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    mpider_tauri_lib::run();
}
