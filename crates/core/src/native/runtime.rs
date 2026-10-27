//! 采集任务运行器：工作进程驱动页面或独立请求，Rust 负责预算、响应核对及原子检查点。

use super::protocol::{self, NativeRequest, WorkerEvent};
use crate::{
    applog::{self, Stage},
    model::{now, RunLogEvent},
    Store,
};
use anyhow::{bail, Context, Result};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
};

const WORKER: &str = include_str!("../../../../native/worker.py");
const PROBE: &str = include_str!("../../../../tools/native_bridge_probe.js");
const UI: &str = include_str!("../../../../native/window_control.js");
const SESSION_FILES: &[(&str, &str)] = &[
    (
        "discovery_worker.py",
        include_str!("../../../../native/discovery_worker.py"),
    ),
    (
        "session_protocol.py",
        include_str!("../../../../native/session_protocol.py"),
    ),
    (
        "session_worker.py",
        include_str!("../../../../native/session_worker.py"),
    ),
    (
        "session_vault.py",
        include_str!("../../../../native/session_vault.py"),
    ),
    (
        "session_agent.js",
        include_str!("../../../../native/session_agent.js"),
    ),
];
const PROFILES: &[&str] = &[
    include_str!("../../../../native/profiles/4.1.12.26.json"),
    include_str!("../../../../native/profiles/4.1.15.56.json"),
];

/// 便携版优先发现程序旁的运行环境；显式绝对路径保持不变。
pub fn python_executable(configured: &str) -> PathBuf {
    if let Ok(executable) = std::env::current_exe() {
        if let Some(parent) = executable.parent() {
            let relative = if configured == "python" {
                Path::new("runtime/python.exe")
            } else {
                Path::new(configured)
            };
            if relative.is_relative() && parent.join(relative).is_file() {
                return parent.join(relative);
            }
        }
    }
    PathBuf::from(configured)
}

pub(crate) fn materialize(
    dir: &Path,
    config: &super::NativeConfig,
    cursor: Option<&str>,
) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    for (name, content) in [("worker.py", WORKER), ("probe.js", PROBE), ("ui.js", UI)] {
        std::fs::write(dir.join(name), content)?;
    }
    for (name, content) in SESSION_FILES {
        std::fs::write(dir.join(name), content)?;
    }
    std::fs::write(
        dir.join("session_probe.py"),
        include_str!("../../../../native/session_probe.py"),
    )?;
    let profiles: Vec<serde_json::Value> = PROFILES
        .iter()
        .map(|text| serde_json::from_str(text))
        .collect::<std::result::Result<_, _>>()?;
    std::fs::write(
        dir.join("profile.json"),
        serde_json::to_vec(&serde_json::json!({"profiles": profiles}))?,
    )?;
    let mut value = serde_json::to_value(config)?;
    if config.transport == "session" {
        value["_resume_cursor"] = serde_json::to_value(cursor)?;
    }
    std::fs::write(dir.join("config.json"), serde_json::to_vec(&value)?)?;
    Ok(())
}

async fn send(stdin: &mut ChildStdin, action: &str) -> Result<()> {
    stdin
        .write_all(format!("{{\"action\":\"{action}\"}}\n").as_bytes())
        .await?;
    stdin.flush().await?;
    Ok(())
}

