//! `sync.status` 与一次同步的结果:设置页「云同步」那一块,以及同步完事后那行总结。
//!
//! 凭据那一级的措辞(`credentials_label`)也住在这里 —— 它和 `SyncStatus` 同一份
//! 回包算出来,分开就得再把状态传一遍。

use super::*;

/// What the B2 section says about the stored credentials.
///
/// There is only ever **one** set of B2 keys (the daemon overwrites the two
/// entries), so the useful information is how many of its two halves are
/// actually there — never the values, which do not leave the store. `store` is
/// the *name* of the tier they are in right now: saying "密钥环" on a machine
/// that has none would promise persistence we do not have.
pub(in crate::ui) fn credentials_label(has_key_id: bool, has_app_key: bool, store: &str) -> String {
    let stored = usize::from(has_key_id) + usize::from(has_app_key);
    // ⚠ 打勾用 `√`(U+221A)而不是 `✓`(U+2713):后者在微软雅黑里**没有字形**,
    // 界面上渲染成豆腐块(用户 2026-09-18:「kopia 后面的字符无法正常显示」)。
    // `√` 在 GBK 里就有,雅黑与 Noto Sans CJK 都覆盖;同理 `✗`(U+2717)→ `×`。
    let mark = |saved: bool| if saved { "√" } else { "× 未保存" };
    format!(
        "{store}里现在有 {stored}/2 项：keyID {}，applicationKey {}。再次保存会覆盖上一套，\
         只进{store}，配置文件里没有任何明文。",
        mark(has_key_id),
        mark(has_app_key),
    )
}

