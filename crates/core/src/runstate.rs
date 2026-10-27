//! 运行期全局状态 —— 抓包 MITM 代理当前是否在跑（及监听地址）、整机限流退避、
//! 定时巡检进度、以及「整链正在运行」的进程级互斥。
//!
//! 为什么是进程级全局：抓包代理由一次 run 的 `capture_start/stop` 临时起停，
//! 但查询方（GUI 底部状态栏的 `system_status`）与触发方（前端按钮 / 托盘菜单）
//! 不在同一条调用链上，只有进程级单点才能让两边看到同一份真相。
//! 单进程模型（迁移方案 §1）下每次只会有一个 CaptureProxy，Option 即可。
//!
//! 限流退避（2026-09-05，巡检需求引入）：同一台机、同一个微信号、同一个出口 IP，微信的限流是
//! **整机级**的，所以退避状态也是进程级单点——不管是手动任务还是巡检批次撞到 429 / 验证页，
//! 都写这里；调度主循环在领下一个单元前先看它，退避没结束就等。指数退避：5 分钟起、每次翻倍、
//! 封顶 60 分钟；一个批次干净跑完（没有限流信号）即把等级归零。
//! 退避状态（截止时刻 / 等级 / 原因）每次变化都持久化到 `config.cooldown_state`，启动时
//! [`cooldown_restore`] 恢复——被封后重启 App，内存里的 6 小时退避不能随之丢失，
//! 一点「开始轮询」就会用被封的号继续打。
//!
//! 整链互斥：代理 / 系统代理 / 微信窗口 / 接力队列都是物理独占资源，`run_once_real` 与
//! `run_loop_real` 进入前都要先拿到 [`RunGuard`]，拿不到说明另一条整链在跑。

use std::sync::Mutex;

use serde::Serialize;

use crate::model::now;

static CAPTURE_ADDR: Mutex<Option<String>> = Mutex::new(None);

/// 抓包代理起停时写入：`Some("127.0.0.1:port")` = 在跑；`None` = 已停。
pub fn set_capture_addr(addr: Option<String>) {
    *CAPTURE_ADDR.lock().unwrap() = addr;
}

/// 当前抓包代理监听地址；`None` = 未在运行。
pub fn capture_addr() -> Option<String> {
    CAPTURE_ADDR.lock().unwrap().clone()
}

// -----------------------------------------------------------------------------
// 整链互斥
// -----------------------------------------------------------------------------

static RUN_BUSY: Mutex<bool> = Mutex::new(false);

/// 「整链正在运行」的守卫：Drop 即释放。
pub struct RunGuard(());

impl Drop for RunGuard {
    fn drop(&mut self) {
        *RUN_BUSY.lock().unwrap() = false;
    }
}

/// 尝试占住整链（一次运行 / 长驻轮询）；已被占返回 `None`。
pub fn try_acquire_run() -> Option<RunGuard> {
    let mut busy = RUN_BUSY.lock().unwrap();
    if *busy {
        return None;
    }
    *busy = true;
    Some(RunGuard(()))
}

/// 整链是否在运行（GUI 状态 / 自更新空闲判定用）。
pub fn run_busy() -> bool {
    *RUN_BUSY.lock().unwrap()
}

static UNIT_BUSY: Mutex<bool> = Mutex::new(false);

/// 「一个执行单元正在处理」的守卫（手动任务 / 巡检批次 / 单号手动重试）：Drop 即释放。
/// 与整链守卫 [`RunGuard`] 独立——主循环长驻时一直持有 `RunGuard`，但批次之间空闲会放掉本守卫，
/// 单号「重新巡检」只需抢到本守卫即可在空闲期立即执行。
pub struct UnitGuard(());

impl Drop for UnitGuard {
    fn drop(&mut self) {
        *UNIT_BUSY.lock().unwrap() = false;
    }
}

/// 尝试占住执行单元；已有单元在处理返回 `None`。
pub fn try_acquire_unit() -> Option<UnitGuard> {
    let mut busy = UNIT_BUSY.lock().unwrap();
    if *busy {
        return None;
    }
    *busy = true;
    Some(UnitGuard(()))
}

/// 是否有执行单元正在处理（GUI 据此置灰「重新巡检」）。
pub fn unit_busy() -> bool {
    *UNIT_BUSY.lock().unwrap()
}

// -----------------------------------------------------------------------------
// 整机限流退避
// -----------------------------------------------------------------------------

/// 退避起步时长（秒）：5 分钟。
pub const COOLDOWN_BASE_SECS: u64 = 5 * 60;
/// 退避封顶（秒）：60 分钟。
pub const COOLDOWN_MAX_SECS: u64 = 60 * 60;
/// **账号级封禁**（`getmsg` 回 `ret=-6/-12`，微信已把这个微信号识别为异常）的退避起步：6 小时。
/// 与 IP 级限流（429 / 5xx / 验证页）的 5 分钟阶梯分开——公开资料与实测都表明
/// 这种限制以「天」计，5 分钟后再打只会延长封禁。
pub const BLOCK_BASE_SECS: u64 = 6 * 60 * 60;
/// 账号级封禁退避封顶：24 小时。
pub const BLOCK_MAX_SECS: u64 = 24 * 60 * 60;