async fn shutdown(child: &mut Child, stdin: &mut ChildStdin) {
    let _ = send(stdin, "stop").await;
    if tokio::time::timeout(Duration::from_secs(3), child.wait())
        .await
        .is_err()
    {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
}

fn next_due(store: &Store, cfg: &super::NativeConfig) -> Result<f64> {
    let policy = store.wb_policy()?;
    let gap = if policy.version > 1 {
        crate::rng::between(policy.list_min_ms, policy.list_max_ms) as f64 / 1000.
    } else {
        crate::rng::between(cfg.min_gap_seconds, cfg.max_gap_seconds) as f64
    };
    // 无历史账本时首次刷新可立即执行；之后任意列表/附带请求都会推迟下一次主动操作。
    Ok(store
        .wb_last_list_request()?
        .map(|t| t.max(now() - 86400.) + gap)
        .unwrap_or_else(now))
}

fn worker_reason(reason: &str) -> &str {
    match reason {
        "unsupported_version_or_no_wechat" => {
            "未找到已适配的微信模块或前台原生浏览器，请检查微信版本及页面"
        }
        "profile_window_not_unique" => "无法确定当前公众号，请只保留目标公众号主页后继续",
        "ui_action_failed" => "页面操作未完成，请检查焦点、窗口尺寸及桌面会话",
        "target_changed" | "target_name_mismatch" => "公众号身份与任务不一致，请重新打开目标列表",
        "ambiguous_callback" => "回调关联存在歧义，需要重新验证客户端协议",
        "worker_environment_error" => "原生环境异常，请检查 Python、Frida 安装及权限",
        "session_capture_environment" => {
            "无法准备会话提取，请登录已适配的微信并将目标公众号主页放到前台"
        }
        "session_environment_error" => {
            "会话环境校验失败，请检查 Python 的 Frida、cryptography 依赖和会话文件"
        }
        "session_capture_timeout" | "session_capture_overflow" | "session_capture_failed" => {
            "未识别到可采集的公众号，请打开公众号主页后继续；搜索页和文章页无法用于采集"
        }
        "session_request_failed" => {
            "独立请求未通过传输或认证校验；会话可能已失效，请保持原公众号主页打开并点击继续"
        }
        "session_cursor_missing" => "响应缺少翻页游标，已停止自动请求",
        _ => "原生采样或载荷异常，需要检查版本适配",
    }
}

fn pause(store: &Store, job_id: i64, reason: &str) {
    if store
        .native_job(job_id)
        .ok()
        .flatten()
        .is_some_and(|j| j.status == "running")
    {
        let _ = store.native_set_state(job_id, "paused", Some(reason));
        let message = format!("原生采集已暂停：{reason}；检查后手动继续");
        applog::warn(Stage::List, &message);
        if reason != "manual" {
            crate::notify::notify(crate::notify::Kind::Task, message);
        }
    }
}

/// 由 Orchestrator 唯一主循环调用。暂停/失败不重试；退出前停探针并保留未完成请求的预算。
pub async fn run(store: Arc<Store>, directory: PathBuf, stop: Arc<AtomicBool>) {
    let Ok(Some(job)) = store.native_active() else {
        return;
    };
    if job.status != "running" {
        return;
    }
    crate::phases::begin(-job.id);
    crate::phases::mark("原生环境校验");
    let started = now();
    let result = run_inner(store.clone(), &directory, job.id, stop).await;
    if let Err(error) = result {
        pause(&store, job.id, &error.to_string());
    }
    let _ = store.native_interrupt_pending(job.id);
    let _ = store.native_purge_calls();
    crate::phases::finish(-job.id);
    if let Ok(Some(final_job)) = store.native_job(job.id) {
        let calls = store.native_calls_since(started, 10000).unwrap_or_default();
        let own: Vec<_> = calls.iter().filter(|c| c.job_id == job.id).collect();
        let _ = store.append_run_log(&RunLogEvent {
            kind: "native".into(),
            started_at: started,
            finished_at: now(),
            accounts: i64::from(final_job.biz.is_some()),
            ok: i64::from(final_job.status == "done"),
            failed: i64::from(final_job.status == "paused"),
            new_articles: final_job.new_articles - job.new_articles,
            urls: own.iter().map(|c| c.articles).sum(),
            list_calls: own.len() as i64,
            rate_limited: own.iter().any(|c| c.outcome == "rate_limited"),
            note: Some(format!(
                "原生任务 {}：{}{}",
                job.id,
                final_job.status,
                final_job
                    .pause_reason
                    .as_ref()
                    .map(|r| format!("（{r}）"))
                    .unwrap_or_default()
            )),
            ..Default::default()
        });
    }
}

async fn run_inner(
    store: Arc<Store>,
    directory: &Path,
    job_id: i64,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    let _list_guard = crate::workbench::list_lock().lock().await;
    let job = store.native_job(job_id)?.context("任务不存在")?;
    let cfg = job.config;
    cfg.validate()?;
    if job.pages >= cfg.max_pages as i64
        || (job.pages > 0 && job.cursor.as_deref().is_some_and(str::is_empty))
    {
        store.native_set_state(job_id, "done", None)?;
        return Ok(());
    }
    let initial_policy = store.wb_policy()?;
    let budget = if initial_policy.version > 1 {
        initial_policy.list_daily_budget
    } else {
        cfg.daily_budget
    };
    anyhow::ensure!(
        store.wb_list_window(now() - 86400.)?.0 < budget,
        "近 24 小时原生请求预算已用完"
    );
    if let Some(remaining) = crate::runstate::cooldown_remaining_secs() {
        bail!("整机仍在退避中，剩余 {remaining} 秒")
    }
    materialize(
        directory,
        &cfg,
        if job.pages > 0 {
            job.cursor.as_deref()
        } else {
            None
        },
    )
    .context("无法准备原生运行环境")?;
    let mut command = Command::new(python_executable(&cfg.python_path));
    command
        .arg(directory.join("worker.py"))
        .arg("--config")
        .arg(directory.join("config.json"))
        .arg("--profile")
        .arg(directory.join("profile.json"))
        .arg("--agent")
        .arg(directory.join("probe.js"))
        .arg("--ui-agent")
        .arg(directory.join("ui.js"))
        .arg("--session")
        .arg(format!("{job_id}-{}", (now() * 1000000.) as u64))
        .env("PYTHONIOENCODING", "utf-8")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let mut child = command
        .spawn()
        .map_err(|_| anyhow::anyhow!("无法启动 Python，请检查原生环境路径及 Frida 安装"))?;
    let mut stdin = child.stdin.take().context("无法创建控制通道")?;
    let mut lines = BufReader::new(child.stdout.take().context("无法创建采样通道")?).lines();
    let mut unit_guard = None;
    let mut complete = false;
    let mut ready = false;
    let mut initializing_capture = cfg.transport == "session" && cfg.session_file.is_empty();
    let mut dll = String::new();
    let mut pending = HashMap::<String, NativeRequest>::new();
    let mut due = next_due(&store, &cfg)?;
    let mut policy_version = store.wb_policy()?.version;
    let mut waiting: Option<Instant> = Some(Instant::now());
    let mut settling_until: Option<Instant> = None;
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    let outcome: Result<()> = async {
        loop {
            tokio::select! {
                line=lines.next_line()=>{
                    let Some(line)=line? else { bail!("工作进程已退出，未收到正常结束信号") };
                    anyhow::ensure!(line.len()<=8*1024*1024,"桥消息超过大小上限");
                    let event:WorkerEvent=serde_json::from_str(&line).map_err(|_|anyhow::anyhow!("工作进程协议无效"))?;
                    match event {
                        WorkerEvent::Ready{dll_sha256,version}=>{
                            anyhow::ensure!(!ready,"重复初始化"); ready=true; dll=dll_sha256; waiting=None;
                            applog::info(Stage::List,format!("原生采集已就绪，微信版本 {version}"));
                            store.native_next_action(job_id,Some(due))?;
                        }
                        WorkerEvent::Request{request}=>{
                            anyhow::ensure!(ready,"初始化前出现请求");
                            anyhow::ensure!(request.transport=="native" || (cfg.transport=="session" && request.transport=="mmtls"),"请求来源不匹配");
                            let permit=request.transport=="mmtls";
                            anyhow::ensure!(!request.capture_only || (cfg.transport=="session" && request.transport=="native" && request.cgi==5814),"无效的会话重提取请求");
                            let current=store.native_job(job_id)?.context("任务不存在")?;
                            if store.native_record_request(&current,&request,&dll)? {
                                if request.cgi==5814 {
                                    anyhow::ensure!(waiting.is_some() && !pending.values().any(|r|r.cgi==5814),"出现非调度列表请求，请保持目标页面不被人工操作");
                                }
                                pending.insert(request.request_id.clone(),request);
                                due=next_due(&store,&cfg)?;
                                store.native_next_action(job_id,Some(due))?;
                                if permit {send(&mut stdin,"permit").await?;}
                            } else {
                                bail!("重复请求已被拒绝发送");
                            }
                        }
                        WorkerEvent::TargetDetected{request_id,payload}=>{
                            anyhow::ensure!(cfg.transport=="session","当前模式不支持自动识别");
                            let request=pending.get(&request_id).context("目标识别无法关联请求")?;
                            anyhow::ensure!(payload.get("transport").is_none(),"目标识别必须来自微信响应");
                            let current=store.native_job(job_id)?.context("任务不存在")?;
                            let page=protocol::parse_page(&payload,request.user_name.as_deref().unwrap_or(""),current.biz.as_deref())?;
                            store.native_bind_target(job_id,request,&page)?;
                            applog::info(Stage::List,"已从微信列表响应识别当前公众号");
                        }
                        WorkerEvent::Response{request_id,ts,payload,session_path,evidence_path}=>{
                            let Some(request)=pending.remove(&request_id) else { bail!("响应无法关联请求") };
                            let codes=protocol::response_codes(&payload);
                            let transport_ok=if request.transport=="mmtls" {protocol::session_authenticated(&payload)} else {codes.outer==Some(0) && codes.jsapi==Some(0)};
                            if codes.rate_signal || !transport_ok || codes.inner.is_some_and(|r|r!=0) {
                                let outcome=if codes.rate_signal {"rate_limited"} else {"error"};
                                store.native_finish_call(&request_id,ts,&codes,outcome,Some("native_response_error"))?;
                                bail!("原生接口返回异常（已记录各层返回码）")
                            }
                            if request.cgi==5814 {
                                initializing_capture=false;
                                let current=store.native_job(job_id)?.context("任务不存在")?;
                                if request.capture_only {
                                    let page=protocol::parse_page(&payload,request.user_name.as_deref().unwrap_or(""),current.biz.as_deref())?;
                                    anyhow::ensure!(page.nickname.as_deref()==Some(current.config.window_title.as_str()) && current.user_name.as_deref()==Some(page.user_name.as_str()),"重新提取会话时公众号身份变化");
                                    store.native_finish_call(&request_id,ts,&codes,"ok",Some("session_refreshed"))?;
                                    waiting=None;unit_guard.take();
                                    applog::info(Stage::List,"会话已重新提取，将从上次游标继续");
                                    continue;
                                }
                                let commit=protocol::parse_page(&payload,request.user_name.as_deref().unwrap_or(""),current.biz.as_deref())
                                    .and_then(|page| {let added=store.native_commit_page(job_id,&request,&page,ts,&codes)?; complete=page.is_end || current.pages+1>=cfg.max_pages as i64; Ok(added)});
                                match commit {
                                    Ok(added)=>{
                                        if let (Some(session),Some(evidence))=(session_path.as_deref(),evidence_path.as_deref()) {
                                            let page=protocol::parse_page(&payload,request.user_name.as_deref().unwrap_or(""),current.biz.as_deref())?;
                                            let first=store.wb_source(&page.biz)?.is_none();
                                            store.wb_capture(&page,&current.config,session,evidence,ts)?;
                                            if first {
                                                if let Some((old_id,offset,observed))=store.wb_legacy_checkpoint(&page.biz,&page.user_name,job_id)? {
                                                    let protected=crate::workbench::engine::seal_legacy_cursor(directory,&current.config.python_path,old_id,&offset,observed).await?;
                                                    store.wb_import_legacy_checkpoint(&page.biz,&offset,&protected,observed)?;
                                                }
                                            }
                                        }
                                        applog::info(Stage::List,format!("原生列表第 {} 页已入库，新增 {added} 篇",current.pages+1));
                                    },
                                    Err(_)=>{ store.native_finish_call(&request_id,ts,&codes,"invalid_payload",Some("identity_cursor_or_schema_mismatch"))?; bail!("列表结构、身份或游标校验未通过") }
                                }
                                waiting=None;
                                unit_guard.take();
                                if !complete {store.native_next_action(job_id,Some(due))?;}
                                settling_until=Some(Instant::now()+Duration::from_secs(3));
                            } else { store.native_finish_call(&request_id,ts,&codes,"ok",None)?; }
                        }
                        WorkerEvent::Fault{reason}=>bail!("{}",worker_reason(&reason)),
                        WorkerEvent::RequestFailed{request_id,ts,reason}=>{
                            anyhow::ensure!(pending.remove(&request_id).is_some(),"失败请求无法关联");
                            store.native_finish_call(&request_id,ts,&Default::default(),"error",Some(&reason))?;
                            bail!("{}",worker_reason(&reason));
                        }
                        WorkerEvent::Stopped=>bail!("采样提前结束"),
                    }
                }
                _=tick.tick()=>{
                    let live_policy=store.wb_policy()?;
                    if live_policy.version!=policy_version {
                        policy_version=live_policy.version;
                        let mut live_cfg=cfg.clone();
                        live_cfg.min_gap_seconds=live_policy.list_min_ms.div_ceil(1000);
                        live_cfg.max_gap_seconds=live_policy.list_max_ms.div_ceil(1000);
                        due=next_due(&store,&live_cfg)?;
                        store.native_next_action(job_id,Some(due))?;
                    }
                    let current=store.native_job(job_id)?.context("任务不存在")?;
                    if stop.load(Ordering::Relaxed) || current.status=="paused" || current.status=="cancelled" {
                        pause(&store,job_id,"manual"); break;
                    }
                    // 成功后保留短暂观察窗口，以记入由这一页引发的附带请求。
                    if complete && pending.is_empty() && settling_until.is_none_or(|t|Instant::now()>=t) {store.native_set_state(job_id,"done",None)?;break }
                    if waiting.is_some_and(|t|t.elapsed().as_secs()>=cfg.response_timeout_seconds) || pending.values().any(|r|now()-r.ts>=cfg.response_timeout_seconds as f64) {
                        for r in pending.values() {store.native_finish_call(&r.request_id,now(),&Default::default(),"timeout",Some("response_timeout"))?;}
                        if initializing_capture {bail!("公众号识别或会话取样超时，请确认已打开公众号主页且桌面未锁屏后继续；未自动重试")}
                        bail!("等待可核对响应超时；未自动重试")
                    }
                    if ready && !complete && current.status=="running" && waiting.is_none() && pending.is_empty() && now()>=due {
                        let (used,_)=store.wb_list_window(now()-86400.)?;
                        let budget=if live_policy.version>1{live_policy.list_daily_budget}else{cfg.daily_budget};
                        anyhow::ensure!(used<budget,"近 24 小时列表请求预算已用完");
                        anyhow::ensure!(live_policy.cooldown_until<=now(),"整机仍在访问冷却中");
                        if let Some(remaining)=crate::runstate::cooldown_remaining_secs() {bail!("整机仍在退避中，剩余 {remaining} 秒")}
                        unit_guard=Some(crate::runstate::try_acquire_unit().context("微信窗口正被其它执行单元占用")?);
                        crate::phases::mark("原生页面操作与响应核对");
                        send(&mut stdin,if current.pages==0 || initializing_capture {"refresh"} else {"advance"}).await?;
                        waiting=Some(Instant::now());
                        store.native_next_action(job_id,None)?;
                    }
                }
            }
        }
        Ok(())
    }.await;
    shutdown(&mut child, &mut stdin).await;
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::{protocol::NativeRequest, NativeConfig};
    #[test]
    fn persisted_requests_delay_next_action() {
        let store = Store::open_in_memory().unwrap();
        let cfg = NativeConfig {
            window_title: "虚构公众号".into(),
            min_gap_seconds: 90,
            max_gap_seconds: 90,
            ..Default::default()
        };
        let job = store.native_create(&cfg).unwrap();
        let req = NativeRequest {
            capture_only: false,
            transport: "native".into(),
            request_id: "fake".into(),
            ts: now(),
            cgi: 5814,
            action: Some(0),
            offset: None,
            user_name: Some("fake".into()),
        };
        store.native_record_request(&job, &req, "fake").unwrap();
        store.native_pause_on_restart().unwrap();
        assert!((next_due(&store, &cfg).unwrap() - (req.ts + 90.)).abs() < 0.01);
    }
    #[tokio::test]
    async fn exhausted_budget_never_launches_worker() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let cfg = NativeConfig {
            python_path: "不存在的运行环境".into(),
            window_title: "虚构公众号".into(),
            daily_budget: 1,
            ..Default::default()
        };
        let job = store.native_create(&cfg).unwrap();
        let req = NativeRequest {
            capture_only: false,
            transport: "native".into(),
            request_id: "fake".into(),
            ts: now(),
            cgi: 5814,
            action: Some(0),
            offset: None,
            user_name: Some("fake".into()),
        };
        store.native_record_request(&job, &req, "fake").unwrap();
        let error = run_inner(
            store,
            Path::new("unused-native-test"),
            job.id,
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("预算已用完"));
    }
}

