//! rclone 失败时说人话：剥掉日志前缀，认出最常见的几种"设置还没弄好"，附一句
//! 该去检查什么，同时把 rclone 原话留着。
//!
//! 单独成文件，是因为它只做文本解释：跑进程、超时和退出码都在
//! `Runner::run_with` 里，这里不碰任何 I/O。

/// Turn rclone's stderr into something the user can act on.
///
/// The common failures are all "the setup is not right yet", and rclone's own
/// wording ("failed to authenticate: Unknown 401  (401 bad_auth_token)") does
/// not say which of the three values to go and check. The original text is kept
/// so nothing is hidden from the user.
pub(super) fn explain_failure(stderr: &str) -> String {
    let detail = clean_stderr(stderr);
    let lower = detail.to_lowercase();

    let hint = if lower.contains("bad_auth_token")
        || lower.contains("401")
        || lower.contains("unauthorized")
    {
        Some(
            "B2 不认这组凭据。检查 keyID 是不是 Application Key ID（形如 005a…，不是账号 ID），\
             以及 applicationKey 有没有完整复制",
        )
    } else if lower.contains("403") || lower.contains("forbidden") || lower.contains("not allowed")
    {
        Some(
            "凭据有效，但这个 key 没有这个 bucket 的权限。创建 Application Key 时要勾上该 bucket，\
             并把 Type of Access 选成 Read and Write",
        )
    } else if lower.contains("bucket")
        && (lower.contains("not found")
            || lower.contains("does not exist")
            || lower.contains("no such"))
    {
        Some("找不到这个 bucket：检查名字有没有写错，以及 Application Key 是否授权了它")
    } else if lower.contains("no such host")
        || lower.contains("connection refused")
        || lower.contains("timeout")
        || lower.contains("dial tcp")
        || lower.contains("tls")
    {
        Some("连不上 B2：检查网络、代理或 DNS 设置")
    } else {
        None
    };

    match hint {
        Some(hint) => format!("{hint}\n（rclone 原话：{detail}）"),
        None => detail,
    }
}

/// kopia 的 stderr 也按"该去检查什么"解释(BUG-7,2026-09-19 实测:直接透传的
/// `can't connect to storage: bucket not found` 让"改了桶名"看起来像"没生效")。
///
/// B2 那边的报错两家措辞接近,hint 共用一套;差别在 kopia 的输出是**多行**的
/// (B2 弃用警告 + 真错误),不走 rclone 的"取最后一行",也不剥时间戳前缀。
pub(super) fn explain_kopia_failure(stderr: &str) -> String {
    let detail = clean_stderr(stderr);
    let lower = detail.to_lowercase();

    let hint = if lower.contains("bucket")
        && (lower.contains("not found")
            || lower.contains("does not exist")
            || lower.contains("no such"))
    {
        Some(
            "找不到这个 bucket：到 B2 控制台核对桶名有没有写错（区分大小写），\
             以及 Application Key 是否授权了它",
        )
    } else if lower.contains("unable to authenticate")
        || lower.contains("unauthorized")
        || lower.contains("401")
    {
        Some(
            "B2 不认这组凭据。检查 keyID 是不是 Application Key ID（形如 005a…，不是账号 ID），\
             以及 applicationKey 有没有完整复制",
        )
    } else if lower.contains("invalid password") || lower.contains("wrong password") {
        Some(
            "仓库密码不对：kopia 的仓库密码默认是 kotori，或你在设置页里自己设的那个;\
             双系统/多机必须用同一个",
        )
    } else if lower.contains("no such host")
        || lower.contains("connection refused")
        || lower.contains("timeout")
        || lower.contains("dial tcp")
        || lower.contains("tls")
    {
        Some("连不上 B2：检查网络、代理或 DNS 设置")
    } else {
        None
    };

    match hint {
        Some(hint) => format!("{hint}\n（kopia 原话：{detail}）"),
        None => detail,
    }
}

/// The last non-empty line of rclone's output, with its log prefix removed.
fn clean_stderr(stderr: &str) -> String {
    stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .rfind(|_| true)
        .map(strip_rclone_log_prefix)
        .unwrap_or_default()
}

/// `2026/09/11 23:54:35 CRITICAL: message` -> `message`.
fn strip_rclone_log_prefix(line: &str) -> String {
    let mut rest = line.trim();
    let starts_with_timestamp = rest.len() > 20
        && rest.is_char_boundary(20)
        && rest[..10].chars().all(|c| c.is_ascii_digit() || c == '/')
        && rest[10..11] == *" "
        && rest[11..19].chars().all(|c| c.is_ascii_digit() || c == ':');
    if starts_with_timestamp {
        rest = rest[20..].trim_start();
    }
    for level in [
        "CRITICAL: ",
        "ERROR : ",
        "ERROR: ",
        "WARNING: ",
        "NOTICE: ",
        "INFO  : ",
        "INFO : ",
        "DEBUG : ",
    ] {
        if let Some(tail) = rest.strip_prefix(level) {
            return tail.trim().to_string();
        }
    }
    rest.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rclone_failures_are_explained_in_terms_of_what_to_check() {
        // Captured from the real rclone against B2 with made-up credentials.
        let unauthorized = "2026/09/11 23:51:56 CRITICAL: Failed to create file system for \
\"kotori:kotori-saves/kotori\": failed to authorize account: failed to authenticate: \
Unknown 401  (401 bad_auth_token)";
        let explained = explain_failure(unauthorized);
        assert!(explained.contains("Application Key ID"), "{explained}");
        assert!(
            explained.contains("bad_auth_token"),
            "the original must survive: {explained}"
        );
        assert!(
            !explained.contains("CRITICAL"),
            "log noise is stripped: {explained}"
        );

        let forbidden = "2026/09/11 10:00:00 ERROR : bucket is not allowed: 403 forbidden";
        assert!(explain_failure(forbidden).contains("Read and Write"));

        let missing = "2026/09/11 10:00:00 CRITICAL: bucket kotori-saves not found";
        assert!(explain_failure(missing).contains("bucket"));

        let offline =
            "2026/09/11 10:00:00 CRITICAL: dial tcp: lookup api.backblazeb2.com: no such host";
        assert!(explain_failure(offline).contains("网络"));

        // Anything unrecognised is passed through as-is, minus the log prefix.
        let other = "2026/09/11 10:00:00 NOTICE: something else happened";
        assert_eq!(explain_failure(other), "something else happened");
        assert_eq!(clean_stderr("\n\n"), "");
    }
}