/// 退避状态快照（GUI 展示 / 日志用）。
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct CooldownSnapshot {
    /// 当前生效的退避截止时刻（epoch 秒）= `until_ip` 与 `until_account` 的较大者；`None` = 不在退避中。
    pub until: Option<f64>,
    /// IP 级限流（429 / 5xx / 接口回网页，整机）的截止时刻。
    #[serde(default)]
    pub until_ip: Option<f64>,
    /// 账号级封禁（`ret=-6`，按激活微信号记）的截止时刻。切换微信号时由 [`sync_active_block`] 对齐。
    #[serde(default)]
    pub until_account: Option<f64>,
    /// 当前退避等级（连续触发次数；干净跑完一个批次归零）。
    pub level: u32,
    /// 最近一次触发原因。
    pub reason: String,
    /// 累计触发次数（进程内）。
    pub hits: u32,
    /// 当前生效的退避种类：`ip` / `account` / 空（两类各自独立计时，这里是 `until` 对应的那一类）。
    #[serde(default)]
    pub kind: String,
}

impl CooldownSnapshot {
    /// 把过期的截止时刻清掉，再按两类截止重算 `until` / `kind`。返回是否有过期项被清。
    fn refresh(&mut self) -> bool {
        let t = now();
        let mut expired = false;
        if self.until_ip.is_some_and(|u| u <= t) {
            self.until_ip = None;
            expired = true;
        }
        if self.until_account.is_some_and(|u| u <= t) {
            self.until_account = None;
            expired = true;
        }
        let (until, kind) = match (self.until_ip, self.until_account) {
            (Some(i), Some(a)) if a >= i => (Some(a), "account"),
            (Some(i), _) => (Some(i), "ip"),
            (None, Some(a)) => (Some(a), "account"),
            (None, None) => (None, ""),
        };
        self.until = until;
        self.kind = kind.to_string();
        expired
    }
}

static COOLDOWN: Mutex<CooldownSnapshot> = Mutex::new(CooldownSnapshot {
    until: None,
    until_ip: None,
    until_account: None,
    level: 0,
    reason: String::new(),
    hits: 0,
    kind: String::new(),
});

/// 单测互斥：退避是进程级全局，会触发 / 断言它的测试要先拿这把锁串行跑（测试默认多线程并行）。
#[cfg(test)]
pub(crate) static COOLDOWN_TEST_LOCK: Mutex<()> = Mutex::new(());

/// 退避时长：`base * 2^(level-1)`，封顶 `COOLDOWN_MAX_SECS`。
pub fn cooldown_secs_for_level(level: u32) -> u64 {
    let shift = level.saturating_sub(1).min(16);
    COOLDOWN_BASE_SECS
        .saturating_mul(1u64 << shift)
        .min(COOLDOWN_MAX_SECS)
}

/// 账号级封禁退避时长：`BLOCK_BASE_SECS * 2^(level-1)`，封顶 `BLOCK_MAX_SECS`。
pub fn block_secs_for_level(level: u32) -> u64 {
    let shift = level.saturating_sub(1).min(16);
    BLOCK_BASE_SECS
        .saturating_mul(1u64 << shift)
        .min(BLOCK_MAX_SECS)
}

/// 触发一次整机退避：等级 +1，按等级算时长，返回本次退避秒数。
/// 已在退避中再触发（并发单元很少见，但可能）只会把截止时刻**往后推**，不会缩短。
pub fn cooldown_trigger(reason: impl Into<String>) -> u64 {
    trigger_with("ip", reason.into(), cooldown_secs_for_level)
}

/// 触发一次**账号级封禁**退避（`ret=-6/-12`）：同一套等级 / 截止时刻，但按小时级阶梯算时长。
/// 退避期间主循环不领任何任务、不做接力换 key（换 key 无用：限制在微信号，不在凭证）。
pub fn block_trigger(reason: impl Into<String>) -> u64 {
    trigger_with("account", reason.into(), block_secs_for_level)
}

/// `kind`：`ip` / `account`，只用于 `cooldown_log` 留档（限流分析页按种类展示）。
fn trigger_with(kind: &str, reason: String, secs_for_level: fn(u32) -> u64) -> u64 {
    let (secs, level) = {
        let mut c = COOLDOWN.lock().unwrap();
        c.level += 1;
        c.hits += 1;
        c.reason = reason.clone();
        let secs = secs_for_level(c.level);
        let until = now() + secs as f64;
        // 只推本类的截止时刻（不缩短）；另一类独立计时，封号期间撞到 IP 限流不会把封号「改名」成 IP 退避。
        let slot = if kind == "account" {
            &mut c.until_account
        } else {
            &mut c.until_ip
        };
        *slot = Some(slot.map_or(until, |u| u.max(until)));
        c.refresh();
        (secs, c.level)
    };
    persist_cooldown_state();
    persist_cooldown_event(kind, level as i64, secs as i64, &reason);
    if kind == "account" {
        alert_push(
            "blocked",
            format!(
                "当前微信号被微信判为异常（ret=-6），退避 {}，期间不发列表请求；换微信号可在「微信号管理」激活另一条",
                fmt_secs_short(secs)
            ),
        );
    }
    if kind == "account" {
        persist_active_block(&reason);
    }
    // 飞书：退避是无人值守时最该有人知道的事（封号尤其）。
    crate::notify::notify(
        crate::notify::Kind::Cooldown,
        format!(
            "{}，整机退避 {}（第 {level} 级），期间不领任务、不做接力\n原因：{}",
            if kind == "account" {
                "微信号被判异常（ret=-6 封号级）"
            } else {
                "撞到限流信号"
            },
            fmt_secs_short(secs),
            crate::applog::redact(&reason)
        ),
    );
    secs
}

