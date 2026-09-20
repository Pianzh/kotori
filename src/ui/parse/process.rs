//! `process.list` 的回包:能挑的进程一览。
//!
//! 与别的 `parse/*` 一样,这里只把 JSON 变成 UI 的结构体,不发请求(tasks 那边发)。
//! 两个入口共用这份结果:详情页的「跟这一局」只要 PID,添加游戏页要名字与 exe 路径。

use super::*;

/// `{"processes": [{pid, name, title, exe}]}` → [`ProcessRow`]。
///
/// 缺字段的行**跳过**而不是报错:列表是"此刻的进程表",某一项在回包与读取之间消失
/// 是正常的 —— 为了它把整张列表判失败,用户只会看到一个空浮层。
pub(in crate::ui) fn parse_processes(value: &Value) -> Result<Vec<ProcessRow>, String> {
    let list = value
        .get("processes")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "回包里没有 processes".to_string())?;

    Ok(list
        .iter()
        .filter_map(|process| {
            Some(ProcessRow {
                pid: process.get("pid")?.as_i64()? as i32,
                name: str_field(process, "name"),
                title: str_field(process, "title"),
                // `exe` 可能是 null(拿不到路径)—— `str_field` 会把它变成空串,
                // 那正是界面要的"这一项没有"。
                exe: str_field(process, "exe"),
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_process_list_becomes_rows_and_missing_paths_are_just_empty() {
        let value = serde_json::json!({
            "processes": [
                { "pid": 4321, "name": "Game.exe", "title": "BLACKSOULS", "exe": "/games/x/Game.exe" },
                { "pid": 99, "name": "GAEL.exe", "title": "", "exe": null },
                { "name": "没有 pid 的行" },
            ]
        });
        let rows = parse_processes(&value).unwrap();
        assert_eq!(rows.len(), 2, "没有 pid 的行跳过");
        assert_eq!(rows[0].pid, 4321);
        assert_eq!(rows[0].title, "BLACKSOULS");
        assert_eq!(rows[1].exe, "", "null 当空串 —— 界面据此说「拿不到路径」");
    }

    /// 形状不对(daemon 换了回包 / 拿错了回包)要说话,别默默给一个空列表 ——
    /// 那会让用户以为"一个进程都没有"。
    #[test]
    fn a_reply_without_the_key_is_an_error_not_an_empty_list() {
        assert!(parse_processes(&serde_json::json!({})).is_err());
        assert!(
            parse_processes(&serde_json::json!({ "processes": [] }))
                .unwrap()
                .is_empty()
        );
    }
}
