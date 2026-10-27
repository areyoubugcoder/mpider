//! 轻量随机数（不引 `rand`）：翻页随机等待、续期随机取样链接用。
//!
//! 只求"每次不一样、分布够散"，不求密码学强度——xorshift64* 以时间纳秒 + 递增计数为种子。

use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0x9E37_79B9_7F4A_7C15);

/// 一个 64 位伪随机数。
pub fn next_u64() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let c = COUNTER.fetch_add(0x2545_F491_4F6C_DD1D, Ordering::Relaxed);
    let mut x = nanos ^ c ^ 0xD6E8_FEB8_6659_FD93;
    if x == 0 {
        x = 0x1234_5678_9ABC_DEF1;
    }
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    x.wrapping_mul(0x2545_F491_4F6C_DD1D)
}

/// `[lo, hi]` 闭区间内的随机整数；`hi < lo` 时按 `lo`。
pub fn between(lo: u64, hi: u64) -> u64 {
    if hi <= lo {
        return lo;
    }
    lo + next_u64() % (hi - lo + 1)
}

/// 从切片里随机挑一个（空切片 → `None`）。
pub fn pick<T>(items: &[T]) -> Option<&T> {
    if items.is_empty() {
        return None;
    }
    items.get((next_u64() % items.len() as u64) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_between_in_range_and_varies() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            let v = between(10, 20);
            assert!((10..=20).contains(&v));
            seen.insert(v);
        }
        assert!(seen.len() > 3, "200 次里应出现多个不同取值");
        assert_eq!(between(5, 5), 5);
        assert_eq!(between(9, 3), 9);
    }

    #[test]
    fn test_pick() {
        assert!(pick::<u8>(&[]).is_none());
        let items = [1, 2, 3];
        for _ in 0..20 {
            assert!(items.contains(pick(&items).unwrap()));
        }
    }
}
