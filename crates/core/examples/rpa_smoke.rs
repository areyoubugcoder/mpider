//! Windows 纯 Rust RPA 冒烟。必须在**已登录微信、文件传输助手为独立置顶窗口**的交互桌面（session 1）跑。
//!   cargo run -p mpider-core --example rpa_smoke -- check                  # 只自检（窗口 / 标定状态，不点击）
//!   cargo run -p mpider-core --example rpa_smoke -- mark x0 y0 x1 y1       # 命令行标定：按屏幕物理像素矩形落盘点位缓存
//!   cargo run -p mpider-core --example rpa_smoke -- click                  # 测试点击：置顶→按标定点位点一次→看是否拉起浏览器（不关窗）
//!   cargo run -p mpider-core --example rpa_smoke -- open [url]             # 正式链路：清残留→置顶→点标定点位→Ctrl+F5→关闭
//! 产品里标定走 GUI「框选种子」；`mark` 只是脱离 GUI 时的脚手架（坐标可从 `check` 的文件助手矩形估算）。
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("open");
    let ctl = mpider_core::rpa::get_controller(true);
    match mode {
        "check" => {
            let sc = ctl.self_check();
            println!("self_check: {sc:?}");
        }
        "mark" => {
            let n: Vec<i32> = args[2..]
                .iter()
                .take(4)
                .filter_map(|s| s.parse().ok())
                .collect();
            if n.len() != 4 {
                eprintln!("用法：rpa_smoke mark x0 y0 x1 y1（屏幕物理像素）");
                std::process::exit(2);
            }
            match ctl.prepare_pick() {
                Ok(rect) => println!("文件助手已置顶，矩形 {rect:?}"),
                Err(e) => {
                    eprintln!("prepare_pick 失败：{e}");
                    std::process::exit(1);
                }
            }
            match ctl.mark_seed((n[0], n[1], n[2], n[3])) {
                Ok(m) => println!("mark_seed ok :: {m:?}"),
                Err(e) => eprintln!("mark_seed 失败：{e}"),
            }
            println!("after: {:?}", ctl.self_check());
        }
        "click" => {
            let tc = ctl.test_click();
            println!("test_click: {tc:?}");
        }
        _ => {
            let url = args
                .get(2)
                .cloned()
                .unwrap_or_else(|| "https://mp.weixin.qq.com/s/cOQPRdgT5_DYyd1XOUabrQ".to_string());
            let (ok, msg) = ctl.open_seed(&url);
            println!("open_seed ok={ok} :: {msg}");
            std::thread::sleep(std::time::Duration::from_secs(3));
            let (ok2, msg2) = ctl.close_browser();
            println!("close_browser ok={ok2} :: {msg2}");
        }
    }
}
