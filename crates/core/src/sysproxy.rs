//! 系统代理开关（服务层）—— 纯命令构造 + **Drop 保证复位**的 RAII 守卫。
//!
//! 安全红线：设的是**全机系统代理**，若进程崩溃/异常退出不还原，会导致全机断网。
//! 因此收尾复位做成 [`SystemProxyGuard`]：只要守卫被 Drop（正常退出或 panic 栈展开），
//! 就**无条件**执行"关代理"命令。真正的平台调用抽象成 [`ProxyApplier`]，便于注入假实现
//! 单测、也便于后续换成 `sysproxy` crate / Windows 注册表实现。
//!
//! **两道兜底**（2026-09-11，起因：mac 上直接 ⌘Q 退出 App，Tauri 走进程退出、主循环里的守卫来不及析构，
//! 系统代理留在一个已无人监听的端口上，全机浏览器断网）：
//! 1. 每个活着的守卫都在进程级登记表 [`force_restore_all`] 里挂一个弱引用；Tauri 的退出事件
//!    （`RunEvent::ExitRequested` / `Exit`）调它把所有仍激活的守卫当场复位（幂等，之后 Drop 不重复）。
//! 2. 应用启动时 [`is_stale_loopback_proxy`] 检查系统代理：开着、指向回环地址、端口无人监听 → 判为上次残留，
//!    [`disable_system_proxy`] 关掉并提示用户（`kill -9` / 断电 / 崩溃这类连退出事件都没有的情况靠它）。
//!
//! 命令构造（[`mac_commands`]）语义与 Python 原型对齐
//! （命令构造由本模块单测覆盖）。

use std::sync::{Arc, Mutex, Weak};

use anyhow::{anyhow, Result};

/// `host:port` 形式（对齐 Python `proxy_value`）。
pub fn proxy_value(host: &str, port: u16) -> String {
    format!("{host}:{port}")
}

/// macOS `networksetup` 命令序列。
///
/// - `enable=true`：设置 http/https 代理地址并置为 on。
/// - `enable=false`：只把 http/https 代理状态置为 off（**不**重设地址）。
pub fn mac_commands(enable: bool, host: &str, port: u16, service: &str) -> Vec<Vec<String>> {
    let p = port.to_string();
    let c = |args: &[&str]| args.iter().map(|s| s.to_string()).collect::<Vec<String>>();
    if enable {
        vec![
            c(&["networksetup", "-setwebproxy", service, host, &p]),
            c(&["networksetup", "-setsecurewebproxy", service, host, &p]),
            c(&["networksetup", "-setwebproxystate", service, "on"]),
            c(&["networksetup", "-setsecurewebproxystate", service, "on"]),
        ]
    } else {
        vec![
            c(&["networksetup", "-setwebproxystate", service, "off"]),
            c(&["networksetup", "-setsecurewebproxystate", service, "off"]),
        ]
    }
}

/// Windows 系统代理注册表命令序列（HKCU\…\Internet Settings）。
///
/// - `enable=true`：设 `ProxyServer=host:port` 并置 `ProxyEnable=1`。
/// - `enable=false`：只把 `ProxyEnable` 置 0（**不**清除地址，对齐 [`mac_commands`] 语义）。
///
/// 注意：仅改注册表不足以让已在跑的 WinINET 客户端（含微信内置浏览器 WeChatAppEx）即时
/// 生效，需再调 WinINET `InternetSetOption` 刷新——由 [`WinInetApplier`] 在跑完命令后完成。
/// 本函数只做纯字符串构造（无 Windows 依赖），便于跨平台单测。
pub fn win_commands(enable: bool, host: &str, port: u16) -> Vec<Vec<String>> {
    const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings";
    let c = |args: &[&str]| args.iter().map(|s| s.to_string()).collect::<Vec<String>>();
    if enable {
        let val = proxy_value(host, port);
        vec![
            c(&[
                "reg",
                "add",
                KEY,
                "/v",
                "ProxyServer",
                "/t",
                "REG_SZ",
                "/d",
                &val,
                "/f",
            ]),
            c(&[
                "reg",
                "add",
                KEY,
                "/v",
                "ProxyEnable",
                "/t",
                "REG_DWORD",
                "/d",
                "1",
                "/f",
            ]),
        ]
    } else {
        vec![c(&[
            "reg",
            "add",
            KEY,
            "/v",
            "ProxyEnable",
            "/t",
            "REG_DWORD",
            "/d",
            "0",
            "/f",
        ])]
    }
}

/// 执行一批命令的抽象。真实实现见 [`CommandApplier`]；单测可注入假实现。
pub trait ProxyApplier {
    fn apply(&self, commands: &[Vec<String>]) -> Result<()>;
}

