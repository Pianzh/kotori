//! 「云端存档」这一页的状态：云端有哪些游戏、点开的那一款有哪几版。
//!
//! 与 `sync`（本机的设置与凭据）分开：这一块**只读云端**，一个字都不改本机配置，而且它
//! 记的是"上次读索引时看到的样子"——云端会在别的机器上变，所以它天生是一张快照。
//!
//! 数据来源是**一个桶一份的云端索引**（`crate::sync::index`）：列这些游戏只读一次索引，
//! 不去遍历每一张身份卡。索引还没建过时（`indexed == false`）页面要提示去点一次「深度
//! 扫描云端」——那才是读所有卡的那条慢路。

use super::confirm::Confirmation;

/// 云端的一款游戏（`sync.cloud_list` 的一行）。
///
/// ⚠ `cloud_key` 是**云端落点**（rclone 是目录名，kopia 是 `game:` 标签值），不是本机
/// id：云端有而本机没有的游戏也要能列出来，它没有 id 可用。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CloudGameRow {
    pub cloud_key: String,
    pub cloud_id: String,
    /// **云端记下的游戏名**（不是落点 —— 那是机器话）。
    pub name: String,
    /// 有几台机器认领过它。
    pub machines: usize,
    pub versions: usize,
    /// 最近一版的版本名与它多大（字节；0 = 不知道）。
    pub latest: Option<String>,
    pub size: u64,
    /// 这台/那台机器用过的 exe 路径：只给人看、只给搜索用。
    pub exe_paths: Vec<String>,
    /// 本机哪一条档案认了它（空 = 本机没有对应的）。
    pub local_id: String,
    pub local_name: String,
    /// 本机明确否过这一条（配对表那笔账）。
    pub rejected: bool,
}

impl CloudGameRow {
    pub(in crate::ui) fn versions_label(&self) -> String {
        match self.versions {
            0 => "还没有存档".to_string(),
            count => format!("{count} 版"),
        }
    }

    /// 最近一版那一行：时间 + 大小。两边都不知道时如实说"不知道"。
    pub(in crate::ui) fn latest_label(&self) -> String {
        let Some(latest) = &self.latest else {
            return String::new();
        };
        let time = crate::sync::describe_stamp(latest);
        match self.size {
            0 => time,
            size => format!("{time} · {}", human_size(size)),
        }
    }

    /// 本机这一侧的状态：认了哪一条 / 本机没有 / 你之前说了不是它。
    pub(in crate::ui) fn local_label(&self) -> String {
        if self.rejected {
            return "你说过不是这一款".to_string();
        }
        if self.local_id.is_empty() {
            return "本机没有它".to_string();
        }
        format!("本机《{}》", self.local_name)
    }

    /// 这一行是不是这一个（搜索用：名字、落点、用过的 exe 路径）。
    ///
    /// ⚠ `exe_paths` 只在这里被用到 —— 它**不参与任何判断**（谁是谁只看指纹），也不写回
    /// 本机配置（见 `crate::sync::cloud::MachineIdentity::exe_paths`）。
    pub(in crate::ui) fn matches(&self, needle: &str) -> bool {
        let needle = needle.trim().to_lowercase();
        if needle.is_empty() {
            return true;
        }
        self.name.to_lowercase().contains(&needle)
            || self.cloud_key.to_lowercase().contains(&needle)
            || self
                .exe_paths
                .iter()
                .any(|path| path.to_lowercase().contains(&needle))
    }
}

/// 云端的一版存档（`sync.cloud_versions` 的一行）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CloudVersionRow {
    pub name: String,
    pub size: u64,
    pub time: String,
}

impl CloudVersionRow {
    /// 给人看的那一行：本机时区的时间。
    pub(in crate::ui) fn label(&self) -> String {
        if self.time.is_empty() {
            crate::sync::describe_stamp(&self.name)
        } else {
            // 引擎给的是 UTC RFC3339；换算与排版交给同一个函数，两边说法才一致。
            crate::sync::describe_stamp(&self.name)
        }
    }

    pub(in crate::ui) fn size_label(&self) -> String {
        match self.size {
            0 => "大小不知道".to_string(),
            size => human_size(size),
        }
    }
}

/// 一次"读云端清单"的结果 —— 清单本身，加上它**是从哪儿来的**。
///
/// 用户 2026-09-23 定的：索引要在本机查、界面要显示是什么时候拿到的、后台刷新失败也要
/// 让人知道。所以这三件事跟清单一起回给界面（见 `daemon::sync_rpc::index::IndexView`）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CloudListReply {
    /// 桶里建过索引没有。`false` = 还没建过（提示去深扫一次），与"云端没有游戏"不同。
    pub indexed: bool,
    /// 这一份来自**本机缓存**（这一趟没打网络）。
    pub from_cache: bool,
    /// 什么时候拿到的（`stamp` 形状，显示时走 `describe_stamp`）。
    pub cached_at: String,
    /// 最近一次刷新失败的原因（有的话）。
    pub refresh_error: Option<String>,
    pub rows: Vec<CloudGameRow>,
}

