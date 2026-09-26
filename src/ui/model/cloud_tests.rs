//! `cloud` 的单元测试：清单、点开某一款、以及那一款详情里那两颗"整款"删除。
//!
//! 从 `cloud.rs` 拆出来 —— 500 行是软线（AGENTS.md），那个文件本来就在 460 上下，
//! 再加"删除"这一族就过线了。

use super::*;

fn row(key: &str, name: &str, local: &str) -> CloudGameRow {
    CloudGameRow {
        cloud_key: key.to_string(),
        cloud_id: format!("id-{key}"),
        name: name.to_string(),
        machines: 2,
        versions: 3,
        latest: Some("20260911T101500Z".to_string()),
        size: 4096,
        exe_paths: vec![format!("/games/{key}/game.exe")],
        local_id: local.to_string(),
        local_name: if local.is_empty() {
            String::new()
        } else {
            "本机那一款".to_string()
        },
        rejected: false,
    }
}

/// 云端的一版（`sync.cloud_versions` 的一行）。
fn version(name: &str, size: u64) -> CloudVersionRow {
    CloudVersionRow {
        name: name.to_string(),
        size,
        time: String::new(),
    }
}

fn board() -> CloudState {
    CloudState {
        rows: vec![
            row("demo", "示例游戏", "demo"),
            row("other", "别的一款", ""),
        ],
        indexed: true,
        ..CloudState::default()
    }
}

/// 「这份清单是什么时候拿到的」是用户 2026-09-23 要的那一句：缓存/刚读到两种说法，
/// 时间按本机时区印（长度固定，与跑测试的机器无关）；时间不知道（老回包）时就不印空时间。
#[test]
fn the_reply_says_when_this_list_was_fetched() {
    assert!(
        source_label(true, "20260923T101500Z").starts_with("本机缓存 · "),
        "{}",
        source_label(true, "20260923T101500Z")
    );
    assert!(
        source_label(false, "20260923T101500Z").starts_with("刚从云端读的 · "),
        "{}",
        source_label(false, "20260923T101500Z")
    );
    assert_eq!(source_label(true, ""), "", "不知道时间就别说时间");
    assert_eq!(trouble_label(None), None, "上一次是好的就别说话");
    assert_eq!(
        trouble_label(Some("连不上桶")).as_deref(),
        Some("上次刷新失败: 连不上桶")
    );
}

#[test]
fn a_new_board_is_quiet_and_not_red() {
    let fresh = CloudState::default();
    assert!(fresh.msg.is_none() && fresh.ok);
    assert!(!fresh.indexed && !fresh.loading && !fresh.scanning);
    assert!(fresh.visible().is_empty());
}

#[test]
fn each_row_says_how_many_versions_and_where_the_machine_stands() {
    let board = board();
    assert_eq!(board.rows[0].versions_label(), "3 版");
    assert_eq!(board.rows[1].local_label(), "本机没有它");
    assert_eq!(board.rows[0].local_label(), "本机《本机那一款》");
    // 时间按本机时区印出来（长度固定，与跑测试的机器无关）。
    assert_eq!(
        board.rows[0].latest_label().len(),
        "2026-09-11 18:15 · 4.0 KiB".len()
    );

    let mut rejected = board.rows[0].clone();
    rejected.rejected = true;
    assert_eq!(rejected.local_label(), "你说过不是这一款");
    // 一版都没有时那句"最近一版"是空的，不是"不知道"。
    let mut empty = board.rows[0].clone();
    empty.versions = 0;
    empty.latest = None;
    assert_eq!(empty.versions_label(), "还没有存档");
    assert_eq!(empty.latest_label(), "");
}

#[test]
fn sizes_are_written_the_way_people_read_them() {
    assert_eq!(human_size(0), "0 B");
    assert_eq!(human_size(512), "512 B");
    assert_eq!(human_size(4096), "4.0 KiB");
    assert_eq!(human_size(1024 * 1024), "1.0 MiB");
    assert_eq!(human_size(3 * 1024 * 1024 * 1024), "3.0 GiB");
    assert_eq!(CloudVersionRow::default().size_label(), "大小不知道");
}

#[test]
fn search_matches_the_name_the_key_and_the_exe_path() {
    let mut board = board();
    // 名字、落点、exe 路径都能搜到；大小写不敏感；空串等于不过滤。
    // （两行的 exe 路径里都有 `game.exe`，所以那条要用带落点的那一段来区分。）
    for needle in ["示例", "demo", "DEMO", "demo/game.exe"] {
        board.search = needle.to_string();
        assert_eq!(board.visible().len(), 1, "{needle}");
    }
    board.search = "game.exe".to_string();
    assert_eq!(board.visible().len(), 2, "两行的 exe 都叫 game.exe");
    board.search = "  ".to_string();
    assert_eq!(board.visible().len(), 2, "空串不过滤");
    board.search = "zzz".to_string();
    assert!(board.visible().is_empty());
}

