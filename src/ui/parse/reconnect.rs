//! 掉线之后自动重连的等待时间:2s、4s、8s、16s,封顶 30s(上限见 `MAX_AUTO_RETRIES`)。
//!
//! 就一个纯函数,但它是一条完整的小生命周期(试到第几次 -> 等多久),和 JSON
//! 解析无关;`update` 里那台定时器直接用它。这里不需要从 `super` 取任何东西,
//! 所以没有 `use`。

/// Backoff for automatic reconnect attempts: 2s, 4s, 8s, 16s, capped at 30s.
pub(in crate::ui) fn retry_delay(attempt: u32) -> std::time::Duration {
    std::time::Duration::from_secs(2u64.pow(attempt.min(4)).min(30))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_backoff_grows_then_caps() {
        assert_eq!(retry_delay(1), std::time::Duration::from_secs(2));
        assert_eq!(retry_delay(2), std::time::Duration::from_secs(4));
        assert_eq!(retry_delay(3), std::time::Duration::from_secs(8));
        assert_eq!(retry_delay(4), std::time::Duration::from_secs(16));
        // capped, and never overflows for a large attempt count
        assert_eq!(retry_delay(5), std::time::Duration::from_secs(16));
        assert_eq!(retry_delay(99), std::time::Duration::from_secs(16));
    }
}