impl CloudListReply {
    /// 「这份清单是什么时候、从哪儿来的」—— 用户要的那句时间。
    pub(in crate::ui) fn source_label(&self) -> String {
        source_label(self.from_cache, &self.cached_at)
    }

    /// 后台刷新失败那句（没有就不说）。
    pub(in crate::ui) fn trouble_label(&self) -> Option<String> {
        trouble_label(self.refresh_error.as_deref())
    }
}

/// 「本机缓存 · 2026-09-23 10:15」/「刚从云端读的 · …」；不知道时间就什么都不说。
pub(in crate::ui) fn source_label(from_cache: bool, cached_at: &str) -> String {
    if cached_at.is_empty() {
        return String::new();
    }
    let when = crate::sync::describe_stamp(cached_at);
    if from_cache {
        format!("本机缓存 · {when}")
    } else {
        format!("刚从云端读的 · {when}")
    }
}

/// 后台刷新失败那句（没有就不说）。
pub(in crate::ui) fn trouble_label(error: Option<&str>) -> Option<String> {
    error.map(|error| format!("上次刷新失败: {error}"))
}

/// 把字节数写成给人看的一行（只保留一位小数）。
pub(in crate::ui) fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// 「云端存档」整页的状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudState {
    /// 云端有哪些游戏（未过滤）。
    pub rows: Vec<CloudGameRow>,
    /// 桶里**建过索引**没有。`false` = 还没建过（要提示去深扫一次），与"云端没有游戏"分得开。
    pub indexed: bool,
    /// 这一次运行里**读过一次**了吗（进页面自动读一次，之后要用户自己按刷新）。
    pub loaded_once: bool,
    pub loading: bool,
    /// 正在深度扫描（读所有身份卡，慢）。
    pub scanning: bool,
    /// 一句话状态（读完 / 出错）。空 = 什么都别说。
    pub msg: Option<String>,
    pub ok: bool,
    /// 搜索框里的字（本地过滤，不打网络）。
    pub search: String,
    /// 点开的那一款（云端落点）；`None` = 在列表上。
    pub open: Option<String>,
    /// 点开那一款的版本，最旧在前。
    pub versions: Vec<CloudVersionRow>,
    pub versions_loading: bool,
    /// 正等着二次确认的那件事（详情页底部那两颗"整款"按钮）。`None` = 没有弹窗。
    pending: Option<Confirmation>,
    /// 已经在路上的那件事：结果回来时要说对是哪一件事失败。
    inflight: Option<Confirmation>,
    /// 删除在路上（挡住连点第二下）。
    pub busy: bool,
}

impl Default for CloudState {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            indexed: false,
            loaded_once: false,
            loading: false,
            scanning: false,
            msg: None,
            // 还没出过错 —— 第一句话不该是红色。
            ok: true,
            search: String::new(),
            open: None,
            versions: Vec::new(),
            versions_loading: false,
            pending: None,
            inflight: None,
            busy: false,
        }
    }
}

impl CloudState {
    /// 搜索之后要显示的那些行。
    pub(in crate::ui) fn visible(&self) -> Vec<&CloudGameRow> {
        self.rows
            .iter()
            .filter(|row| row.matches(&self.search))
            .collect()
    }

    /// 云端这几款里，本机已经认了的有几款（"扫描完成"那句话要用）。
    pub(in crate::ui) fn matched(&self) -> usize {
        self.rows
            .iter()
            .filter(|row| !row.local_id.is_empty())
            .count()
    }

    /// 点开的那一款（在列表里找不到时是 `None`：刷新之后它可能已经不在了）。
    pub(in crate::ui) fn opened(&self) -> Option<&CloudGameRow> {
        let key = self.open.as_deref()?;
        self.rows.iter().find(|row| row.cloud_key == key)
    }

    /// 点开某一款：记下落点，并标成"正在读它的版本"。
    ///
    /// 返回 `true` = 要去云端问一次版本。**重复点同一款不会再问**（回包乱序的保护见
    /// `versions_loaded`）。
    ///
    /// 这里以前是"点一下展开、再点一下收起"（`toggle`）；用户 2026-09-26 定的是**进去是一整
    /// 页**，回来走「返回」，所以"收起"那一支没有存在的余地了 —— 列表上没有"开着的那一款"
    /// 可点。
    pub(in crate::ui) fn open(&mut self, key: &str) -> bool {
        if self.open.as_deref() == Some(key) {
            return false;
        }
        self.open = Some(key.to_string());
        self.versions.clear();
        self.versions_loading = true;
        // 换一款就把上一款的弹窗/忙收掉（它属于上一款）。
        self.pending = None;
        self.busy = false;
        true
    }

    /// 回到列表（详情页的「返回」）。
    pub(in crate::ui) fn back(&mut self) {
        self.open = None;
        self.versions.clear();
        self.versions_loading = false;
        self.pending = None;
        self.busy = false;
    }

