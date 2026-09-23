//! 「目标签名」与「配对结论」：这一款游戏的配对是针对**哪个云目标**做的。
//!
//! 换 Bucket、换引擎、换 prefix 之后，上一次的结论就不再适用了 —— 但"要不要重扫一遍"
//! 不该由"用户点了一下设置"来决定（换凭证、改保留版本数都与目标无关），而该由**目标本身
//! 有没有变**来决定。签名就是那个"目标本身"：引擎 + endpoint + bucket + prefix。
//!
//! 签名**不进密钥**：它写进配置的 `cloud_conclusion` 里（`ok:<签名>` / `off:<签名>`），
//! 只在"打开游戏前"那一刻与当前签名比一次 —— 一样 ⇒ 不重扫、不问。
//!
//! 形状带版本号：将来判据变了就换 `v2:`，老结论从此对不上任何签名（等于"未定"），
//! 而不是与新签名碰巧相等。

use crate::config::SyncConfig;

/// 当前云目标的签名；**目标没配齐**（没有 bucket）就是 `None`。
///
/// 归一化是刻意的：`https://host/`、`https://host`、`/kotori/`、`kotori` 都是同一个
/// 目标，不该因为它们写法不同就让用户重新回答一次。
pub fn of(config: &SyncConfig) -> Option<String> {
    let bucket = config.bucket.trim();
    if bucket.is_empty() {
        return None;
    }
    let endpoint = config.endpoint.trim().trim_end_matches('/').to_lowercase();
    let prefix = config.prefix.trim().trim_matches('/');
    Some(format!(
        "v1:{}:{endpoint}:{bucket}:{prefix}",
        config.engine.slug()
    ))
}

/// 这一款在某个目标上走到哪一步了（存在 [`GameConfig::cloud_conclusion`] 里）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conclusion<'a> {
    /// 已确认：静默认领过、用户答过"没问题"、或者手动配对过。
    Confirmed(&'a str),
    /// 已问过，用户选了"关掉这一款的同步"（签名 = 在哪个目标上问的）。
    Declined(&'a str),
}

impl<'a> Conclusion<'a> {
    pub fn parse(raw: &'a str) -> Option<Self> {
        if let Some(rest) = raw.strip_prefix("ok:") {
            Some(Self::Confirmed(rest))
        } else {
            raw.strip_prefix("off:").map(Self::Declined)
        }
    }

    pub fn confirmed(signature: &str) -> String {
        format!("ok:{signature}")
    }

    pub fn declined(signature: &str) -> String {
        format!("off:{signature}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SyncEngine;

    fn settings(engine: SyncEngine, endpoint: &str, bucket: &str, prefix: &str) -> SyncConfig {
        SyncConfig {
            enabled: true,
            engine,
            endpoint: endpoint.to_string(),
            bucket: bucket.to_string(),
            prefix: prefix.to_string(),
            ..SyncConfig::default()
        }
    }

    #[test]
    fn a_target_without_a_bucket_has_no_signature() {
        assert_eq!(of(&settings(SyncEngine::Kopia, "", "", "kotori")), None);
        assert_eq!(of(&settings(SyncEngine::Kopia, "", "   ", "kotori")), None);
    }

    #[test]
    fn the_same_target_written_differently_has_the_same_signature() {
        let plain = of(&settings(SyncEngine::Kopia, "", "bkt", "kotori"));
        let sloppy = of(&settings(
            SyncEngine::Kopia,
            "https://api001.backblazeb2.com/",
            "bkt",
            "/kotori/",
        ));
        assert_eq!(
            of(&settings(
                SyncEngine::Kopia,
                "https://api001.backblazeb2.com",
                "bkt",
                "kotori"
            )),
            sloppy,
            "尾部斜杠、大小写都不该让目标变成另一个"
        );
        assert_ne!(plain, sloppy, "endpoint 不一样就是另一个目标");
    }

    #[test]
    fn engine_bucket_and_prefix_are_all_part_of_the_target() {
        let base = settings(SyncEngine::Kopia, "https://host", "bkt", "kotori");
        let signature = of(&base).unwrap();
        for other in [
            settings(SyncEngine::Rclone, "https://host", "bkt", "kotori"),
            settings(SyncEngine::Kopia, "https://host", "other", "kotori"),
            settings(SyncEngine::Kopia, "https://host", "bkt", "elsewhere"),
        ] {
            assert_ne!(of(&other).unwrap(), signature);
        }
    }

    #[test]
    fn a_conclusion_round_trips_and_keeps_its_kind() {
        let confirmed = Conclusion::confirmed("v1:kopia::bkt:kotori");
        let declined = Conclusion::declined("v1:kopia::bkt:kotori");
        assert_eq!(
            Conclusion::parse(&confirmed),
            Some(Conclusion::Confirmed("v1:kopia::bkt:kotori"))
        );
        assert_eq!(
            Conclusion::parse(&declined),
            Some(Conclusion::Declined("v1:kopia::bkt:kotori"))
        );
        // 认不出来的（手写的、上一个版本的）等于"未定"，而不是猜一个。
        assert_eq!(Conclusion::parse("随便写的"), None);
        assert_eq!(Conclusion::parse("v1:kopia::bkt:kotori"), None);
    }
}
