//! 公众号监测：只登记用户打开的主页身份与会话，显式恢复意图到齐后继续。
use crate::{
    model::{now, RunLogEvent},
    native::{protocol, runtime, NativeConfig},
    Store,
};
use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum Event {
    Ready,
    Phase {
        phase: String,
    },
    ObservedRequest {
        request_id: String,
        ts: f64,
    },
    Home {
        request_id: String,
        user_name: String,
        payload: Value,
        ts: f64,
        #[serde(default)]
        request_ts: Option<f64>,
    },
    SessionReady {
        request_id: String,
        user_name: String,
        payload: Value,
        ts: f64,
        session_path: String,
        evidence_path: String,
        #[serde(default)]
        request_ts: Option<f64>,
    },
    Diagnostic {
        reason: String,
    },
    Fault {
        reason: String,
    },
    Stopped,
}

fn valid_file(directory: &Path, path: &str) -> Result<()> {
    let canonical = std::fs::canonicalize(path)?;
    ensure!(
        canonical.starts_with(std::fs::canonicalize(directory.join("sessions"))?),
        "会话证据不属于本轮观察目录"
    );
    Ok(())
}

pub async fn run(store: Arc<Store>, root: PathBuf, id: i64, python: String) {
    crate::phases::begin(-id);
    crate::phases::mark("持续识别公众号");
    let started = now();
    let result = run_inner(&store, &root, id, &python).await;
    let reason = result
        .err()
        .map(|_| "主页同步已中断，请检查微信登录状态后重新开启");
    let _ = store.wb_discovery_state(
        id,
        if reason.is_some() {
            "failed"
        } else {
            "stopped"
        },
        reason,
    );
    let run = store.wb_discovery().ok().flatten();
    let _ = store.append_run_log(&RunLogEvent {
        kind: "discovery".into(),
        started_at: started,
        finished_at: now(),
        accounts: run.as_ref().map_or(0, |r| r.accounts.len() as i64),
        ok: i64::from(reason.is_none()),
        failed: i64::from(reason.is_some()),
        list_calls: run.as_ref().map_or(0, |r| r.observed_requests as i64),
        note: Some(
            reason
                .unwrap_or("持续识别已停止，已创建任务继续运行")
                .into(),
        ),
        ..Default::default()
    });
    crate::phases::finish(-id);
}
async fn run_inner(store: &Store, root: &Path, id: i64, python: &str) -> Result<()> {
    let directory = root.join("discovery").join(id.to_string());
    let config = NativeConfig {
        enabled: true,
        transport: "session".into(),
        python_path: python.into(),
        max_pages: 1,
        ..Default::default()
    };
    store.wb_discovery_phase(id, "materialize")?;
    runtime::materialize(&directory, &config, None)?;
    let mut command = tokio::process::Command::new(runtime::python_executable(python));
    command
        .arg(directory.join("discovery_worker.py"))
        .arg("--directory")
        .arg(&directory)
        .arg("--run-id")
        .arg(id.to_string())
        .env("PYTHONIOENCODING", "utf-8")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let mut child = command.spawn().context("无法启动观察器")?;
    store.wb_discovery_phase(id, "worker_spawned")?;
    let mut input = child.stdin.take().context("观察器控制通道不可用")?;
    let mut lines = BufReader::new(child.stdout.take().context("观察器响应通道不可用")?).lines();
    let started = Instant::now();
    let mut ready = false;
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    let result: Result<()> = async {
        loop {
            tokio::select! {
                _=tick.tick()=>{
                    let run=store.wb_discovery()?.context("识别运行不存在")?;
                    if run.id!=id || !matches!(run.state.as_str(),"starting"|"running") { break; }
                    ensure!(ready || started.elapsed()<Duration::from_secs(60),"准备观察器超时");

                }
                line=lines.next_line()=>{
                    let line=line?.context("观察器意外退出")?;
                    ensure!(line.len()<=8*1024*1024,"观察消息超出大小限制");
                    let event:Event=serde_json::from_str(&line).context("观察器协议异常")?;
                    // 停止后的晚到响应不允许更新列表或创建历史任务。
                    if store.wb_discovery()?.is_none_or(|r|r.id!=id || !matches!(r.state.as_str(),"starting"|"running")) { break; }
                    match event {
                        Event::Ready=>{ store.wb_discovery_phase(id,"ready")?; ready=true; store.wb_discovery_state(id,"running",None)?; }
                        Event::Phase{phase}=>{
                            if matches!(phase.as_str(),"attached"|"script_loaded"|"session_armed"|"unload_begin"|"unload_done"|"detached") {store.wb_discovery_phase(id,&phase)?;}
                        }
                        Event::ObservedRequest{request_id,ts}=>{store.wb_discovery_request(id,&request_id,ts)?;}
                        Event::Home{request_id,user_name,payload,ts,request_ts}=>{
                            match protocol::parse_page(&payload,&user_name,None) {
                                Ok(page)=>{ if !store.wb_discovery_observe_home(id,&page,request_ts.unwrap_or(0.),ts)? { continue; } store.wb_recovery_home(id,&page.biz,request_ts.unwrap_or(ts))?; store.wb_discovery_admit(id,&page.biz,false,request_ts.unwrap_or(ts))?; store.wb_discovery_state(id,"running",None)?; store.wb_discovery_response(&request_id,&page.biz,"ok",ts)?; }
                                Err(_)=>{ store.wb_discovery_response(&request_id,"","schema_or_identity_error",ts)?; store.wb_discovery_state(id,"running",Some("观察到列表响应，但未通过返回码或公众号身份校验；继续等待新的主页请求"))?; }
                            }
                        }
                        Event::SessionReady{request_id,user_name,payload,ts,session_path,evidence_path,request_ts}=>{
                            let page=protocol::parse_page(&payload,&user_name,None)?;
                            valid_file(&directory,&session_path)?; valid_file(&directory,&evidence_path)?;
                            let first=store.wb_source(&page.biz)?.is_none_or(|s|s.session_id.is_empty());
                            if !store.wb_discovery_capture(id,request_ts.unwrap_or(0.),&page,&config,&session_path,&evidence_path,ts)? { continue; }
                            if first {
                                if let Some((old_id,offset,observed))=store.wb_legacy_checkpoint(&page.biz,&page.user_name,i64::MAX)? {
                                    let protected=super::engine::seal_legacy_cursor(&directory,python,old_id,&offset,observed).await?;
                                    store.wb_import_legacy_checkpoint(&page.biz,&offset,&protected,observed)?;
                                }
                            }
                            store.wb_discovery_admit(id,&page.biz,true,request_ts.unwrap_or(ts))?;
                            store.wb_discovery_response(&request_id,&page.biz,"ok",now())?;
                            store.wb_discovery_state(id,"running",None)?;
                        }
                        Event::Diagnostic{reason}=>{
                            let message=match reason.as_str(){"ambiguous_callback"=>"观察到并发回调歧义，已跳过该响应，继续等待新的主页请求", "session_material_incomplete"=>"公众号已登记，但独立请求会话材料未齐全，等待下一次打开主页", _=>"观察到无法解析的桥消息，继续等待新的主页请求"};
                            store.wb_discovery_state(id,"running",Some(message))?;
                        }
                        Event::Fault{reason}=>{let _=reason; anyhow::bail!("观察器停止");}
                        Event::Stopped=>{anyhow::bail!("观察器提前停止");}
                    }
                }
            }
        }
        Ok(())
    }.await;
    store.wb_discovery_phase(id, "stop_requested")?;
    let _ = input.write_all(b"{\"action\":\"stop\"}\n").await;
    let _ = input.flush().await;
    let shutdown = async {
        loop {
            tokio::select! {
                status=child.wait()=>{ status?; return Ok::<_,anyhow::Error>(()); }
                line=lines.next_line()=>{
                    match line? {
                        Some(line) if line.len()<=8*1024*1024 => {
                            if let Ok(Event::Phase{phase})=serde_json::from_str(&line) {
                                if matches!(phase.as_str(),"unload_begin"|"unload_done"|"detached") { let _=store.wb_discovery_phase(id,&phase); }
                            }
                        }
                        _=>{child.wait().await?; return Ok(());}
                    }
                }
            }
        }
    };
    if tokio::time::timeout(Duration::from_secs(3), shutdown)
        .await
        .is_err()
    {
        let _ = store.wb_discovery_phase(id, "worker_timeout_kill");
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    let _ = store.wb_discovery_phase(id, "worker_reaped");
    result
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn real_worker_pipe_stores_multiple_homes_and_stops_without_creating_history_early() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let id = store.wb_discovery_start(None).unwrap();
        let root = std::env::temp_dir().join(format!(
            "mpider-discovery-pipe-{}",
            (now() * 1000000.) as u64
        ));
        std::fs::create_dir_all(&root).unwrap();
        let executable = root.join("fake-worker");
        let mut script = String::from("#!/bin/sh\necho '{\"event\":\"ready\"}'\n");
        for biz in ["FAKE_A", "FAKE_B"] {
            let request = format!("fake-request-{biz}");
            let body = json!({"BaseResponse":{"Ret":0},"AccountInfo":{"UserName":biz,"NickName":format!("虚构{biz}")},"MsgList":{"PagingInfo":{"Offset":"synthetic-only","IsEnd":0},"Msg":[{"AppMsg":{"BaseInfo":{"CreateTime":1000},"DetailInfo":[{"Title":"虚构文章","ContentUrl":format!("https://mp.weixin.qq.com/s?__biz={biz}&mid=1&idx=1")} ]}}]}});
            let payload = json!({"base_resp":{"ret":0},"err_msg":"H5ExtTransfer:ok","jsapi_resp":{"ret":0,"resp_json":body}});
            for event in [
                json!({"event":"observed_request","request_id":request,"ts":now()}),
                json!({"event":"home","request_id":request,"user_name":biz,"ts":now(),"payload":payload}),
            ] {
                script.push_str(&format!("echo '{event}'\n"));
            }
        }
        script.push_str("IFS= read -r command\n[ \"$command\" = '{\"action\":\"stop\"}' ] || exit 3\necho '{\"event\":\"phase\",\"phase\":\"unload_done\"}'\n");
        std::fs::write(&executable, script).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let runner = tokio::spawn(run(
            store.clone(),
            root.clone(),
            id,
            executable.display().to_string(),
        ));
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if store.wb_discovery().unwrap().unwrap().accounts.len() == 2 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(store.wb_sources().unwrap().len(), 2);
        assert_eq!(store.wb_snapshot().unwrap().list_calls_24h, 2);
        assert!(store.wb_tasks().unwrap().is_empty());
        store.wb_discovery_state(id, "stopping", None).unwrap();
        tokio::time::timeout(Duration::from_secs(5), runner)
            .await
            .unwrap()
            .unwrap();
        let final_run = store.wb_discovery().unwrap().unwrap();
        assert_eq!(final_run.state, "stopped");
        assert!(final_run.phases.iter().any(|p| p.phase == "worker_reaped"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
