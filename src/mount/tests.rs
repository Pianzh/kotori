//! 挂载查找的纯函数测试。只测模块自己,不碰真实挂载表。

use super::*;
use std::path::PathBuf;

/// 构造一张假的挂载表:`/dev/sda1` = uuid "AAAA"、`/dev/sdb1` = uuid "BBBB"。
/// mountinfo 的设备列是 major:minor 数字,不是 `/dev` 路径。
fn table(mountinfo: &str) -> MountTable {
    crate::mount::linux::parse(mountinfo, |device, _source| match device {
        "8:1" => Some("AAAA-1111".into()),
        "8:17" => Some("BBBB-2222".into()),
        _ => None,
    })
}

const BASE: &str = "\
36 27 8:1 / /r/w rw,relatime shared:1 - ext4 /dev/sda1 rw
37 27 8:17 / /media/extra rw,relatime shared:2 - ext4 /dev/sdb1 rw
";

#[test]
fn parse_reads_root_and_mountpoint() {
    let table = table(BASE);
    assert_eq!(table.entries.len(), 2);
    // 设备列是 major:minor，不是 /dev 路径。
    let root = table.entries.iter().find(|m| m.device == "8:1").unwrap();
    assert_eq!(root.root, PathBuf::from("/"));
    assert_eq!(root.point, PathBuf::from("/r/w"));
    assert_eq!(root.disk.as_deref(), Some("AAAA-1111"));
}

#[test]
fn tree_rooted_mount_keeps_its_root_in_references() {
    // root 非 / 的挂载（bind / subvolume）：引用里要带 root 前缀。
    let mountinfo =
        format!("{BASE}40 27 8:1 /bind /bind rw,relatime shared:3 - ext4 /dev/sda1 rw\n");
    let table = table(&mountinfo);
    let reference = table
        .infer_canonical(Path::new("/bind/Game/game.exe"))
        .unwrap();
    assert_eq!(reference.relative, PathBuf::from("bind/Game/game.exe"));
    assert_eq!(
        table.resolve(&reference).unwrap(),
        PathBuf::from("/bind/Game/game.exe")
    );
}

#[test]
fn escape_sequences_are_decoded() {
    // mountinfo 里空格是 \040,制表符是 \011。
    let mountinfo =
        format!("{BASE}40 27 8:1 / /media/My\\040Disk rw,relatime shared:3 - ext4 /dev/sda1 rw\n");
    let table = table(&mountinfo);
    let reference = table
        .infer_canonical(Path::new("/media/My Disk/Game"))
        .unwrap();
    assert_eq!(reference.relative, PathBuf::from("Game"));
    assert_eq!(
        table.resolve(&reference).unwrap(),
        PathBuf::from("/media/My Disk/Game")
    );
}

#[test]
fn nested_mount_wins_over_the_parent() {
    // sdb1 挂载在 sda1 的 /r/w 里面:同一路径要认最长的那个挂载点。
    let mountinfo =
        format!("{BASE}38 36 8:17 / /r/w/nested rw,relatime shared:4 - ext4 /dev/sdb1 rw\n");
    // /r/w/nested 是 /dev/sdb1 的 root（整盘 bind 在那），故引用落在 BBBB。
    let table = table(&mountinfo);
    let reference = table
        .infer_canonical(Path::new("/r/w/nested/data"))
        .unwrap();
    assert_eq!(reference.disk, "BBBB-2222");
    assert_eq!(reference.relative, PathBuf::from("data"));
}

#[test]
fn resolve_reports_a_disk_that_is_not_mounted() {
    let table = table(BASE);
    let reference = MountPath {
        disk: "DEAD-BEEF".into(),
        relative: PathBuf::from("games"),
    };
    let error = table.resolve(&reference).unwrap_err();
    assert!(error.contains("DEAD-BEEF"), "{error}");
}

#[test]
fn references_with_traversal_or_absolute_parts_are_rejected() {
    let table = table(BASE);
    let absolute = MountPath {
        disk: "AAAA-1111".into(),
        relative: PathBuf::from("/etc"),
    };
    let dotdot = MountPath {
        disk: "AAAA-1111".into(),
        relative: PathBuf::from("../x"),
    };
    assert!(absolute.validate().is_err());
    assert!(dotdot.validate().is_err());
    assert!(table.resolve(&absolute).is_err());
    assert!(table.resolve(&dotdot).is_err());

    let clean = MountPath {
        disk: "AAAA-1111".into(),
        relative: PathBuf::from("games/demo"),
    };
    assert_eq!(
        table.resolve(&clean).unwrap(),
        PathBuf::from("/r/w/games/demo")
    );
}

#[test]
fn infer_refuses_paths_without_a_known_disk() {
    let table = table(BASE);
    // 路径要真实存在才会被 canonicalize;不存在的路径没有归属,绝不能猜。
    let missing = Path::new("/r/w/does-not-exist-12345");
    assert!(table.infer(missing).is_none());
}

#[test]
fn round_trip_through_a_bind_point_keeps_the_same_path() {
    // 同一块盘 bind 在两个位置:比如既挂 /r/w 又挂 /media/games。
    let mountinfo =
        format!("{BASE}40 27 8:1 /games /media/games rw,relatime shared:3 - ext4 /dev/sda1 rw\n");
    let table = table(&mountinfo);
    // sda1 的 root 是 /,所以 /r/w/games/Game 与 /media/games/Game 是同一块盘同一份数据。
    let reference = table
        .infer_canonical(Path::new("/r/w/games/Game/game.exe"))
        .unwrap();
    assert_eq!(reference.relative, PathBuf::from("games/Game/game.exe"));
    // 两个候选长度相同,按确定性规则（字典序）选一个；两者都是同一份数据。
    assert!(matches!(
        table.resolve(&reference).unwrap(),
        path if path == PathBuf::from("/r/w/games/Game/game.exe")
            || path == PathBuf::from("/media/games/Game/game.exe")
    ));
}
