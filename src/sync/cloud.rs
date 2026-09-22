//! 云端有哪几款游戏：跨机器可见性的那一层。
//!
//! 从前 kotori 只会在**已知 id** 的目录下列版本（`games/<id>/`），从来没有列过
//! `games/` 那一层 —— 于是第二台机器发现不了云端有什么，只有"两台机器给同一款
//! 游戏起的名字一字不差"时才能碰巧对上。这个模块补的就是"先问云端有哪几款"。
//!
//! 它是**云同步身份**那件事的第一步：先看得见（这里），再谈"哪一款对应哪一款"
//! （exe 指纹、身份卡、配对界面）。两条路在这一层说同一句话：
//!
//!   * rclone：`games/` 下的**目录名**就是一个游戏（一版一个 zip 摆在里面）；
//!   * kopia：整个仓库是不透明的一块，能认人的只有快照上的 `game:` **标签值**。
//!
//! 于是"云端这一款的标识"在两个引擎下都叫 [`CloudGame::id`]：rclone 那边它同时
//! 也是目录名，kopia 那边它就是标签值。第 3 步之后 rclone 的目录名可能为了避开
//! 占用而带后缀，那时"目录名"与"身份"才需要分开说。

use serde::Serialize;

/// 云端的一款游戏。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CloudGame {
    /// 云端这一款的标识（rclone 是目录名，kopia 是 `game:` 标签的值）。
    pub id: String,
    /// 有几版存档。kopia 那条**身份快照**不算版本，rclone 的身份卡也不是包。
    pub versions: usize,
}

/// 解析 `rclone lsf --dirs-only` 的输出：一行一个目录名。
///
/// rclone 默认给目录名加尾斜杠（`--dir-slash`），去掉它才是能用进路径的名字。
/// 空行与重复项一并收掉：`games/` 那一层不该有空白名字，而重名的目录本就不存在
/// ——真要出现，宁可只报一次也不要让界面上出现两行一样的东西。
pub fn parse_dirs(output: &str) -> Vec<String> {
    let mut dirs: Vec<String> = output
        .lines()
        .map(|line| line.trim().trim_end_matches('/'))
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect();
    dirs.sort();
    dirs.dedup();
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_listings_become_usable_names() {
        let output = "3days/\nlife-game/\n\nlong-name/\n3days/\n";
        assert_eq!(
            parse_dirs(output),
            vec![
                "3days".to_string(),
                "life-game".to_string(),
                "long-name".to_string()
            ],
            "尾斜杠要去掉，空行与重复项都不留"
        );
        assert!(parse_dirs("").is_empty(), "云端还没有游戏时是空的");
        // 没带尾斜杠也认（老 rclone、或者有人 `--dir-slash=false`）。
        assert_eq!(parse_dirs("solo\n"), vec!["solo".to_string()]);
    }
}