#[test]
fn a_late_reply_never_lands_under_the_wrong_game() {
    let mut board = board();
    board.open("demo");
    board.open("other");
    // 甲的回包后到：必须丢掉，而不是铺到乙底下。
    board.versions_loaded("demo", vec![CloudVersionRow::default()]);
    assert!(board.versions.is_empty(), "{:?}", board.versions);
    assert!(board.versions_loading, "还在等乙的版本");

    board.versions_loaded(
        "other",
        vec![CloudVersionRow {
            name: "20260910T090000Z".into(),
            size: 128,
            time: String::new(),
        }],
    );
    assert_eq!(board.versions.len(), 1);
    assert!(!board.versions_loading);
    assert_eq!(board.versions[0].size_label(), "128 B");

    // 报错走同一条判据：别人的错不该挂在这一款上。
    board.versions_failed("demo", "列的途中断了".into());
    assert!(board.ok && board.msg.is_none());
    board.versions_failed("other", "列的途中断了".into());
    assert!(!board.ok && board.versions.is_empty());

    // 「返回」回到列表：开着的那一款与它的版本一起清掉。
    board.back();
    assert_eq!(board.open, None);
    assert!(board.opened().is_none());
}

#[test]
fn refreshing_closes_whatever_was_open() {
    let mut board = board();
    board.open("demo");
    board.loaded(CloudListReply {
        indexed: true,
        rows: vec![row("demo", "示例游戏", "demo")],
        ..CloudListReply::default()
    });
    assert!(board.indexed && !board.loading);
    assert_eq!(board.open, None, "刚刷新过，那一款可能已经不在了");
    assert!(board.versions.is_empty());
    assert!(board.opened().is_none());
}

/// 详情页底部那两颗"整款"按钮：先问、再动手，动完**本地那一份也要跟着对**。
#[test]
fn the_two_whole_game_deletes_fix_the_local_table_too() {
    let mut board = board();
    board.open("demo");
    board.versions_loaded(
        "demo",
        vec![
            version("20260901T000000Z", 128),
            version("20260911T101500Z", 256),
        ],
    );

    // ① 清空这一款的存档：详情里那几版空掉，表里那一款的版数改成 0，身份还开着。
    board.delete_requested(Confirmation::ClearVersions);
    assert_eq!(board.pending(), Some(&Confirmation::ClearVersions));
    assert!(!board.busy, "确认之前不该在路上");
    board.cancelled();
    assert!(board.pending().is_none());
    assert_eq!(board.confirmed(), None, "取消了就确认不出东西");

    board.delete_requested(Confirmation::ClearVersions);
    assert_eq!(board.confirmed(), Some(Confirmation::ClearVersions));
    assert!(board.busy && board.msg.as_deref() == Some("正在清空…"));
    board.deleted(Ok("已清空这一款的云端存档（删掉 2 版），身份留着".into()));
    assert!(!board.busy && board.ok);
    assert!(board.versions.is_empty());
    assert_eq!(board.open.as_deref(), Some("demo"), "身份还开着");
    assert_eq!(board.opened().unwrap().versions, 0, "表里那一行要说 0 版");
    assert_eq!(board.rows.len(), 2, "这一款还在云端，只是没有存档了");

    // ② 抹掉词条：这一款整个没了 —— 退回列表，并从表里拿掉那一行。
    board.versions_loaded("demo", vec![version("20260911T101500Z", 256)]);
    board.delete_requested(Confirmation::ForgetIdentity);
    assert_eq!(board.confirmed(), Some(Confirmation::ForgetIdentity));
    board.deleted(Ok("已把这一款从云端抹掉（1 版存档连身份一起）".into()));
    assert_eq!(board.open, None, "云端都不认识它了，不该还停在它的详情页");
    assert!(board.opened().is_none());
    assert_eq!(board.rows.len(), 1);
    assert_eq!(board.rows[0].cloud_key, "other");
    assert!(board.msg.as_deref().unwrap().contains("抹掉"));
}

/// 删不成：一个字都不许改（云端还在，界面就别说它没了），失败那句话还要说对是哪件事。
#[test]
fn a_failed_delete_changes_nothing() {
    let mut board = board();
    board.open("demo");
    board.versions_loaded("demo", vec![version("20260911T101500Z", 256)]);

    board.delete_requested(Confirmation::ClearVersions);
    board.confirmed();
    board.deleted(Err("连不上桶".into()));
    assert!(!board.ok && !board.busy);
    assert_eq!(board.versions.len(), 1, "没删成就不许动列表");
    assert_eq!(board.opened().unwrap().versions, 3, "表里那一行也是原样");
    assert!(
        board.msg.as_deref().unwrap().starts_with("清空失败: "),
        "{:?}",
        board.msg
    );
}

/// 再下一层删掉一版：那一行从详情里拿掉、表里的版数减一，结果那句话写在**这一页**上
/// （用户被送回来时看得见）。
#[test]
fn a_version_deleted_one_level_down_is_reflected_here() {
    let mut board = board();
    board.open("demo");
    board.versions_loaded(
        "demo",
        vec![
            version("20260901T000000Z", 128),
            version("20260911T101500Z", 256),
        ],
    );

    board.forget_version(
        "20260901T000000Z",
        "已删掉云端那一版，这一款还剩 1 版".into(),
    );
    assert_eq!(board.versions.len(), 1);
    assert_eq!(board.versions[0].name, "20260911T101500Z");
    assert_eq!(board.opened().unwrap().versions, 2, "表里那一行也要减一");
    assert!(board.msg.as_deref().unwrap().contains("还剩 1 版"));
    assert!(board.ok);
}