/// 秒 → 「6 小时」/「5 分钟」/「45 秒」（通知文案）。
fn fmt_secs_short(secs: u64) -> String {
    if secs >= 3600 && secs.is_multiple_of(3600) {
        format!("{} 小时", secs / 3600)
    } else if secs >= 60 {
        format!("{} 分钟", secs.div_ceil(60))
    } else {
        format!("{secs} 秒")
    }
}

/// 退避事件留档到 `cooldown_log`（限流分析）：走环节日志总线上挂的 store（GUI / 长驻轮询都挂了；
/// 单测没挂就跳过）。触发点分散在编排器 / 巡检 / 手动重试 / GUI 解除，集中在这里记一处即可。
fn persist_cooldown_event(kind: &str, level: i64, secs: i64, reason: &str) {
    if let Some(store) = crate::applog::bus().store() {
        let _ = store.append_cooldown_log(kind, level, secs, &crate::applog::redact(reason));
    }
}

/// 一个批次干净跑完（没有任何限流信号）：等级归零（截止时刻若仍在未来则保留，等它自然过去）。
pub fn cooldown_note_clean() {
    let changed = {
        let mut c = COOLDOWN.lock().unwrap();
        let before = (c.level, c.until);
        c.level = 0;
        c.refresh();
        before != (c.level, c.until)
    };
    if changed {
        persist_cooldown_state();
    }
}

/// 剩余退避秒数；`None` = 不在退避中（过期的截止时刻顺手清掉）。
pub fn cooldown_remaining_secs() -> Option<u64> {
    let (remaining, expired) = {
        let mut c = COOLDOWN.lock().unwrap();
        let expired = c.refresh();
        (c.until.map(|u| (u - now()).ceil().max(0.0) as u64), expired)
    };
    if expired {
        persist_cooldown_state();
    }
    remaining
}

/// 退避状态快照。
pub fn cooldown_snapshot() -> CooldownSnapshot {
    let _ = cooldown_remaining_secs();
    COOLDOWN.lock().unwrap().clone()
}

/// 清空退避（单测 / 用户手动解除）。真有退避在身（截止时刻或等级非零）时留一条 `clear` 记录。
pub fn cooldown_reset() {
    let prev = std::mem::take(&mut *COOLDOWN.lock().unwrap());
    persist_cooldown_state();
    if prev.until_account.is_some() {
        if let Some(store) = crate::applog::bus().store() {
            if let Ok(Some(a)) = store.wx_active() {
                let _ = store.wx_unblock(a.id);
            }
        }
    }
    if prev.until.is_some() || prev.level > 0 {
        persist_cooldown_event(
            "clear",
            0,
            0,
            &format!("手动解除（原第 {} 级：{}）", prev.level, prev.reason),
        );
    }
}

/// 退避状态在 `config` 表里的键：`{"until": 秒|null, "level": n, "reason": "…", "kind": "ip|account"}`。
pub const COOLDOWN_STATE_KEY: &str = "cooldown_state";

/// 封号退避同时写到**激活微信号**的 `blocked_until / blocked_reason`（退避按号记，换号即可继续）。
/// 走环节日志总线上挂的 store；没挂（单测）就跳过。**调用时不能持有 `COOLDOWN` 锁。**
fn persist_active_block(reason: &str) {
    let Some(store) = crate::applog::bus().store() else {
        return;
    };
    let until = COOLDOWN.lock().unwrap().until_account;
    let (Some(until), Ok(Some(active))) = (until, store.wx_active()) else {
        return;
    };
    if let Err(e) = store.wx_set_blocked(active.id, until, &crate::applog::redact(reason)) {
        tracing::warn!(error = %e, "微信号封号状态持久化失败");
    }
}

/// 把全局退避快照里的**账号类**退避与激活微信号对齐：激活号 `blocked_until` 仍在未来 → 全局按它等；
/// 激活号未封 → 清掉账号类退避（IP 类不动）。激活 / 解封 / 启动恢复后调用，主循环无需改判定。
pub fn sync_active_block(store: &crate::store::Store) {
    let active = store.wx_active().ok().flatten();
    let blocked = active
        .as_ref()
        .and_then(|a| a.blocked_until.filter(|u| *u > now()).map(|u| (u, a)));
    let changed = {
        let mut c = COOLDOWN.lock().unwrap();
        let before = c.until_account;
        match blocked {
            Some((until, a)) => {
                c.until_account = Some(until);
                if before != Some(until) {
                    c.reason = a
                        .blocked_reason
                        .clone()
                        .unwrap_or_else(|| "微信号被判异常（ret=-6）".into());
                }
            }
            None => c.until_account = None,
        }
        c.refresh();
        before != c.until_account
    };
    if changed {
        persist_cooldown_state();
    }
}

