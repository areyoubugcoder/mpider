// Windows 桌面程序不弹附带控制台，避免最小化主界面后控制台抢占微信焦点。
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

fn main() {
    mpider_tauri_lib::run();
}