/// 真实执行器：逐条用 `std::process::Command` 运行（如 macOS `networksetup`）。
pub struct CommandApplier;

impl ProxyApplier for CommandApplier {
    fn apply(&self, commands: &[Vec<String>]) -> Result<()> {
        for cmd in commands {
            let (prog, args) = cmd.split_first().ok_or_else(|| anyhow!("empty command"))?;
            let status = std::process::Command::new(prog).args(args).status()?;
            if !status.success() {
                anyhow::bail!("command failed ({status}): {cmd:?}");
            }
        }
        Ok(())
    }
}

/// Windows 执行器：先跑 `reg` 命令改注册表，再调 WinINET `InternetSetOption` 广播
/// 「代理设置已变更 + 刷新」，让 WinINET 客户端（含微信内置浏览器 WeChatAppEx）即时生效。
///
/// 与 [`CommandApplier`] 语义相同（`apply` 幂等地执行整批命令），只是额外在末尾刷新 WinINET，
/// 因此可直接塞进 [`SystemProxyGuard`]：enable/disable 两批 reg 命令跑完都会触发一次刷新。
#[cfg(windows)]
pub struct WinInetApplier;

#[cfg(windows)]
impl ProxyApplier for WinInetApplier {
    fn apply(&self, commands: &[Vec<String>]) -> Result<()> {
        // 复用命令执行器跑 reg（失败即向上报错，不会走到刷新）。
        CommandApplier.apply(commands)?;
        // 广播设置变更并刷新：让已在跑的 WinINET 进程重读代理配置。
        // INTERNET_OPTION_SETTINGS_CHANGED=39, INTERNET_OPTION_REFRESH=37。
        unsafe {
            use windows::Win32::Networking::WinInet::{
                InternetSetOptionW, INTERNET_OPTION_REFRESH, INTERNET_OPTION_SETTINGS_CHANGED,
            };
            let _ = InternetSetOptionW(None, INTERNET_OPTION_SETTINGS_CHANGED, None, 0);
            let _ = InternetSetOptionW(None, INTERNET_OPTION_REFRESH, None, 0);
        }
        Ok(())
    }
}

/// 守卫内部状态：守卫本体与进程级登记表共享（登记表只持弱引用，守卫 Drop 后自动失效）。
struct GuardInner<A: ProxyApplier> {
    applier: A,
    disable_commands: Vec<Vec<String>>,
    active: bool,
}

/// 「当场复位」抽象：登记表里存的是这个 trait 对象，抹掉 `A` 的泛型。
trait Restorable: Send {
    /// 幂等复位：激活中才执行 disable 命令，之后标记为非激活。
    fn restore_now(&mut self) -> Result<()>;
    /// 是否仍激活（还没复位过）。
    fn is_active(&self) -> bool;
}

impl<A: ProxyApplier + Send> Restorable for GuardInner<A> {
    fn restore_now(&mut self) -> Result<()> {
        if self.active {
            self.applier.apply(&self.disable_commands)?;
            self.active = false;
        }
        Ok(())
    }
    fn is_active(&self) -> bool {
        self.active
    }
}

/// 进程级登记表：所有活着的守卫的弱引用。退出钩子据此把仍激活的守卫一次性复位。
static ACTIVE_GUARDS: Mutex<Vec<Weak<Mutex<dyn Restorable + Send>>>> = Mutex::new(Vec::new());

/// **退出兜底**：把登记表里仍激活的系统代理守卫当场复位（幂等；复位过的守卫之后 Drop 不再重复）。
/// 返回实际复位的守卫数。Tauri 退出事件 / 任何「进程要没了」的路径都可以调；没有守卫时什么也不做。
pub fn force_restore_all() -> usize {
    let mut list = ACTIVE_GUARDS.lock().unwrap_or_else(|e| e.into_inner());
    let mut restored = 0;
    list.retain(|w| {
        let Some(inner) = w.upgrade() else {
            return false; // 守卫已 Drop，清掉登记
        };
        let mut g = inner.lock().unwrap_or_else(|e| e.into_inner());
        // 只数「这次真的关了代理」的守卫；失败也不 panic（退出路径上不能炸）。
        if g.is_active() && g.restore_now().is_ok() {
            restored += 1;
        }
        true
    });
    restored
}

/// 系统代理 RAII 守卫：构造时应用 enable 命令，Drop 时**无条件**应用 disable 命令。
///
/// 用法（编排层）：起 MITM + 设系统代理后持有本守卫；任务结束/进程退出/panic 时守卫 Drop，
/// 自动关代理。平台差异只体现在传入的命令序列上，守卫本身跨平台。构造即登记到进程级登记表，
/// 供 [`force_restore_all`] 在退出事件里兜底复位。
pub struct SystemProxyGuard<A: ProxyApplier + Send + 'static> {
    inner: Arc<Mutex<GuardInner<A>>>,
}

