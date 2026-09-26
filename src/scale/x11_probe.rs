//! 从 gamescope 的内部 Xwayland 里**读出**游戏自己那块窗口的尺寸。
//!
//! `x11.rs` 说的是"往 gamescope 写"(滤镜/缩放/锐度),这里说的是"从 gamescope
//! 读"(窗口几何)—— 两者只有那个连接与根窗口是共用的,所以各住一个文件。
//!
//! 为什么要读:档案里的「游戏分辨率」从前只能靠用户手填,而 gamescope 一直知道
//! 答案 —— 它把当前聚焦的窗口 id 挂在根窗口的 `GAMESCOPE_FOCUSED_WINDOW` 上
//! (`steamcompmgr.cpp`)。谁启动游戏谁就能顺手把这个数填进档案(见
//! `crate::daemon::scale_probe`)。

use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, Window};

use super::{GamescopeDisplay, X11Error};

impl GamescopeDisplay {
    /// 这一局游戏自己画的是多大。
    ///
    /// 先问 gamescope 聚焦的是哪个窗口,它没答(或那个窗口还没映射出来)才自己去
    /// 根窗口下认人;两边都认不出来就是 `None` —— **不猜**,空着比填错好。
    ///
    /// `exe_name` 只用于兜底那一半(见 [`GamescopeDisplay::largest_window_size`])。
    pub fn game_window_size(&self, exe_name: &str) -> Result<Option<(u32, u32)>, X11Error> {
        if let Some(window) = self.focused_window()?
            && let Some(size) = self.window_size(window)?
        {
            return Ok(Some(size));
        }
        self.largest_window_size(exe_name)
    }

    /// gamescope 现在聚焦的那个窗口。
    ///
    /// 属性**可能不在**:游戏还没把窗口画出来的时候 gamescope 不会写它,而
    /// "没有属性"和"属性是 0"是同一件事(`0` 是 X 里的 `None` 窗口)。
    pub fn focused_window(&self) -> Result<Option<Window>, X11Error> {
        let reply = self
            .conn
            .get_property(
                false,
                self.root,
                self.atoms.GAMESCOPE_FOCUSED_WINDOW,
                AtomEnum::CARDINAL,
                0,
                1,
            )?
            .reply()?;
        Ok(reply
            .value32()
            .and_then(|mut values| values.next())
            .filter(|window| *window != 0))
    }

    /// 一个窗口的几何尺寸。宽或高是 0 的不算数:还没映射出来的窗口就是 0。
    pub fn window_size(&self, window: Window) -> Result<Option<(u32, u32)>, X11Error> {
        let geometry = self.conn.get_geometry(window)?.reply()?;
        Ok(usable_size(geometry.width, geometry.height))
    }

    /// 兜底:焦点属性没写的时候,自己在 gamescope 的根窗口下找一个。
    ///
    /// 先按 `WM_CLASS` 认人(exe 名去扩展名、忽略大小写),认不出来就取**面积最大**
    /// 的那一个 —— galgame 摆在前面的那个窗口通常就是最大的。这既是兜底,就宁可
    /// 空着:一个都认不出来时返回 `None`,不拿"反正有一个窗口"去冒充游戏分辨率。
    fn largest_window_size(&self, exe_name: &str) -> Result<Option<(u32, u32)>, X11Error> {
        let mut named: Option<((u32, u32), u64)> = None;
        let mut any: Option<((u32, u32), u64)> = None;
        for window in self.conn.query_tree(self.root)?.reply()?.children {
            let Some(size) = self.window_size(window)? else {
                continue;
            };
            keep_larger(&mut any, size);
            let classes = self.wm_class(window)?;
            if classes.iter().any(|class| matches_exe(class, exe_name)) {
                keep_larger(&mut named, size);
            }
        }
        Ok(named.or(any).map(|(size, _)| size))
    }

    /// 一个窗口的 `WM_CLASS`,按 NUL 拆成它那几个字符串(通常两个:instance 与 class)。
    fn wm_class(&self, window: Window) -> Result<Vec<String>, X11Error> {
        let reply = self
            .conn
            .get_property(
                false,
                window,
                AtomEnum::WM_CLASS,
                AtomEnum::STRING,
                0,
                256,
            )?
            .reply()?;
        Ok(split_class(&reply.value))
    }
}

/// 宽高里只要有一个是 0,这就不是一个尺寸。
fn usable_size(width: u16, height: u16) -> Option<(u32, u32)> {
    (width > 0 && height > 0).then_some((u32::from(width), u32::from(height)))
}

/// `WM_CLASS` 是一串 NUL 结尾的字符串,空的那几个不算名字。
fn split_class(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).to_string())
        .collect()
}

/// 一个 `WM_CLASS` 名字是不是这个 exe。
///
/// 比的是**去掉 `.exe` 之后的小写**:wine 给窗口设的类名与 exe 名常常一致,差的是
/// 大小写与那个扩展名。再宽一点允许互相包含(有的游戏会在类名里带上自己的副标题),
/// 但要求至少有 [`MIN_CLASS_LEN`] 个字符 —— 两三个字母的类名会把无关窗口也认进来。
fn matches_exe(class: &str, exe_name: &str) -> bool {
    const MIN_CLASS_LEN: usize = 4;

    let stem = |name: &str| name.trim().trim_end_matches(".exe").to_lowercase();
    let class = stem(class);
    let exe = stem(exe_name);
    if exe.is_empty() {
        return false;
    }
    class == exe
        || (class.chars().count() >= MIN_CLASS_LEN
            && (class.contains(&exe) || exe.contains(&class)))
}

/// 面积更大的那个留下。
fn keep_larger(best: &mut Option<((u32, u32), u64)>, size: (u32, u32)) {
    let area = u64::from(size.0) * u64::from(size.1);
    if best.is_none_or(|(_, best_area)| area > best_area) {
        *best = Some((size, area));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_sized_window_is_not_a_resolution() {
        assert_eq!(usable_size(0, 500), None);
        assert_eq!(usable_size(640, 0), None);
        assert_eq!(usable_size(640, 500), Some((640, 500)));
    }

    #[test]
    fn wm_class_splits_on_nuls_and_drops_the_empty_ones() {
        assert_eq!(split_class(b"game.exe\0Game\0"), ["game.exe", "Game"]);
        assert_eq!(split_class(b""), Vec::<String>::new());
    }

    #[test]
    fn an_exe_name_is_recognised_with_or_without_its_extension() {
        // 扩展名与大小写都不该让认人失败。
        assert!(matches_exe("game.exe", "game.exe"));
        assert!(matches_exe("Game", "game.exe"));
        assert!(matches_exe("GAME.EXE", "game"));
        // 副标题:类名比 exe 名长也算同一个。
        assert!(matches_exe("海猫鸣泣之时散语音版", "海猫鸣泣之时散语音版.exe"));
    }

    #[test]
    fn a_tiny_or_unrelated_class_is_not_a_match() {
        // 两三个字母的类名(wine 自己那些辅助窗口)不许认进来。
        assert!(!matches_exe("wine", "game.exe"));
        assert!(!matches_exe("exe", "game.exe"));
        assert!(!matches_exe("完全另一个游戏", "game.exe"));
        // 没有 exe 名可比的时候也不认人 —— 那会认成"谁都像"。
        assert!(!matches_exe("game.exe", ""));
    }

    #[test]
    fn the_biggest_window_wins() {
        let mut best = None;
        keep_larger(&mut best, (640, 500));
        keep_larger(&mut best, (1280, 720));
        keep_larger(&mut best, (400, 300));
        assert_eq!(best.map(|(size, _)| size), Some((1280, 720)));
    }
}
