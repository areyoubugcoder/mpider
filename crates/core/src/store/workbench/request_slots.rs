//! 文章网页的并发工作位：空闲工作位立即发送，响应结束后再计算该位的间隔。
use super::*;
#[cfg(test)]
mod tests;

pub(super) fn available(
    tx: &Transaction<'_>,
    workers: usize,
    gap_ms: u64,
    ts: f64,
) -> Result<(Option<i64>, f64)> {
    for slot in 0..workers {
        tx.execute(
            "INSERT OR IGNORE INTO wb_article_slots(slot) VALUES(?)",
            [slot as i64],
        )?;
    }
    // 降低并发后，旧的高位请求仍占用总并发，直到实际结束。
    let active: usize = tx.query_row(
        "SELECT COUNT(*) FROM wb_article_slots WHERE request_id IS NOT NULL",
        [],
        |r| r.get(0),
    )?;
    if active >= workers {
        return Ok((None, ts + 0.1));
    }
    let free: Option<(i64, Option<f64>)> = tx.query_row(
        "SELECT slot,finished_at FROM wb_article_slots WHERE slot<? AND request_id IS NULL ORDER BY COALESCE(finished_at,0),slot LIMIT 1",
        [workers as i64], |r| Ok((r.get(0)?, r.get(1)?)),
    ).optional()?;
    let Some((slot, finished)) = free else {
        return Ok((None, ts + 0.1));
    };
    let ready = finished.map_or(ts, |v| v + gap_ms as f64 / 1000.);
    // 有请求尚未结束时继续短轮询，防止它先结束后出现更早可用的工作位。
    let next = if active > 0 {
        ready.min(ts + 0.1)
    } else {
        ready
    };
    Ok(((ready <= ts).then_some(slot), next))
}