/// 把当前退避状态写进 `config.cooldown_state`（走环节日志总线上挂的 store；没挂就跳过）。
/// **调用时不能持有 `COOLDOWN` 锁。**
fn persist_cooldown_state() {
    if let Some(store) = crate::applog::bus().store() {
        let snap = COOLDOWN.lock().unwrap().clone();
        if let Err(e) = cooldown_save(&store, &snap) {
            tracing::warn!(error = %e, "退避状态持久化失败");
        }
    }
}

/// 写一份退避状态到 `store`（`hits` 是进程内计数，不存）。
pub fn cooldown_save(store: &crate::store::Store, snap: &CooldownSnapshot) -> anyhow::Result<()> {
    store.set_config(
        COOLDOWN_STATE_KEY,
        &serde_json::json!({
            "until": snap.until,
            "until_ip": snap.until_ip,
            "until_account": snap.until_account,
            "level": snap.level,
            "reason": snap.reason,
            "kind": snap.kind,
        }),
    )
}

/// 启动时从 `config.cooldown_state` 恢复退避：截止时刻仍在未来就照旧等（GUI「解除退避」可清），
/// 已过期只恢复等级（下次触发继续升级，不从头 5 分钟 / 6 小时算）。返回恢复后的快照；
/// 没有记录或记录已无意义（无截止且等级 0）返回 `None`，内存状态不动。
pub fn cooldown_restore(store: &crate::store::Store) -> Option<CooldownSnapshot> {
    let v = store.get_config(COOLDOWN_STATE_KEY).ok().flatten()?;
    let until = v
        .get("until")
        .and_then(serde_json::Value::as_f64)
        .filter(|u| *u > now());
    let level = v
        .get("level")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as u32;
    let reason = v
        .get("reason")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();
    let kind = v
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();
    let get = |k: &str| {
        v.get(k)
            .and_then(serde_json::Value::as_f64)
            .filter(|u| *u > now())
    };
    // 新格式两类各存；老格式只有 `until` + 可选 `kind`（没有 kind 当 ip）。
    let (until_ip, until_account) =
        if v.get("until_ip").is_some() || v.get("until_account").is_some() {
            (get("until_ip"), get("until_account"))
        } else if kind == "account" {
            (None, until)
        } else {
            (until, None)
        };
    if until_ip.is_none() && until_account.is_none() && level == 0 {
        return None;
    }
    let mut c = COOLDOWN.lock().unwrap();
    c.until_ip = until_ip;
    c.until_account = until_account;
    c.level = level;
    c.reason = reason;
    c.refresh();
    Some(c.clone())
}

/// [`cooldown_restore`] + 环节日志：App / 长驻轮询启动时调用一次。有截止时刻就记 warn（GUI 一眼能看到
/// 「上次被封还没过」），只剩等级记 info。
pub fn cooldown_restore_logged(store: &crate::store::Store) -> Option<CooldownSnapshot> {
    let snap = cooldown_restore(store)?;
    use crate::applog::{self, Stage};
    match snap.until {
        Some(_) => applog::warn(
            Stage::App,
            format!(
                "⏸ 恢复上次的整机退避：还剩 {}（第 {} 级），期间不领任务、不做接力；确认换了微信号可在控制面板「解除退避」。原因：{}",
                fmt_secs_short(cooldown_remaining_secs().unwrap_or(0)),
                snap.level,
                applog::redact(&snap.reason)
            ),
        ),
        None => applog::info(
            Stage::App,
            format!(
                "上次退避已过期，保留退避等级 {}（再撞限流按下一级时长退避；干净跑完一批即归零）",
                snap.level
            ),
        ),
    }
    Some(snap)
}

// -----------------------------------------------------------------------------
// 列表接口（getmsg）全局闸门与今日计数
// -----------------------------------------------------------------------------
//
// 实测教训：`getmsg` 的限流按**微信号**计，与走哪个号的凭证无关。此前只有翻页之间
// 有等待，同批 20 个号之间是连发（≈5 请求/秒），失败号又立刻回到下一批，被限后越打越密，
// 最终整个微信号被封（`ret=-6`，天级）。所以节流必须是**进程级单点**：手动任务、巡检、
// 单号手动重试、集中续期后的续采……凡是发 `getmsg` 的路径都先过这道闸门。

/// 上一次 `getmsg` 发出的时刻。`tokio::sync::Mutex`：并发调用方（轮询 + 手动重试）排队等，
/// 而不是各算各的间隔。
static LIST_GATE: std::sync::LazyLock<tokio::sync::Mutex<Option<std::time::Instant>>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(None));

/// 发一次 `getmsg` 前调用：与上一次请求的间隔不足 `[min_ms, max_ms]` 内随机值时等到够为止，
/// 然后把「上一次」更新为现在。返回实际等待的毫秒数（日志用）。`max_ms < min_ms` 按 `min_ms`。
/// 与翻页等待叠加时取的是**自上次请求起**的间隔，所以实际间隔 = max(页间等待, 闸门)。
pub async fn list_gate_wait(min_ms: u64, max_ms: u64) -> u64 {
    let mut last = LIST_GATE.lock().await;
    let gap = std::time::Duration::from_millis(crate::rng::between(min_ms, max_ms.max(min_ms)));
    let mut waited = 0u64;
    if let Some(prev) = *last {
        let elapsed = prev.elapsed();
        if elapsed < gap {
            let remain = gap - elapsed;
            waited = remain.as_millis() as u64;
            tokio::time::sleep(remain).await;
        }
    }
    // 放行时与上一次请求的**实际**间隔（留档 `list_call_log.gap_ms` 用；进程内第一次为 None）。
    *LIST_GATE_LAST_GAP.lock().unwrap() = last.map(|prev| prev.elapsed().as_millis() as u64);
    *last = Some(std::time::Instant::now());
    waited
}