    /// 点开的那一款，**可变**（删完之后要把表里那一行的数字改对）。
    fn open_row_mut(&mut self) -> Option<&mut CloudGameRow> {
        let key = self.open.clone()?;
        self.rows.iter_mut().find(|row| row.cloud_key == key)
    }

    /// 点了详情页底部那两颗"整款"按钮：只记下要问哪一件事，真正的动作等确认。
    pub(in crate::ui) fn delete_requested(&mut self, action: Confirmation) {
        self.pending = Some(action);
        self.msg = None;
        self.ok = true;
    }

    pub(in crate::ui) fn cancelled(&mut self) {
        self.pending = None;
    }

    /// 确认了：把这件事交出去，界面进入忙。
    pub(in crate::ui) fn confirmed(&mut self) -> Option<Confirmation> {
        let action = self.pending.take()?;
        self.busy = true;
        self.ok = true;
        self.msg = Some(format!("正在{}…", action.verb()));
        self.inflight = Some(action.clone());
        Some(action)
    }

    /// 删完了（成或不成）：顺手把本地这一份**改对**，不等下一次刷新。
    pub(in crate::ui) fn deleted(&mut self, result: Result<String, String>) {
        let action = self.inflight.take();
        self.busy = false;
        match result {
            Ok(summary) => {
                self.ok = true;
                self.msg = Some(summary);
                self.after_delete(action.as_ref());
            }
            Err(error) => {
                self.ok = false;
                let verb = action.as_ref().map_or("删除", Confirmation::verb);
                self.msg = Some(format!("{verb}失败: {error}"));
            }
        }
    }

    /// 删成之后本地怎么改：清空 → 详情里那几版空掉、表里那一款的版数改成 0；抹掉词条 →
    /// 这一款在云端整个没了，退回列表并把它从表里拿掉。
    ///
    /// 不等下一次刷新：这一页就摆在用户眼前，删完还挂着旧数字是最刺眼的一种错。
    fn after_delete(&mut self, action: Option<&Confirmation>) {
        match action {
            Some(Confirmation::ClearVersions) => {
                self.versions.clear();
                self.versions_loading = false;
                if let Some(row) = self.open_row_mut() {
                    row.versions = 0;
                }
            }
            Some(Confirmation::ForgetIdentity) => {
                let gone = self.open.take();
                self.versions.clear();
                self.versions_loading = false;
                if let Some(key) = gone {
                    self.rows.retain(|row| row.cloud_key != key);
                }
            }
            _ => {}
        }
    }

    /// 在**再下一层**（一个存档的管理页）删掉了某一版：那一行从详情列表里拿掉、表里那一款
    /// 的版数减一，并在这里说一句结果 —— 用户被送回这一页时看得见。
    pub(in crate::ui) fn forget_version(&mut self, version: &str, summary: String) {
        self.versions.retain(|row| row.name != version);
        self.versions_loading = false;
        if let Some(row) = self.open_row_mut() {
            row.versions = row.versions.saturating_sub(1);
        }
        self.ok = true;
        self.msg = Some(summary);
    }

    /// 弹窗现在要问的那件事。
    pub(in crate::ui) fn pending(&self) -> Option<&Confirmation> {
        self.pending.as_ref()
    }

    /// 某一款的版本回来了。
    ///
    /// ⚠ **只认还开着的那一款**：两份回包可能乱序（点开甲、又点开乙，甲的回包后到），
    /// 把甲的结果铺到乙底下就是给用户看错东西。
    pub(in crate::ui) fn versions_loaded(&mut self, key: &str, versions: Vec<CloudVersionRow>) {
        if self.open.as_deref() != Some(key) {
            return;
        }
        self.versions = versions;
        self.versions_loading = false;
    }

    /// 某一款的版本没列成。
    pub(in crate::ui) fn versions_failed(&mut self, key: &str, message: String) {
        if self.open.as_deref() != Some(key) {
            return;
        }
        self.versions.clear();
        self.versions_loading = false;
        self.msg = Some(message);
        self.ok = false;
    }

    /// 清单回来了：整张表换掉，顺手把点开的那一款收起来（它可能已经不在了）。
    ///
    /// ⚠ "这份清单什么时候拿到的"**不存在这里** —— 它属于那句话本身（`cloud_summary` 拼进
    /// `msg` 了）。这一页存的是"表里有什么"，不是"什么时候看的"。
    pub(in crate::ui) fn loaded(&mut self, reply: CloudListReply) {
        self.rows = reply.rows;
        self.indexed = reply.indexed;
        self.loaded_once = true;
        self.loading = false;
        self.open = None;
        self.versions.clear();
        self.versions_loading = false;
        self.pending = None;
        self.busy = false;
    }
}

#[cfg(test)]
// ⚠ 这是**文件模块**(`model/cloud.rs` 这种),它的子模块默认要放在同名目录下
// (`cloud/`);测试就住在同一个目录里,用 `#[path]` 指过去 —— 比为了一个测试文件
// 专门建目录清楚(照 `sync/cloud.rs`)。
#[path = "cloud_tests.rs"]
mod cloud_tests;