impl<A: ProxyApplier + Send + 'static> SystemProxyGuard<A> {
    /// 应用 `enable_commands` 设置系统代理，持有 `disable_commands` 以便 Drop 复位。
    pub fn set(
        applier: A,
        enable_commands: Vec<Vec<String>>,
        disable_commands: Vec<Vec<String>>,
    ) -> Result<Self> {
        applier.apply(&enable_commands)?;
        let inner = Arc::new(Mutex::new(GuardInner {
            applier,
            disable_commands,
            active: true,
        }));
        let dynamic: Arc<Mutex<dyn Restorable + Send>> = inner.clone();
        let mut list = ACTIVE_GUARDS.lock().unwrap_or_else(|e| e.into_inner());
        list.retain(|w| w.strong_count() > 0);
        list.push(Arc::downgrade(&dynamic));
        Ok(Self { inner })
    }

    /// macOS 便捷构造：按 host/port/service 生成 enable/disable 命令。
    pub fn set_mac(applier: A, host: &str, port: u16, service: &str) -> Result<Self> {
        Self::set(
            applier,
            mac_commands(true, host, port, service),
            mac_commands(false, host, port, service),
        )
    }

    /// 手动提前复位（幂等）；调用后 Drop 不再重复关代理。
    pub fn restore(&mut self) -> Result<()> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .restore_now()
    }
}

impl<A: ProxyApplier + Send + 'static> Drop for SystemProxyGuard<A> {
    fn drop(&mut self) {
        // 无条件复位：正常退出 / panic 栈展开都要关代理，避免全机断网（已复位过的不重复）。
        let _ = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .restore_now();
    }
}

/// 回环地址上的某端口现在有没有进程在听（300ms 内能建 TCP 连接即有）。
pub fn loopback_port_listening(port: u16) -> bool {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(300)).is_ok()
}

/// 判断当前系统代理是不是**上次运行残留**：开着、指向回环地址（`127.0.0.1` / `localhost` / `::1`）、
/// 且该端口无人监听 —— 这就是「App 没正常收尾、代理端口随进程一起没了」的现场。命中返回端口号。
///
/// 指向别的代理（如用户自己的 127.0.0.1:7890 且在跑）不会误判：端口有人听就不算残留。
pub fn is_stale_loopback_proxy(enabled: bool, endpoint: Option<&str>) -> Option<u16> {
    if !enabled {
        return None;
    }
    let ep = endpoint?.trim();
    let (host, port) = ep.rsplit_once(':')?;
    let host = host.trim_matches(|c| c == '[' || c == ']');
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false);
    if !loopback {
        return None;
    }
    let port: u16 = port.trim().parse().ok()?;
    if loopback_port_listening(port) {
        None
    } else {
        Some(port)
    }
}