/// 最近一次闸门放行时记下的「与上一次 `getmsg` 的实际间隔」（毫秒）。
static LIST_GATE_LAST_GAP: Mutex<Option<u64>> = Mutex::new(None);

/// 最近一次 [`list_gate_wait`] 放行时与上一次请求的实际间隔（毫秒）；进程内第一次请求为 `None`。
/// 调用方应在 `list_gate_wait` 返回后**立刻**读取（下一位调用方放行后会覆盖）。
pub fn list_gate_last_gap_ms() -> Option<u64> {
    *LIST_GATE_LAST_GAP.lock().unwrap()
}

/// 单测 / 手动重置闸门（下一次请求不等待）。
pub fn list_gate_reset() {
    if let Ok(mut g) = LIST_GATE.try_lock() {
        *g = None;
    }
    *LIST_GATE_LAST_GAP.lock().unwrap() = None;
}

/// 列表请求预算的滚动窗口：24 小时（`list_daily_budget` 按「当前微信号 × 最近 24 小时」计，见
/// [`crate::store::Store::list_calls_window`]；2026-09-09 三个微信号都在累计 206–224 次 `getmsg` 处被 `ret=-6`）。
pub const LIST_BUDGET_WINDOW_SECS: i64 = 86_400;

/// 当前微信号近 24 小时 `getmsg` 计数（进程内镜像；权威值在 `list_call_log`，每次留档后由 `collector`
/// 重新数一遍回写到这里供 GUI 展示）与每号预算（0 = 不限）。
static LIST_CALLS: Mutex<(i64, i64)> = Mutex::new((0, 0));

/// 回写「当前微信号近 24 小时已发次数」。
pub fn set_list_calls_24h(n: i64) {
    LIST_CALLS.lock().unwrap().0 = n.max(0);
}

/// 设置每号预算（轮询启动时按运行配置设；0 = 不限）。
pub fn set_list_daily_budget(b: i64) {
    LIST_CALLS.lock().unwrap().1 = b.max(0);
}

/// （当前号近 24 小时已发, 每号预算）。
pub fn list_calls_snapshot() -> (i64, i64) {
    *LIST_CALLS.lock().unwrap()
}

/// 预算占满时的暂停信息：已用 / 预算 / 最早可恢复时刻（窗口内最早一次请求滑出 24 小时的时刻）。
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BudgetHold {
    pub used: i64,
    pub budget: i64,
    pub resume_at: crate::model::Epoch,
}

/// 查当前微信号近 24 小时的 `getmsg` 计数并回写镜像；达到预算返回 `Some(hold)`（`budget <= 0` = 不限）。
/// 每个执行单元（手动任务 / 巡检批次 / 手动重试）开始前调用——单元内部不再逐请求检查，
/// 所以一批最多超出一批的号数（≤ 10），默认预算 180 已为此留出到实测 206 的余量。
/// 没抓过凭证（不知道当前号）时按窗口内全部请求数计。
pub fn list_budget_check(store: &crate::store::Store, budget: i64) -> Option<BudgetHold> {
    let uin_hash = store.current_uin_hash().ok().flatten();
    let (used, oldest) = store
        .list_calls_window(uin_hash.as_deref(), LIST_BUDGET_WINDOW_SECS)
        .unwrap_or((0, None));
    set_list_calls_24h(used);
    set_list_daily_budget(budget);
    if budget > 0 && used >= budget {
        let resume_at = oldest.unwrap_or_else(now) + LIST_BUDGET_WINDOW_SECS as f64;
        alert_push(
            "budget",
            format!(
                "当前微信号近 24 小时列表请求已达预算（{used}/{budget}），采集已暂停，{} 腾出额度后自动继续",
                crate::applog::format_local(resume_at)
            ),
        );
        Some(BudgetHold {
            used,
            budget,
            resume_at,
        })
    } else {
        None
    }
}

// -----------------------------------------------------------------------------
// 全局提醒（任何页面都要看到的一条：预算达到上限 / 微信号受限 / 历史任务暂停）
// -----------------------------------------------------------------------------

/// 提醒状态：下一个 id、最近一条未确认的提醒、同文案最近一次推送时刻（去重用）。
struct AlertState {
    next_id: i64,
    latest: Option<crate::model::Alert>,
    recent: Vec<(String, f64)>,
}

static ALERT: Mutex<AlertState> = Mutex::new(AlertState {
    next_id: 1,
    latest: None,
    recent: Vec::new(),
});

/// 同文案在这段时间内不重复推（秒）。
pub const ALERT_DEDUP_SECS: f64 = 600.0;

