//! 「云端存档」页在无显示环境下的整页渲染：列表 → 详情 → 各种空/错状态 → 搜索。
//!
//! 从 `window_test/mod.rs` 拆出来（那边已经装不下这几页了）。它接手时窗口在
//! **「云同步」页**上，跑完把页面切回去 —— 后面那段量凭据组宽度的断言还等着它。

use super::*;

pub(super) fn cloud_page(ui: &mut Ui) {
    // 「云端存档」是独立一页:还没读 → 读到了(一款展开着看每一版)→ 空云 → 报错。
    // ⚠ 这一页的状态在一个 Slint 全局里,所以要把它那两份模型指到 `Ui` 持有的这两份上
    // —— 否则 `push_model` 写的那一份窗口根本看不见,量出来是空树。
    show_tab(ui, Tab::Cloud);
    ui.window
        .global::<CloudBoard>()
        .set_rows(ui.cloud_rows.clone().into());
    ui.window
        .global::<CloudBoard>()
        .set_versions(ui.cloud_versions.clone().into());

    // ⚠ `ElementHandle` 只看得见**没被裁掉**的部分(`ItemRc::is_visible` 判的是裁剪矩形):
    // 740 高的窗口里凭据组刚好在折线以下,而这一页的内容也不止一屏 —— 所以量之前先把窗口
    // 撑高,否则查询会静默返回空,断言等于没写(这一点踩过一次)。
    ui.window
        .window()
        .set_size(slint::LogicalSize::new(1120.0, 2600.0));
    render(ui);
    fits(ui, &["CloudPage"]);

    ui.app.cloud = CloudState {
        indexed: true,
        rows: vec![
            CloudGameRow {
                cloud_key: "original-name".into(),
                cloud_id: "8f2c1234-0000-0000-0000-000000000000".into(),
                name: "云端记下的游戏名（很长很长的那种）".into(),
                machines: 2,
                versions: 3,
                latest: Some("20260911T101500Z".into()),
                size: 4 * 1024 * 1024,
                // 云端有、本机没有的那种:exe 路径也得能显示出来(而且很长)。
                exe_paths: vec![r"D:\Games\Some Very Long Folder Name\game.exe".into()],
                local_id: "renamed".into(),
                local_name: "Renamed".into(),
                rejected: false,
            },
            CloudGameRow {
                cloud_key: "only-on-the-other-machine".into(),
                cloud_id: "1111".into(),
                name: "本机没有的那一款".into(),
                machines: 1,
                versions: 0,
                latest: None,
                size: 0,
                exe_paths: Vec::new(),
                local_id: String::new(),
                local_name: String::new(),
                rejected: true,
            },
        ],
        msg: Some("云端 2 款游戏。点一款看它每一版。".into()),
        ..CloudState::default()
    };
    render(ui);
    // 最宽的形态:一行长名字 + 用过的 exe 路径。
    fits(ui, &["CloudPage"]);

    // 点开一款:详情(meta + exe 路径 + 每一版)三种状态都要能画。
    ui.app.cloud.toggle("original-name");
    ui.app.cloud.versions_loading = false;
    ui.app.cloud.versions = vec![
        CloudVersionRow {
            name: "20260910T090000Z".into(),
            size: 128 * 1024,
            time: "2026-09-10T09:00:00Z".into(),
        },
        CloudVersionRow {
            name: "20260911T101500123Z-1a2b3c4d".into(),
            size: 4 * 1024 * 1024,
            time: "2026-09-11T10:15:00Z".into(),
        },
    ];
    render(ui);
    fits(ui, &["CloudPage"]);

    // 版本还在路上 / 一款都没有 / 索引还没建过 / 读失败:四种都不许画成半截。
    ui.app.cloud.versions_loading = true;
    render(ui);
    ui.app.cloud.versions_loading = false;
    ui.app.cloud.versions.clear();
    render(ui);
    ui.app.cloud.back();
    ui.app.cloud.indexed = false;
    ui.app.cloud.rows.clear();
    ui.app.cloud.msg = Some("桶里还没有这份索引。".into());
    render(ui);
    ui.app.cloud.ok = false;
    ui.app.cloud.msg = Some("读云端索引失败: 连不上桶".into());
    render(ui);
    fits(ui, &["CloudPage"]);

    // 搜索:命中一行 / 一行都不命中(那句"没有匹配的"要能画出来)。
    ui.app.cloud = CloudState {
        indexed: true,
        rows: vec![CloudGameRow {
            cloud_key: "demo".into(),
            cloud_id: "2222".into(),
            name: "示例".into(),
            machines: 1,
            versions: 1,
            latest: Some("20260911T101500Z".into()),
            size: 4096,
            exe_paths: vec!["/games/demo/game.exe".into()],
            local_id: "demo".into(),
            local_name: "示例".into(),
            rejected: false,
        }],
        search: "zzz".into(),
        msg: Some("云端 1 款游戏。".into()),
        ..CloudState::default()
    };
    render(ui);
    fits(ui, &["CloudPage"]);
    ui.app.cloud.search.clear();
    render(ui);

    // 跑完把页面切回「云同步」：下面那一大段（凭据、连接、kopia）都是量它的。
    show_tab(ui, Tab::Sync);
    render(ui);
}
