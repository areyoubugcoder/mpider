//! 任务阶段计时 —— 记一条任务在各环节各花了多久（GUI「任务列表」耗时列 hover 展示，用于判断该调哪个环节）。
//!
//! 进程级单例（与 [`crate::applog`] 的 `set_job` 同一思路：整链互斥，同一时刻只有一条任务在跑），
//! 编排器在各转折点 [`mark`] 一下即可：**打点即开启新阶段、同时结束上一阶段**，阶段按时间顺序排成
//! 一串，同名阶段可重复出现（如看门狗重点种子后再次「打开微信浏览器」）。任务开始前发生的环节
//! （历史下一页 / 合成巡检批次）用 [`prelude`] 先记下，[`begin`] 时并入。[`finish`] 收口并取走全部阶段，
//! 编排器把它随 `jobs.phases_json` 落库。
//!
//! 只记「名称 + 毫秒」，不含任何微信接口参数。

use std::sync::Mutex;
use std::time::Instant;

use crate::model::JobPhase;

struct Recorder {
    job_id: i64,
    phases: Vec<JobPhase>,
    /// 进行中的阶段（名称 + 起点）。
    current: Option<(String, Instant)>,
}

/// 计时器本体（进程级单例用它；单测直接建一个本地实例，不受并行测试互相干扰）。
#[derive(Default)]
pub struct Tracker {
    /// 任务开始前记下的环节（`prelude`），`begin` 时并入。
    pending: Vec<JobPhase>,
    active: Option<Recorder>,
}

impl Tracker {
    pub fn prelude(&mut self, name: &str, ms: i64) {
        self.pending.push(JobPhase {
            name: name.to_string(),
            ms: ms.max(0),
        });
    }

    pub fn begin(&mut self, job_id: i64) {
        let phases = std::mem::take(&mut self.pending);
        self.active = Some(Recorder {
            job_id,
            phases,
            current: None,
        });
    }

    pub fn mark(&mut self, name: &str) {
        let Some(rec) = self.active.as_mut() else {
            return;
        };
        let now = Instant::now();
        if let Some((prev, at)) = rec.current.take() {
            rec.phases.push(JobPhase {
                name: prev,
                ms: now.duration_since(at).as_millis() as i64,
            });
        }
        rec.current = Some((name.to_string(), now));
    }

    /// 进行中的阶段名（没有任务在跑 / 还没打点则 `None`）。
    pub fn current(&self) -> Option<String> {
        self.active
            .as_ref()
            .and_then(|r| r.current.as_ref().map(|(n, _)| n.clone()))
    }

    pub fn finish(&mut self, job_id: i64) -> Vec<JobPhase> {
        let Some(mut rec) = self.active.take() else {
            return Vec::new();
        };
        if rec.job_id != job_id {
            return Vec::new();
        }
        if let Some((prev, at)) = rec.current.take() {
            rec.phases.push(JobPhase {
                name: prev,
                ms: at.elapsed().as_millis() as i64,
            });
        }
        rec.phases
    }
}

static STATE: Mutex<Tracker> = Mutex::new(Tracker {
    pending: Vec::new(),
    active: None,
});

fn lock() -> std::sync::MutexGuard<'static, Tracker> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// 记一个**任务开始前**就完成的环节（历史下一页 / 合成巡检批次），下一次 [`begin`] 并入。
pub fn prelude(name: &str, ms: i64) {
    lock().prelude(name, ms);
}

/// 开始记录一条任务（丢弃上一条未收口的记录）。
pub fn begin(job_id: i64) {
    lock().begin(job_id);
}

/// 打点：结束上一阶段（记下耗时）、开启名为 `name` 的新阶段。没有 `begin` 过则忽略。
pub fn mark(name: &str) {
    lock().mark(name);
}

/// 进行中的阶段名（GUI「运行状态」展示用；没有任务在跑则 `None`）。
pub fn current() -> Option<String> {
    lock().current()
}

/// 收口：结束进行中的阶段，取走该任务的全部阶段（按顺序）。`job_id` 对不上（理论上不会）返回空。
pub fn finish(job_id: i64) -> Vec<JobPhase> {
    lock().finish(job_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 打点顺序 = 阶段顺序；prelude 并入开头；finish 收掉进行中的阶段；同名可重复。
    #[test]
    fn test_phases_sequence() {
        let mut t = Tracker::default();
        t.prelude("获取任务", 120);
        t.begin(7);
        t.mark("启动代理");
        t.mark("打开微信浏览器");
        std::thread::sleep(std::time::Duration::from_millis(5));
        t.mark("接力抓凭证");
        t.mark("打开微信浏览器");
        t.mark("上报结果");
        let got = t.finish(7);
        let names: Vec<&str> = got.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "获取任务",
                "启动代理",
                "打开微信浏览器",
                "接力抓凭证",
                "打开微信浏览器",
                "上报结果"
            ]
        );
        assert_eq!(got[0].ms, 120);
        assert!(got[2].ms >= 5, "sleep 过的阶段应计入耗时");
        // 收口后再打点 / 再收口都是空操作
        t.mark("x");
        assert!(t.finish(7).is_empty());
        // begin 会吞掉上一条未收口的记录，prelude 只并入下一条；job_id 对不上返回空
        t.prelude("合成巡检批次", 3);
        t.begin(8);
        t.mark("采集文章列表");
        t.begin(9);
        assert!(t.finish(8).is_empty());
        assert!(t.finish(9).is_empty());
    }
}
