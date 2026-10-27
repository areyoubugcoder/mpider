//! 系统日志存储与导出：复用全局日志表，按模块和关联 ID 检索，业务提交与日志同事务。
use super::*;
use anyhow::{ensure, Context};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LogFilter {
    pub since: Option<f64>,
    pub until: Option<f64>,
    pub module: Option<String>,
    pub level: Option<String>,
    pub keyword: Option<String>,
    pub task_id: Option<i64>,
    pub experiment_id: Option<i64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SystemLogRow {
    pub id: i64,
    pub ts: f64,
    pub level: String,
    pub module: String,
    pub job_id: Option<i64>,
    pub message: String,
    pub context: Value,
}
#[derive(Debug, Serialize)]
pub struct LogPage {
    pub rows: Vec<SystemLogRow>,
    pub total: i64,
    pub next_before_id: Option<i64>,
    pub retention_days: i64,
    pub max_rows: i64,
}

/// 错误文本也经过字段和链接脱敏，兼容旧日志及前端异常中的 JSON/参数形式。
pub fn safe_text(text: &str) -> String {
    use std::sync::LazyLock;
    static SECRETS: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r#"(?i)\b(key|pass_ticket|uin|wxtoken|appmsg_token|psk|inner_enc_key|inner_dec_key|offset|cursor)["']?\s*[:=]\s*["']?[^\s,;&}"'<>]+"#).unwrap()
    });
    static URLS: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r#"(?i)https?://[^\s"'<>]+"#).unwrap());
    let value = SECRETS.replace_all(text, "$1=***");
    URLS.replace_all(&value, "[链接已省略]")
        .chars()
        .take(8000)
        .collect()
}

/// 只接收明确允许的标量分析字段，避免协议载荷、密钥或原始游标混入日志。
fn safe_context(value: Value) -> Value {
    let allowed = [
        "biz",
        "nickname",
        "task_id",
        "experiment_id",
        "discovery_id",
        "article_id",
        "request_id",
        "cursor_id",
        "next_cursor_id",
        "current_cursor_id",
        "cursor_age_seconds",
        "received_articles",
        "new_articles",
        "pages",
        "completed",
        "failed",
        "skipped",
        "partial",
        "requested",
        "queued",
        "already_downloaded",
        "already_queued",
        "duration_ms",
        "outcome",
        "state",
        "previous_state",
        "purpose",
        "category",
        "policy_version",
        "http_status",
        "business_ret",
        "outer_ret",
        "list_min_ms",
        "list_max_ms",
        "list_daily_budget",
        "body_gap_ms",
        "image_gap_ms",
        "cooldown_until",
        "scheduled_at",
        "role",
        "count",
        "format",
        "reason_code",
        "is_end",
        "age_reset",
        "source_page",
        "reason",
        "phase",
        "next_action_at",
        "verdict",
    ];
    Value::Object(
        value
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(k, v)| {
                if !allowed.contains(&k.as_str()) || v.is_array() || v.is_object() {
                    return None;
                }
                let v = match v {
                    Value::String(s) => {
                        Value::String(safe_text(&s.chars().take(500).collect::<String>()))
                    }
                    _ => v.clone(),
                };
                Some((k.clone(), v))
            })
            .collect(),
    )
}