/// 推一条全局提醒（覆盖上一条未确认的）。同文案 [`ALERT_DEDUP_SECS`] 内重复推返回 `None`。
pub fn alert_push(kind: &str, message: impl Into<String>) -> Option<i64> {
    let message = crate::applog::redact(&message.into());
    let mut a = ALERT.lock().unwrap();
    let t = now();
    a.recent.retain(|(_, at)| t - *at < ALERT_DEDUP_SECS);
    if a.recent.iter().any(|(m, _)| *m == message) {
        return None;
    }
    a.recent.push((message.clone(), t));
    let id = a.next_id;
    a.next_id += 1;
    a.latest = Some(crate::model::Alert {
        id,
        kind: kind.to_string(),
        message,
        at: t,
    });
    Some(id)
}

/// 最近一条未确认的提醒。
pub fn alert_latest() -> Option<crate::model::Alert> {
    ALERT.lock().unwrap().latest.clone()
}

/// 用户点「知道了」：只清掉 id 匹配的那条（新提醒不受影响）。
pub fn alert_ack(id: i64) {
    let mut a = ALERT.lock().unwrap();
    if a.latest.as_ref().is_some_and(|x| x.id == id) {
        a.latest = None;
    }
}

/// 单测用：清空提醒与去重记录。
pub fn alert_reset() {
    let mut a = ALERT.lock().unwrap();
    a.latest = None;
    a.recent.clear();
}

// -----------------------------------------------------------------------------
// 定时巡检进度
// -----------------------------------------------------------------------------

/// 巡检阶段。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SweepPhase {
    /// 调度器没在跑（未启动轮询 / 巡检未开启）。
    #[default]
    Off,
    /// 本轮进行中（批次之间也算）。
    Running,
    /// 一轮跑完，停留到 `next_pass_at` 再开始下一轮。
    Waiting,
    /// 整机限流退避中（退避结束后从断点续跑）。
    Cooldown,
}

/// 巡检进度快照（GUI 控制面板用；由 `sweep` 模块维护）。
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SweepStatus {
    pub phase: SweepPhase,
    /// 本轮开始时刻（epoch 秒）。
    pub pass_started_at: Option<f64>,
    /// 本轮要巡检的号数（轮开始时冻结）。
    pub pass_total: usize,
    /// 本轮已处理（成功 + 判定失败）的号数。
    pub pass_done: usize,
    /// 本轮到目前采到的新文章数。
    pub pass_new_articles: usize,
    /// 下一轮开始时刻（epoch 秒；只在 Waiting 时有意义）。
    pub next_pass_at: Option<f64>,
    /// 退避截止时刻（epoch 秒；只在 Cooldown 时有意义）。
    pub cooldown_until: Option<f64>,
    /// 退避原因。
    pub cooldown_reason: Option<String>,
    /// 最近一个批次的告警 / 错误（没有则空）。
    pub last_error: Option<String>,
    /// 累计完成的轮数（进程内）。
    pub passes_completed: u32,
    /// 环境故障（RPA 点不开种子 / 代理没流量）导致的暂停截止时刻；`None` = 没暂停。
    pub paused_until: Option<f64>,
    /// 暂停原因。
    pub paused_reason: Option<String>,
    /// 当前微信号近 24 小时已发的列表（getmsg）请求数（所有路径合计，滚动窗口）。
    pub list_calls_24h: i64,
    /// 每号列表请求预算（近 24 小时；0 = 不限）；达到后历史页与巡检都暂停领取，等最早一次请求滑出窗口。
    pub list_daily_budget: i64,
    /// 用户已点「停止巡检」、正在等当前批次收尾（收尾后 phase 回到 Off）。
    pub stopping: bool,
}

static SWEEP: Mutex<Option<SweepStatus>> = Mutex::new(None);

/// 巡检模块更新进度。
pub fn set_sweep_status(status: SweepStatus) {
    *SWEEP.lock().unwrap() = Some(status);
}

/// 当前巡检进度（调度器没跑时 phase=Off；退避信息顺手同步进去）。
pub fn sweep_status() -> SweepStatus {
    let mut s = SWEEP.lock().unwrap().clone().unwrap_or_default();
    let cd = cooldown_snapshot();
    if s.phase != SweepPhase::Off && cd.until.is_some() {
        s.phase = SweepPhase::Cooldown;
    }
    s.cooldown_until = cd.until;
    s.cooldown_reason = cd.until.map(|_| cd.reason.clone());
    let (calls, budget) = list_calls_snapshot();
    s.list_calls_24h = calls;
    s.list_daily_budget = budget;
    s.stopping = s.phase != SweepPhase::Off && sweep_stopping();
    s
}

/// 「停止巡检」已请求、主循环还在收尾当前批次。
static SWEEP_STOPPING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// GUI「停止巡检」：置位停止中标志（`sweep_status().stopping`）；主循环退出时随 [`sweep_set_off`] 清零。
pub fn sweep_set_stopping(on: bool) {
    SWEEP_STOPPING.store(on, std::sync::atomic::Ordering::SeqCst);
}

/// 是否处于「停止巡检」收尾中。
pub fn sweep_stopping() -> bool {
    SWEEP_STOPPING.load(std::sync::atomic::Ordering::SeqCst)
}

/// 巡检开关是否打开（调度器在跑：Waiting / Running / Cooldown 都算；收尾中也算，直到真正退出）。
pub fn sweep_on() -> bool {
    SWEEP
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|s| s.phase != SweepPhase::Off)
}