/// 直接关掉系统代理（不经守卫；启动自检发现残留时用）。`service` 只在 macOS 有意义。
pub fn disable_system_proxy(service: &str) -> Result<()> {
    #[cfg(windows)]
    {
        let _ = service;
        WinInetApplier.apply(&win_commands(false, "127.0.0.1", 0))
    }
    #[cfg(not(windows))]
    {
        CommandApplier.apply(&mac_commands(false, "127.0.0.1", 0, service))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct FakeApplier {
        // 记录每次 apply 的命令批次。
        calls: Arc<Mutex<Vec<Vec<Vec<String>>>>>,
    }
    impl ProxyApplier for FakeApplier {
        fn apply(&self, commands: &[Vec<String>]) -> Result<()> {
            self.calls.lock().unwrap().push(commands.to_vec());
            Ok(())
        }
    }

    fn c(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn test_sysproxy_commands() {
        assert_eq!(proxy_value("127.0.0.1", 8080), "127.0.0.1:8080");
        let cmds = mac_commands(true, "127.0.0.1", 8080, "Wi-Fi");
        assert!(cmds.contains(&c(&[
            "networksetup",
            "-setwebproxy",
            "Wi-Fi",
            "127.0.0.1",
            "8080"
        ])));
        assert!(cmds.contains(&c(&["networksetup", "-setwebproxystate", "Wi-Fi", "on"])));
        let off = mac_commands(false, "127.0.0.1", 8080, "Wi-Fi");
        assert!(off.contains(&c(&["networksetup", "-setwebproxystate", "Wi-Fi", "off"])));
        // 关代理不重设地址
        assert!(off
            .iter()
            .all(|cmd| cmd.get(1).map(String::as_str) != Some("-setwebproxy")));
    }

    #[test]
    fn test_win_commands() {
        let on = win_commands(true, "127.0.0.1", 8888);
        assert!(on.iter().any(|cmd| cmd.contains(&"ProxyServer".to_string())
            && cmd.contains(&"127.0.0.1:8888".to_string())));
        assert!(on
            .iter()
            .any(|cmd| cmd.contains(&"ProxyEnable".to_string()) && cmd.contains(&"1".to_string())));
        let off = win_commands(false, "127.0.0.1", 8888);
        // 关代理只置 ProxyEnable=0，不重设 ProxyServer 地址（对齐 mac 语义）
        assert!(off
            .iter()
            .any(|cmd| cmd.contains(&"ProxyEnable".to_string()) && cmd.contains(&"0".to_string())));
        assert!(off
            .iter()
            .all(|cmd| !cmd.contains(&"ProxyServer".to_string())));
    }

    #[test]
    fn test_guard_disables_on_drop() {
        let calls: Arc<Mutex<Vec<Vec<Vec<String>>>>> = Arc::new(Mutex::new(Vec::new()));
        let applier = FakeApplier {
            calls: calls.clone(),
        };
        {
            let _g = SystemProxyGuard::set_mac(applier, "127.0.0.1", 8080, "Wi-Fi").unwrap();
            // set 时应用了 enable（第 1 批）
            assert_eq!(calls.lock().unwrap().len(), 1);
        } // 作用域结束 → Drop → 应用 disable（第 2 批）
        let recorded = calls.lock().unwrap();
        assert_eq!(recorded.len(), 2, "Drop 必须触发一次关代理");
        assert!(recorded[1].contains(&c(&["networksetup", "-setwebproxystate", "Wi-Fi", "off"])));
    }

    /// 退出兜底：登记表里仍激活的守卫被 force_restore_all 当场复位一次，之后 Drop 不再重复；
    /// 已手动复位 / 已 Drop 的守卫不计数。
    #[test]
    fn test_force_restore_all_restores_live_guards_once() {
        let calls: Arc<Mutex<Vec<Vec<Vec<String>>>>> = Arc::new(Mutex::new(Vec::new()));
        let applier = FakeApplier {
            calls: calls.clone(),
        };
        let g = SystemProxyGuard::set_mac(applier.clone(), "127.0.0.1", 8080, "Wi-Fi").unwrap();
        let mut done = SystemProxyGuard::set_mac(applier, "127.0.0.1", 8081, "Wi-Fi").unwrap();
        done.restore().unwrap(); // 已手动复位，不该再被数
        assert_eq!(calls.lock().unwrap().len(), 3); // enable ×2 + disable ×1
        assert_eq!(force_restore_all(), 1, "只有仍激活的那个被复位");
        assert_eq!(calls.lock().unwrap().len(), 4);
        assert_eq!(force_restore_all(), 0, "再调一次幂等");
        drop(g);
        assert_eq!(
            calls.lock().unwrap().len(),
            4,
            "复位过的守卫 Drop 不重复关代理"
        );
        assert_eq!(force_restore_all(), 0);
    }

    #[test]
    fn test_stale_loopback_proxy_detection() {
        // 端口有人听 → 不是残留；关掉监听 → 残留。
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let ep = format!("127.0.0.1:{port}");
        assert_eq!(is_stale_loopback_proxy(true, Some(&ep)), None);
        drop(l);
        assert_eq!(is_stale_loopback_proxy(true, Some(&ep)), Some(port));
        // 没开 / 没地址 / 非回环 / 端口不合法都不算残留。
        assert_eq!(is_stale_loopback_proxy(false, Some(&ep)), None);
        assert_eq!(is_stale_loopback_proxy(true, None), None);
        assert_eq!(is_stale_loopback_proxy(true, Some("10.0.0.2:8080")), None);
        assert_eq!(is_stale_loopback_proxy(true, Some("127.0.0.1:abc")), None);
        assert_eq!(is_stale_loopback_proxy(true, Some("localhost")), None);
    }

    #[test]
    fn test_guard_restore_then_no_double_disable() {
        let calls: Arc<Mutex<Vec<Vec<Vec<String>>>>> = Arc::new(Mutex::new(Vec::new()));
        let applier = FakeApplier {
            calls: calls.clone(),
        };
        let mut g = SystemProxyGuard::set_mac(applier, "127.0.0.1", 8080, "Wi-Fi").unwrap();
        g.restore().unwrap(); // 手动复位
        drop(g); // Drop 不应再关一次
        assert_eq!(
            calls.lock().unwrap().len(),
            2,
            "restore 后 Drop 不重复关代理"
        );
    }
}