/// 调用者可传入当前业务事务；任何失败一起回滚，不制造虚假的成功日志。
pub(super) fn write(
    conn: &Connection,
    module: &str,
    level: &str,
    message: &str,
    context: Value,
) -> Result<i64> {
    let context = safe_context(context);
    conn.execute(
        "INSERT INTO app_log(ts,level,stage,job_id,message,context) VALUES(?,?,?,?,?,?)",
        params![
            now(),
            level,
            module,
            context["task_id"].as_i64(),
            safe_text(message),
            serde_json::to_string(&context)?
        ],
    )?;
    let id = conn.last_insert_rowid();
    if id % 200 == 0 {
        conn.execute(
            "DELETE FROM app_log WHERE ts < ?",
            [now() - LOG_RETENTION_SECS as f64],
        )?;
        conn.execute("DELETE FROM app_log WHERE id <= (SELECT id FROM app_log ORDER BY id DESC LIMIT 1 OFFSET ?)", [LOG_MAX_ROWS])?;
    }
    Ok(id)
}
fn conditions(filter: &LogFilter) -> Result<(String, Vec<SqlValue>)> {
    ensure!(
        filter.since.is_none_or(f64::is_finite) && filter.until.is_none_or(f64::is_finite),
        "日志时间无效"
    );
    ensure!(
        filter.since.zip(filter.until).is_none_or(|(a, b)| a <= b),
        "开始时间不能晚于结束时间"
    );
    let mut sql = " FROM app_log WHERE 1=1".to_string();
    let mut args = Vec::new();
    for (column, op, value) in [("ts", ">=", filter.since), ("ts", "<=", filter.until)] {
        if let Some(value) = value {
            sql.push_str(&format!(" AND {column}{op}?"));
            args.push(SqlValue::Real(value));
        }
    }
    if let Some(module) = filter.module.as_deref().filter(|s| !s.is_empty()) {
        sql.push_str(" AND stage=?");
        args.push(SqlValue::Text(module.into()));
    }
    if let Some(level) = filter.level.as_deref().filter(|s| !s.is_empty()) {
        ensure!(
            ["info", "warn", "error", "debug"].contains(&level),
            "日志级别无效"
        );
        sql.push_str(" AND level=?");
        args.push(SqlValue::Text(level.into()));
    }
    if let Some(id) = filter.task_id {
        sql.push_str(" AND (job_id=? OR json_extract(context,'$.task_id')=?)");
        args.extend([SqlValue::Integer(id), SqlValue::Integer(id)]);
    }
    if let Some(id) = filter.experiment_id {
        sql.push_str(" AND json_extract(context,'$.experiment_id')=?");
        args.push(SqlValue::Integer(id));
    }
    if let Some(word) = filter.keyword.as_deref().filter(|s| !s.trim().is_empty()) {
        ensure!(word.len() <= 1000, "搜索词过长");
        sql.push_str(" AND (instr(lower(message),lower(?))>0 OR instr(lower(context),lower(?))>0)");
        args.extend([SqlValue::Text(word.into()), SqlValue::Text(word.into())]);
    }
    Ok((sql, args))
}
fn rows(conn: &Connection, sql: &str, args: Vec<SqlValue>) -> Result<Vec<SystemLogRow>> {
    let mut query = conn.prepare(sql)?;
    let result = query
        .query_map(params_from_iter(args), |r| {
            let text: String = r.get(6)?;
            Ok(SystemLogRow {
                id: r.get(0)?,
                ts: r.get(1)?,
                level: r.get(2)?,
                module: r.get(3)?,
                job_id: r.get(4)?,
                message: safe_text(&r.get::<_, String>(5)?),
                context: safe_context(serde_json::from_str(&text).unwrap_or_default()),
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(result)
}
const SELECT: &str = "SELECT id,ts,level,stage,job_id,message,context";
impl Store {
    pub fn system_log(
        &self,
        module: &str,
        level: &str,
        message: &str,
        context: Value,
    ) -> Result<i64> {
        write(&self.conn(), module, level, message, context)
    }
    pub fn system_logs(
        &self,
        filter: &LogFilter,
        before_id: Option<i64>,
        limit: i64,
    ) -> Result<LogPage> {
        self.system_logs_inner(filter, before_id, limit, 0)
    }
    pub fn system_logs_offset(
        &self,
        filter: &LogFilter,
        offset: i64,
        limit: i64,
    ) -> Result<LogPage> {
        ensure!(
            offset >= 0 && (1..=100).contains(&limit),
            "日志分页参数无效"
        );
        self.system_logs_inner(filter, None, limit, offset)
    }
    fn system_logs_inner(
        &self,
        filter: &LogFilter,
        before_id: Option<i64>,
        limit: i64,
        offset: i64,
    ) -> Result<LogPage> {
        let (base, args) = conditions(filter)?;
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let total = tx.query_row(
            &format!("SELECT COUNT(*){base}"),
            params_from_iter(args.clone()),
            |r| r.get(0),
        )?;
        let mut sql = format!("{SELECT}{base}");
        let mut args = args;
        if let Some(before) = before_id {
            sql.push_str(" AND id<?");
            args.push(SqlValue::Integer(before));
        }
        sql.push_str(" ORDER BY id DESC LIMIT ? OFFSET ?");
        let limit = limit.clamp(1, 500);
        args.push(SqlValue::Integer(limit + 1));
        args.push(SqlValue::Integer(offset));
        let mut rows = rows(&tx, &sql, args)?;
        let more = rows.len() > limit as usize;
        rows.truncate(limit as usize);
        let next_before_id = more.then(|| rows.last().unwrap().id);
        tx.commit()?;
        Ok(LogPage {
            rows,
            total,
            next_before_id,
            retention_days: LOG_RETENTION_SECS / 86400,
            max_rows: LOG_MAX_ROWS,
        })
    }
    /// 导出全部匹配记录，而非界面当前页；查询在单次 SQLite 读快照内完成。
    pub fn system_log_export(&self, filter: &LogFilter, format: &str) -> Result<(Vec<u8>, usize)> {
        ensure!(["json", "txt"].contains(&format), "请选择 JSON 或文本格式");
        let (base, args) = conditions(filter)?;
        let rows = rows(
            &self.conn(),
            &format!("{SELECT}{base} ORDER BY id ASC"),
            args,
        )?;
        let count = rows.len();
        let data = if format == "json" {
            serde_json::to_vec_pretty(
                &serde_json::json!({"version":1,"exported_at":now(),"filter":filter,"count":count,"retention_days":LOG_RETENTION_SECS/86400,"max_rows":LOG_MAX_ROWS,"logs":rows}),
            )?
        } else {
            let mut text = format!(
                "MPider 系统日志\n导出时间：{}\n筛选：{}\n记录数：{}\n保留：{} 天，最多 {} 条\n\n",
                crate::applog::format_local(now()),
                serde_json::to_string(filter)?,
                count,
                LOG_RETENTION_SECS / 86400,
                LOG_MAX_ROWS
            );
            for row in rows {
                text.push_str(&format!(
                    "{} [{}][{}] #{} {} {}\n",
                    crate::applog::format_local(row.ts),
                    row.level.to_uppercase(),
                    crate::applog::Stage::parse(&row.module).label(),
                    row.id,
                    row.message,
                    row.context
                ));
            }
            text.into_bytes()
        };
        Ok((data, count))
    }
    pub fn save_system_log_export(
        &self,
        filter: &LogFilter,
        format: &str,
        path: &Path,
    ) -> Result<usize> {
        let (data, count) = self.system_log_export(filter, format)?;
        crate::workbench::archive::atomic_write(path, &data).context("日志文件写入失败")?;
        self.system_log(
            "export",
            "info",
            "系统日志导出完成",
            serde_json::json!({"count":count,"format":format}),
        )?;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn filters_search_all_rows_and_export_beyond_first_page() {
        let s = Store::open_in_memory().unwrap();
        let start = now() - 1.;
        for n in 0..123 {
            s.system_log("history","info","页面保存成功",json!({"task_id":9,"request_id":format!("req-{n}"),"biz":"SYNTHETIC","received_articles":10,"new_articles":0})).unwrap();
        }
        s.system_log("archive", "error", "正文失败", json!({"task_id":9}))
            .unwrap();
        s.system_log("history", "warn", "请求失败", json!({"task_id":10}))
            .unwrap();
        let f = LogFilter {
            module: Some("history".into()),
            level: Some("info".into()),
            task_id: Some(9),
            since: Some(start),
            until: Some(now() + 1.),
            keyword: Some("synthetic".into()),
            ..Default::default()
        };
        let first = s.system_logs(&f, None, 100).unwrap();
        assert_eq!(first.total, 123);
        assert_eq!(first.rows.len(), 100);
        let second = s.system_logs(&f, first.next_before_id, 100).unwrap();
        assert_eq!(second.rows.len(), 23);
        assert!(second.next_before_id.is_none());
        assert!(first
            .rows
            .iter()
            .all(|a| second.rows.iter().all(|b| a.id != b.id)));
        let (data, count) = s.system_log_export(&f, "json").unwrap();
        let document: Value = serde_json::from_slice(&data).unwrap();
        assert_eq!(count, 123);
        assert_eq!(document["logs"].as_array().unwrap().len(), 123);
        assert_eq!(document["logs"][0]["context"]["received_articles"], 10);
        let (txt, count) = s.system_log_export(&f, "txt").unwrap();
        assert_eq!(count, 123);
        assert!(String::from_utf8(txt).unwrap().contains("[列表翻页]"));
        let no_match = LogFilter {
            keyword: Some("%_'; DROP TABLE app_log;".into()),
            ..Default::default()
        };
        assert_eq!(s.system_logs(&no_match, None, 100).unwrap().total, 0);
        assert_eq!(
            s.system_logs(
                &LogFilter {
                    since: Some(now() + 100.),
                    ..Default::default()
                },
                None,
                100
            )
            .unwrap()
            .total,
            0
        );
        assert!(s
            .system_logs(
                &LogFilter {
                    since: Some(2.),
                    until: Some(1.),
                    ..Default::default()
                },
                None,
                100
            )
            .is_err());
    }
    #[test]
    fn sensitive_fields_are_dropped_and_legacy_rows_survive_migration_and_restart() {
        let path = std::env::temp_dir().join(format!(
            "mpider-system-log-migration-{}-{}.db",
            std::process::id(),
            now()
        ));
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch("CREATE TABLE app_log(id INTEGER PRIMARY KEY AUTOINCREMENT,ts REAL NOT NULL,level TEXT NOT NULL,stage TEXT NOT NULL,job_id INTEGER,message TEXT NOT NULL);").unwrap();
            conn.execute(
                "INSERT INTO app_log(ts,level,stage,message) VALUES(?,'info','app','旧日志')",
                [now()],
            )
            .unwrap();
        }
        {
            let s = Store::open(&path).unwrap();
            s.init_db().unwrap();
            s.system_log("history","warn","失败 key=TOPSECRET",json!({"task_id":11,"request_id":"test-request","psk":"PRIVATE","cursor":"RAW-CURSOR","payload":{"secret":"PRIVATE"},"outcome":"timeout"})).unwrap();
        }
        let s = Store::open(&path).unwrap();
        let logs = s.system_logs(&LogFilter::default(), None, 100).unwrap();
        assert_eq!(logs.total, 2);
        assert_eq!(logs.rows[1].message, "旧日志");
        let (data, _) = s.system_log_export(&LogFilter::default(), "json").unwrap();
        let text = String::from_utf8(data).unwrap();
        assert!(!text.contains("TOPSECRET"));
        assert!(!text.contains("PRIVATE"));
        assert!(!text.contains("RAW-CURSOR"));
        assert_eq!(logs.rows[0].context["task_id"], 11);
        let output = path.with_extension("json");
        assert_eq!(
            s.save_system_log_export(&LogFilter::default(), "json", &output)
                .unwrap(),
            2
        );
        let exported: Value = serde_json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
        assert_eq!(exported["count"], 2);
        assert!(s
            .save_system_log_export(&LogFilter::default(), "csv", &output)
            .is_err());
        drop(s);
        let _ = std::fs::remove_file(output);
        let _ = std::fs::remove_file(path);
    }
    #[test]
    fn log_failure_rolls_back_business_transaction_and_parallel_ids_stay_separate() {
        let store = std::sync::Arc::new(Store::open_in_memory().unwrap());
        {
            let mut conn = store.conn();
            let tx = conn.transaction().unwrap();
            tx.execute("INSERT INTO wb_tasks(document) VALUES('{}')", [])
                .unwrap();
            write(&tx, "history", "info", "事务尚未提交", json!({"task_id":1})).unwrap();
            // 模拟业务失败：事务析构回滚，同时撤回日志。
        }
        assert!(store.wb_tasks().unwrap().is_empty());
        assert_eq!(store.count_logs().unwrap(), 0);
        let handles: Vec<_> = (1..=8)
            .map(|id| {
                let store = store.clone();
                std::thread::spawn(move || {
                    for _ in 0..10 {
                        store
                            .system_log(
                                "archive",
                                "info",
                                "并发任务记录",
                                json!({"task_id":id,"article_id":id*10}),
                            )
                            .unwrap();
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        for id in 1..=8 {
            let logs = store
                .system_logs(
                    &LogFilter {
                        task_id: Some(id),
                        ..Default::default()
                    },
                    None,
                    100,
                )
                .unwrap();
            assert_eq!(logs.total, 10);
            assert!(logs
                .rows
                .iter()
                .all(|r| r.context["article_id"] == id * 10 && r.job_id == Some(id)));
        }
    }
}

#[cfg(test)]
mod failure_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn logging_failure_prevents_false_success_and_sensitive_json_is_redacted() {
        let s = Store::open_in_memory().unwrap();
        s.conn().execute_batch("CREATE TRIGGER reject_system_log BEFORE INSERT ON app_log BEGIN SELECT RAISE(ABORT,'模拟日志写入失败'); END;").unwrap();
        assert!(s.wb_discovery_start(None).is_err());
        assert!(s.wb_discovery().unwrap().is_none());
        s.conn()
            .execute_batch("DROP TRIGGER reject_system_log;")
            .unwrap();
        let message = r#"异常 {"PSK":"PRIVATE","offset":"RAW","key":"SECRET"} https://example.test/s?key=OTHER"#;
        s.system_log(
            "app",
            "error",
            message,
            json!({"reason":message,"task_id":123}),
        )
        .unwrap();
        let (data, _) = s.system_log_export(&LogFilter::default(), "json").unwrap();
        let text = String::from_utf8(data).unwrap();
        for sensitive in ["PRIVATE", "RAW", "SECRET", "OTHER", "https://"] {
            assert!(!text.contains(sensitive), "{text}");
        }
    }
}