pub(in crate::ui) fn parse_sync_status(value: &Value) -> Result<SyncStatus, String> {
    if value.get("settings").is_none() {
        return Err("守护进程没有返回同步设置".to_string());
    }
    let games = value
        .get("games")
        .and_then(|v| v.as_array())
        .map(|rows| {
            rows.iter()
                .map(|row| SyncGameRow {
                    id: str_field(row, "id"),
                    name: str_field(row, "name"),
                    locations: row.get("locations").and_then(|v| v.as_u64()).unwrap_or(0),
                    problem: row
                        .get("location_problem")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    last: row.get("last").and_then(|last| {
                        if last.is_null() {
                            return None;
                        }
                        let action = last.get("action").and_then(|v| v.as_str()).unwrap_or("");
                        let detail = last.get("detail").and_then(|v| v.as_str()).unwrap_or("");
                        let when = last
                            .get("at")
                            .and_then(|v| v.as_str())
                            .map(|at| at.chars().take(16).collect::<String>())
                            .unwrap_or_default();
                        let mark = if last.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
                            "√"
                        } else {
                            "×"
                        };
                        Some(format!("{mark} {when} {action} {detail}"))
                    }),
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(SyncStatus {
        settings: value.get("settings").cloned().unwrap_or(Value::Null),
        remote: str_field(value, "remote"),
        rclone: value
            .get("rclone")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        kopia: value
            .get("kopia")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        // 认不出来就按 rclone 算：这个字段是 2026-09-16 才加的，而老配置当年级的
        // 就是 rclone。页面显示错引擎会让人以为对面没存档。
        engine: match str_field(value, "engine").as_str() {
            "kopia" => "kopia".to_string(),
            _ => "rclone".to_string(),
        },
        kopia_prefix: str_field(value, "kopia_prefix"),
        keyring: value
            .get("keyring")
            .map(|keyring| str_field(keyring, "backend"))
            .unwrap_or_default(),
        store_kind: value
            .get("keyring")
            .and_then(|keyring| keyring.get("store"))
            .map(|store| str_field(store, "kind"))
            .unwrap_or_default(),
        store_locked: value
            .get("keyring")
            .and_then(|keyring| keyring.get("store"))
            .and_then(|store| store.get("locked"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        store_path: value
            .get("keyring")
            .and_then(|keyring| keyring.get("store"))
            .map(|store| str_field(store, "path"))
            .unwrap_or_default(),
        master_file: value
            .get("keyring")
            .map(|keyring| str_field(keyring, "secrets_file"))
            .unwrap_or_default(),
        min_master_password: value
            .get("keyring")
            .and_then(|keyring| keyring.get("min_master_password"))
            .and_then(|v| v.as_u64())
            .unwrap_or(8) as usize,
        ephemeral: value
            .get("keyring")
            .and_then(|v| v.get("ephemeral"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        secrets: string_list(value.get("secrets")),
        ready: value
            .get("ready")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        problem: value
            .get("problem")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        games,
    })
}

/// One line summarising a sync result: how many locations moved, or what broke.
pub(in crate::ui) fn describe_sync_outcome(value: &Value) -> String {
    let outcomes: Vec<&Value> = match (
        value.get("games").and_then(|v| v.as_array()),
        value.get("game").filter(|v| !v.is_null()),
    ) {
        (Some(games), _) => games.iter().collect(),
        (None, Some(game)) => vec![game],
        _ => vec![value],
    };

    let mut moved = 0usize;
    let mut skipped = 0usize;
    let mut problems = Vec::new();
    for outcome in &outcomes {
        if let Some(error) = outcome.get("error").and_then(|v| v.as_str()) {
            problems.push(error.to_string());
        }
        for location in outcome
            .get("locations")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            match location.get("action").and_then(|v| v.as_str()) {
                Some("skipped") => skipped += 1,
                Some("failed") => {}
                _ => moved += 1,
            }
        }
    }

    if !problems.is_empty() {
        return format!("失败：{}", problems.join("；"));
    }
    if moved == 0 {
        return format!("没有需要同步的变化（跳过 {skipped} 个位置）");
    }
    format!("完成：{moved} 个位置已同步，跳过 {skipped} 个")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_support::sync_status_fixture;

    #[test]
    fn parses_the_sync_status() {
        let status = sync_status_fixture();
        assert!(status.ready);
        assert!(!status.ephemeral);
        assert_eq!(status.store_kind, "system");
        assert!(!status.store_locked);
        assert_eq!(status.store(), CredentialStore::System);

        // 明文文件那一级(现在的默认):路径要能取到,不然页面说不出"存在哪"。
        let plain = parse_sync_status(&serde_json::json!({
            "keyring": {
                "backend": "明文凭据文件 /home/user/.config/kotori/credentials.json（权限 0600，只有你能读）",
                "ephemeral": false,
                "store": { "kind": "plain-file", "path": "/home/user/.config/kotori/credentials.json" },
                "secrets_file": "/home/user/.config/kotori/secrets.json",
                "min_master_password": 8,
            },
            "settings": {},
        }))
        .unwrap();
        assert_eq!(plain.store(), CredentialStore::Plain);
        assert_eq!(
            plain.store_path,
            "/home/user/.config/kotori/credentials.json"
        );
        assert!(
            status.master_file.ends_with("secrets.json"),
            "凭据会存到哪要一直有答案:{}",
            status.master_file
        );
        assert_eq!(status.min_master_password, 8);
        assert_eq!(status.remote, "kotori:kotori-saves/kotori");
        assert_eq!(status.rclone.as_deref(), Some("/usr/bin/rclone"));
        assert!(status.problem.is_none());
        assert!(status.keyring.contains("Secret Service"));

        assert_eq!(status.games.len(), 2);
        assert_eq!(status.games[0].locations, 2);
        let last = status.games[0].last_label();
        assert!(last.contains("√") && last.contains("上传"), "{last}");
        assert_eq!(status.games[1].last_label(), "还没同步过");

        // A malformed payload is an error, not a silently empty page.
        assert!(parse_sync_status(&Value::Null).is_err());
    }

    #[test]
    fn the_credentials_section_counts_what_is_stored() {
        // One pair of keys per account, so "how many" means "how many of the
        // two halves are there" — the values never come back from the daemon.
        assert!(credentials_label(true, true, "系统密钥环").contains("2/2"));
        assert!(credentials_label(true, false, "系统密钥环").contains("1/2"));
        let empty = credentials_label(false, false, "系统密钥环");
        assert!(empty.contains("0/2"), "{empty}");
        assert!(empty.contains("未保存"), "{empty}");
        assert!(empty.contains("覆盖"), "覆盖语义要写出来：{empty}");

        // 说哪一级就说那一级:没有密钥环的机器上不能写成"密钥环里"。
        let memory = credentials_label(true, true, CredentialStore::Session.name());
        assert!(memory.contains("本次会话的内存里现在有 2/2"), "{memory}");
        assert!(!memory.contains("密钥环"), "{memory}");

        let status = sync_status_fixture();
        assert!(status.has_secret("b2-key-id") && status.has_secret("b2-app-key"));
        assert!(!status.has_secret("b2-app-key-x"));
    }

    #[test]
    fn sync_outcomes_are_summarised_for_a_human() {
        // A bulk upload: one location moved, one skipped.
        let value = serde_json::json!({
            "ok": true,
            "games": [{
                "game_id": "demo",
                "name": "Demo",
                "ok": true,
                "locations": [
                    { "configured": "savedata", "local": "/g/savedata", "action": "uploaded", "detail": "已上传" },
                    { "configured": "%APPDATA%\\\\X", "local": "/w/X", "action": "skipped", "detail": "本地没有这个目录" }
                ]
            }]
        });
        let summary = describe_sync_outcome(&value);
        assert!(summary.contains("1 个位置已同步"), "{summary}");
        assert!(summary.contains("跳过 1"), "{summary}");

        // Nothing changed is not a failure.
        let value = serde_json::json!({
            "ok": true,
            "games": [{ "game_id": "demo", "name": "Demo", "ok": true, "locations": [] }]
        });
        assert!(describe_sync_outcome(&value).contains("没有需要同步的变化"));

        // A failure names the location that broke.
        let value = serde_json::json!({
            "ok": false,
            "game": {
                "game_id": "demo",
                "name": "Demo",
                "ok": false,
                "error": "savedata: rclone 执行失败",
                "locations": []
            }
        });
        let summary = describe_sync_outcome(&value);
        assert!(summary.contains("失败"), "{summary}");
        assert!(summary.contains("savedata"), "{summary}");
    }
}