/// 用户要求「立即开始新一轮」的信号（GUI 命令置位，巡检在下一次要批次时消费）。
static SWEEP_RESTART: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 置位「立即开始新一轮」：跳过轮间停留 / 环境故障暂停，所有号重新成为候选。
pub fn sweep_request_restart() {
    SWEEP_RESTART.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// 消费「立即开始新一轮」信号（返回是否有请求，并清零）。
pub fn sweep_take_restart() -> bool {
    SWEEP_RESTART.swap(false, std::sync::atomic::Ordering::SeqCst)
}

/// 调度器退出：巡检进度回到 Off（保留计数便于 GUI 显示「上次」）。
pub fn sweep_set_off() {
    let mut g = SWEEP.lock().unwrap();
    if let Some(s) = g.as_mut() {
        s.phase = SweepPhase::Off;
    }
    sweep_set_stopping(false);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_clear() {
        set_capture_addr(Some("127.0.0.1:9999".into()));
        assert_eq!(capture_addr().as_deref(), Some("127.0.0.1:9999"));
        set_capture_addr(None);
        assert_eq!(capture_addr(), None);
    }

    #[test]
    fn run_guard_is_exclusive() {
        let g = try_acquire_run().expect("首次应能占住");
        assert!(run_busy());
        assert!(try_acquire_run().is_none(), "已占住时第二次应失败");
        drop(g);
        assert!(!run_busy());
        assert!(try_acquire_run().is_some());
    }

    #[test]
    fn cooldown_levels_double_and_cap() {
        assert_eq!(cooldown_secs_for_level(1), 300);
        assert_eq!(cooldown_secs_for_level(2), 600);
        assert_eq!(cooldown_secs_for_level(3), 1200);
        assert_eq!(cooldown_secs_for_level(4), 2400);
        assert_eq!(cooldown_secs_for_level(5), 3600);
        assert_eq!(cooldown_secs_for_level(30), 3600);
    }

    #[test]
    fn cooldown_trigger_and_clean() {
        // 全局状态：与其它触发退避的测试串行。
        let _g = COOLDOWN_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        cooldown_reset();
        assert_eq!(cooldown_remaining_secs(), None);
        let s = cooldown_trigger("HTTP 429");
        assert_eq!(s, 300);
        let snap = cooldown_snapshot();
        assert_eq!(snap.level, 1);
        assert_eq!(snap.reason, "HTTP 429");
        assert!(cooldown_remaining_secs().is_some_and(|r| r > 290 && r <= 300));
        // 再触发：等级 2，截止只会往后推
        let s2 = cooldown_trigger("验证页");
        assert_eq!(s2, 600);
        assert!(cooldown_remaining_secs().is_some_and(|r| r > 590));
        // 干净批次：等级归零，但未来的截止仍保留
        cooldown_note_clean();
        assert_eq!(cooldown_snapshot().level, 0);
        assert!(cooldown_remaining_secs().is_some());
        cooldown_reset();
        assert_eq!(cooldown_remaining_secs(), None);
    }

    #[test]
    fn cooldown_persists_and_restores_across_process() {
        // 模拟「被封 → 重启」：写库后清掉内存状态，再从库恢复，截止时刻与等级都还在。
        let _g = COOLDOWN_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let store = crate::store::Store::open_in_memory().unwrap();
        cooldown_reset();
        block_trigger("ret=-6");
        let snap = cooldown_snapshot();
        cooldown_save(&store, &snap).unwrap();
        // 「重启」：内存清空（直接清，不走 cooldown_reset，免得它去写总线上的 store）
        *COOLDOWN.lock().unwrap() = CooldownSnapshot::default();
        assert_eq!(cooldown_remaining_secs(), None);
        let restored = cooldown_restore(&store).expect("应恢复");
        assert_eq!(restored.level, 1);
        assert_eq!(restored.reason, "ret=-6");
        assert!(cooldown_remaining_secs().is_some_and(|r| r > 6 * 3600 - 10));
        // 截止已过、等级仍在：只恢复等级
        cooldown_save(
            &store,
            &CooldownSnapshot {
                until: Some(now() - 1.0),
                until_account: Some(now() - 1.0),
                level: 2,
                reason: "旧".into(),
                kind: "account".into(),
                ..Default::default()
            },
        )
        .unwrap();
        *COOLDOWN.lock().unwrap() = CooldownSnapshot::default();
        let restored = cooldown_restore(&store).expect("等级仍应恢复");
        assert_eq!(restored.level, 2);
        assert_eq!(restored.until, None);
        // 无截止且等级 0：视为无记录
        cooldown_save(&store, &CooldownSnapshot::default()).unwrap();
        assert!(cooldown_restore(&store).is_none());
        *COOLDOWN.lock().unwrap() = CooldownSnapshot::default();
    }

    #[test]
    fn block_levels_are_hours_and_cap_at_a_day() {
        assert_eq!(block_secs_for_level(1), 6 * 3600);
        assert_eq!(block_secs_for_level(2), 12 * 3600);
        assert_eq!(block_secs_for_level(3), 24 * 3600);
        assert_eq!(block_secs_for_level(9), 24 * 3600);
        let _g = COOLDOWN_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        cooldown_reset();
        let s = block_trigger("ret=-6");
        assert_eq!(s, 6 * 3600);
        assert!(cooldown_remaining_secs().is_some_and(|r| r > 6 * 3600 - 10));
        cooldown_reset();
    }

    #[tokio::test]
    async fn list_gate_spaces_consecutive_requests() {
        list_gate_reset();
        let w0 = list_gate_wait(200, 200).await;
        let t = std::time::Instant::now();
        let w1 = list_gate_wait(200, 200).await;
        assert!(t.elapsed() >= std::time::Duration::from_millis(180));
        assert!(w1 > 0 && w1 <= 200, "第二次应等约 200ms，实际 {w1}");
        assert_eq!(w0, 0, "闸门重置后第一次不等");
        // 0/0 = 不设闸门
        list_gate_reset();
        list_gate_wait(0, 0).await;
        let t2 = std::time::Instant::now();
        list_gate_wait(0, 0).await;
        assert!(t2.elapsed() < std::time::Duration::from_millis(50));
    }

    #[test]
    fn list_calls_snapshot_roundtrip() {
        set_list_daily_budget(180);
        set_list_calls_24h(42);
        assert_eq!(list_calls_snapshot(), (42, 180));
        let s = sweep_status();
        assert_eq!((s.list_calls_24h, s.list_daily_budget), (42, 180));
    }

    #[test]
    fn sync_active_block_follows_active_account() {
        let _g = COOLDOWN_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let store = crate::store::Store::open_in_memory().unwrap();
        *COOLDOWN.lock().unwrap() = CooldownSnapshot::default();
        // 没有激活号：不动
        sync_active_block(&store);
        assert!(cooldown_remaining_secs().is_none());
        // 激活号被封 → 全局按它等（account 类）
        let a = match store.wx_note_captured_uin("U1").unwrap() {
            crate::model::WxBind::Registered { id, .. } => id,
            other => panic!("{other:?}"),
        };
        store.wx_set_blocked(a, now() + 600.0, "ret=-6").unwrap();
        sync_active_block(&store);
        let snap = cooldown_snapshot();
        assert_eq!(snap.kind, "account");
        assert!(cooldown_remaining_secs().is_some_and(|r| r > 500));
        // 切到未封的号 → account 类退避清掉
        let b = match store.wx_note_captured_uin("U2").unwrap() {
            crate::model::WxBind::Registered { id, .. } => id,
            other => panic!("{other:?}"),
        };
        store.wx_activate(b).unwrap();
        sync_active_block(&store);
        assert!(cooldown_remaining_secs().is_none());
        // ip 类退避不受影响
        *COOLDOWN.lock().unwrap() = CooldownSnapshot {
            until: Some(now() + 300.0),
            until_ip: Some(now() + 300.0),
            kind: "ip".into(),
            ..Default::default()
        };
        sync_active_block(&store);
        assert!(cooldown_remaining_secs().is_some());
        *COOLDOWN.lock().unwrap() = CooldownSnapshot::default();
    }

    #[test]
    fn ip_and_account_cooldowns_are_independent() {
        // 封号期间撞到 IP 限流：不会把封号「改名」成 ip；切到未封的号后只剩 ip 类。
        let _g = COOLDOWN_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let store = crate::store::Store::open_in_memory().unwrap();
        *COOLDOWN.lock().unwrap() = CooldownSnapshot::default();
        let a = match store.wx_note_captured_uin("U1").unwrap() {
            crate::model::WxBind::Registered { id, .. } => id,
            other => panic!("{other:?}"),
        };
        store
            .wx_set_blocked(a, now() + 6.0 * 3600.0, "ret=-6")
            .unwrap();
        sync_active_block(&store);
        cooldown_trigger("429");
        let snap = cooldown_snapshot();
        assert_eq!(snap.kind, "account", "封号截止更晚，生效的仍是 account");
        assert!(snap.until_ip.is_some() && snap.until_account.is_some());
        // 模拟 ip 退避到期：account 仍在
        COOLDOWN.lock().unwrap().until_ip = Some(now() - 1.0);
        assert!(cooldown_remaining_secs().is_some_and(|r| r > 5 * 3600));
        assert_eq!(cooldown_snapshot().until_ip, None);
        // 切到未封的号：account 类清掉；再来一次 ip 退避就只剩 ip 的
        let b = match store.wx_note_captured_uin("U2").unwrap() {
            crate::model::WxBind::Registered { id, .. } => id,
            other => panic!("{other:?}"),
        };
        store.wx_activate(b).unwrap();
        sync_active_block(&store);
        assert!(cooldown_remaining_secs().is_none());
        cooldown_trigger("429");
        let snap = cooldown_snapshot();
        assert_eq!(snap.kind, "ip");
        assert!(snap.until_account.is_none());
        assert!(cooldown_remaining_secs().is_some_and(|r| r <= 1200));
        *COOLDOWN.lock().unwrap() = CooldownSnapshot::default();
    }

    #[test]
    fn unit_guard_is_independent_of_run_guard() {
        // 主循环长驻持有 RunGuard，但批次之间空闲：执行单元锁应能被单号重试抢到。
        let run = try_acquire_run().expect("占住整链");
        let u = try_acquire_unit().expect("整链在跑但无单元时应能占住单元");
        assert!(unit_busy());
        assert!(try_acquire_unit().is_none());
        drop(u);
        assert!(!unit_busy());
        drop(run);
    }
}