#[cfg(all(test, unix))]
mod process_tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;
    /// 假工作进程只读 stdin、返回虚构桥消息；不导入 Frida、不访问微信或网络。
    #[allow(clippy::await_holding_lock)]
    async fn exercise(aux_error: bool, stop_early: bool, autodetect: bool) {
        let _guard = crate::runstate::COOLDOWN_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::runstate::cooldown_reset();
        let root = std::env::temp_dir().join(format!(
            "mpider-native-contract-{}-{}",
            std::process::id(),
            crate::rng::next_u64()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let executable = root.join("fake-worker");
        let stamp = now();
        let payload = json!({"base_resp":{"ret":0},"err_msg":"H5ExtTransfer:ok","jsapi_resp":{"ret":0,"resp_json":{"BaseResponse":{"Ret":0},"AccountInfo":{"UserName":"fake-account","NickName":"虚构公众号"},"MsgList":{"Msg":[{"AppMsg":{"DetailInfo":[{"Title":"虚构文章","ContentUrl":"https://mp.weixin.qq.com/s?__biz=FAKE&mid=1&idx=1&sn=fake"}]}}],"PagingInfo":{"Offset":"next","IsEnd":0}}}}});
        let mut events = vec![
            json!({"event":"request","request":{"request_id":"fake-1","ts":stamp,"cgi":5814,"action":0,"offset":"","user_name":"fake-account"}}),
            json!({"event":"response","request_id":"fake-1","ts":stamp+0.1,"payload":payload}),
        ];
        if autodetect {
            events.insert(
                1,
                json!({"event":"target_detected","request_id":"fake-1","payload":payload}),
            );
        }
        if aux_error {
            events.extend([json!({"event":"request","request":{"request_id":"fake-2","ts":stamp+0.2,"cgi":26365,"action":null,"offset":null,"user_name":null}}),json!({"event":"response","request_id":"fake-2","ts":stamp+0.3,"payload":{"base_resp":{"ret":0},"err_msg":"H5ExtTransfer:fail","jsapi_resp":{"ret":7}}})]);
        }
        let events = events
            .iter()
            .map(|v| serde_json::to_string(v).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&executable,format!("#!/bin/sh\ncat <<'READY'\n{{\"event\":\"ready\",\"dll_sha256\":\"fake\",\"version\":\"fake\"}}\nREADY\nIFS= read -r command\ncat <<'EVENTS'\n{events}\nEVENTS\nIFS= read -r command\n")).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let store = Arc::new(Store::open_in_memory().unwrap());
        let cfg = super::super::NativeConfig {
            python_path: executable.display().to_string(),
            transport: if autodetect { "session" } else { "page" }.into(),
            window_title: if autodetect { "" } else { "虚构公众号" }.into(),
            max_pages: if stop_early { 2 } else { 1 },
            ..Default::default()
        };
        let job = store.native_create(&cfg).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let watcher = if stop_early {
            let watched = store.clone();
            let signal = stop.clone();
            Some(tokio::spawn(async move {
                for _ in 0..100 {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                    let current = watched.native_job(job.id).unwrap().unwrap();
                    if current.pages == 1 {
                        assert!(
                            current.next_action_at.is_some_and(|t| t > now()),
                            "页间等待须持久化到界面检查点"
                        );
                        signal.store(true, Ordering::Relaxed);
                        return;
                    }
                }
                panic!("未等到首页入库")
            }))
        } else {
            None
        };
        let outcome = run_inner(store.clone(), &root.join("scripts"), job.id, stop).await;
        if let Some(watcher) = watcher {
            watcher.await.unwrap();
        }
        let after = store.native_job(job.id).unwrap().unwrap();
        assert_eq!(store.count_articles(None).unwrap(), 1);
        if aux_error {
            assert!(outcome.is_err());
            assert_eq!(after.status, "running", "附带请求异常前不能提前完成任务");
            assert!(store
                .native_calls_since(0., 10)
                .unwrap()
                .iter()
                .any(|c| c.cgi == 26365 && c.outcome == "error"));
        } else {
            outcome.unwrap();
            assert_eq!(after.status, if stop_early { "paused" } else { "done" });
            assert_eq!(after.new_articles, 1);
        }
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn worker_contract_commits_page_and_shuts_down() {
        exercise(false, false, false).await;
    }
    #[tokio::test]
    async fn session_detects_target_before_collecting_without_name_input() {
        exercise(false, false, true).await;
    }
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn final_page_aux_error_does_not_finish_job() {
        exercise(true, false, false).await;
    }
    #[tokio::test]
    async fn page_wait_checkpoint_is_visible_and_can_pause() {
        exercise(false, true, false).await;
    }
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn session_resume_refreshes_without_replacing_checkpoint() {
        let _guard = crate::runstate::COOLDOWN_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::runstate::cooldown_reset();
        let root =
            std::env::temp_dir().join(format!("mpider-session-resume-{}", crate::rng::next_u64()));
        std::fs::create_dir_all(&root).unwrap();
        let executable = root.join("fake-worker");
        let cfg = super::super::NativeConfig {
            transport: "session".into(),
            python_path: executable.display().to_string(),
            window_title: "虚构公众号".into(),
            max_pages: 2,
            min_gap_seconds: 60,
            max_gap_seconds: 60,
            ..Default::default()
        };
        let store = Arc::new(Store::open_in_memory().unwrap());
        let job = store.native_create(&cfg).unwrap();
        let body = json!({"BaseResponse":{"Ret":0},"AccountInfo":{"UserName":"fake-account","NickName":"虚构公众号"},"MsgList":{"Msg":[{"AppMsg":{"DetailInfo":[{"ContentUrl":"https://mp.weixin.qq.com/s?__biz=FAKE&mid=1&idx=1"}]}}],"PagingInfo":{"Offset":"checkpoint","IsEnd":0}}});
        let native = json!({"base_resp":{"ret":0},"err_msg":"H5ExtTransfer:ok","jsapi_resp":{"ret":0,"resp_json":body}});
        let initial:NativeRequest=serde_json::from_value(json!({"request_id":"initial","ts":now()-200.,"cgi":5814,"action":0,"user_name":"fake-account"})).unwrap();
        store.native_record_request(&job, &initial, "fake").unwrap();
        let page = protocol::parse_page(&native, "fake-account", None).unwrap();
        store
            .native_commit_page(
                job.id,
                &initial,
                &page,
                now(),
                &protocol::response_codes(&native),
            )
            .unwrap();
        store
            .native_set_state(job.id, "paused", Some("manual"))
            .unwrap();
        store.native_resume(job.id).unwrap();
        let warm = json!({"event":"request","request":{"request_id":"warm","ts":now()-120.,"cgi":5814,"action":0,"user_name":"fake-account","capture_only":true}});
        let warm_response =
            json!({"event":"response","request_id":"warm","ts":now(),"payload":native});
        let request = json!({"event":"request","request":{"request_id":"continued","ts":now(),"cgi":5814,"action":1,"offset":"checkpoint","user_name":"fake-account","transport":"mmtls"}});
        let mut final_body = body.clone();
        final_body["MsgList"]["PagingInfo"] = json!({"Offset":"end","IsEnd":1});
        final_body["MsgList"]["Msg"][0]["AppMsg"]["DetailInfo"][0]["ContentUrl"] =
            json!("https://mp.weixin.qq.com/s?__biz=FAKE&mid=2&idx=1");
        let response = json!({"event":"response","request_id":"continued","ts":now(),"payload":{"transport":"mmtls","authenticated_outer_records":3,"inner_authenticated":true,"body":final_body}});
        let script=format!("#!/bin/sh\necho '{{\"event\":\"ready\",\"dll_sha256\":\"fake\",\"version\":\"fake\"}}'\nIFS= read -r cmd\n[ \"$cmd\" = '{{\"action\":\"refresh\"}}' ] || exit 2\necho '{warm}'\necho '{warm_response}'\nIFS= read -r cmd\n[ \"$cmd\" = '{{\"action\":\"advance\"}}' ] || exit 3\necho '{request}'\nIFS= read -r cmd\n[ \"$cmd\" = '{{\"action\":\"permit\"}}' ] || exit 4\necho '{response}'\nIFS= read -r cmd\n");
        std::fs::write(&executable, script).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        run_inner(
            store.clone(),
            &root.join("scripts"),
            job.id,
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
        let after = store.native_job(job.id).unwrap().unwrap();
        assert_eq!(after.status, "done");
        assert_eq!(after.pages, 2);
        assert_eq!(after.new_articles, 2);
        assert_eq!(store.native_calls_window(0.).unwrap().0, 3);
        std::fs::remove_dir_all(root).unwrap();
    }
}
