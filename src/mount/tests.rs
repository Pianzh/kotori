//! 挂载查找的纯函数测试。只测模块自己,不碰真实挂载表。

use super::*;
use std::path::PathBuf;

/// 构造一张假的挂载表:`/dev/sda1` = uuid "AAAA"、`/dev/sdb1` = uuid "BBBB"。
/// mountinfo 的设备列是 major:minor 数字,不是 `/dev` 路径。
fn table(mountinfo: &str) -> MountTable {
    table_with(mountinfo, |device, _source| match device {
        "8:1" => Some("AAAA-1111".into()),
        "8:17" => Some("BBBB-2222".into()),
        _ => None,
    })
}

/// 用自定义的 uuid 对照表构造挂载表(重复 uuid、未知盘等场景)。
fn table_with(
    mountinfo: &str,
    uuid: impl Fn(&str, &std::path::PathBuf) -> Option<String>,
) -> MountTable {
    crate::mount::linux::parse(mountinfo, uuid)
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
fn a_disk_mounted_at_two_points_resolves_to_the_shorter_one() {
    // 同一块盘 bind 在两个位置:比如既挂 /r/w 又挂 /media/games。
    let mountinfo =
        format!("{BASE}40 27 8:1 /games /media/games rw,relatime shared:3 - ext4 /dev/sda1 rw\n");
    let table = table(&mountinfo);
    // sda1 的 root 是 /,所以 /r/w/games/Game 与 /media/games/Game 是同一块盘同一份数据。
    let reference = table
        .infer_canonical(Path::new("/r/w/games/Game/game.exe"))
        .unwrap();
    assert_eq!(reference.relative, PathBuf::from("games/Game/game.exe"));
    // 两个都能解析;规则是取挂载点更短(组件数最少)的那一个,结果确定。
    assert_eq!(
        table.resolve(&reference).unwrap(),
        Path::new("/media/games/Game/game.exe")
    );
}

#[test]
fn the_same_disk_remounting_elsewhere_keeps_references_working() {
    // 场景:外置盘今天挂在 /media/game-old,明天挪到 /media/game-new ——
    // 配置里存的是老挂载点生成的引用,换地方后照样能解析到新位置。
    let old = table("40 27 8:1 / /media/game-old rw,relatime shared:1 - ext4 /dev/sda1 rw\n");
    let reference = old
        .infer_canonical(Path::new("/media/game-old/Game/game.exe"))
        .unwrap();
    assert_eq!(reference.disk, "AAAA-1111");
    assert_eq!(reference.relative, PathBuf::from("Game/game.exe"));

    let fresh = table("40 27 8:1 / /media/game-new rw,relatime shared:1 - ext4 /dev/sda1 rw\n");
    assert_eq!(
        fresh.resolve(&reference).unwrap(),
        Path::new("/media/game-new/Game/game.exe")
    );
}

#[test]
fn btrfs_subvol_layout_keeps_the_subvol_root_in_references() {
    // btrfs 子卷:文件系统 root 是 /data/@(子卷根),挂在 /mnt/games。
    // 引用必须带上子卷前缀,否则恢复到另一个子卷就指错数据。
    let mountinfo = "40 27 8:1 /data/@ /mnt/games rw,relatime shared:1 - btrfs /dev/sda1 rw\n";
    let table = table(mountinfo);
    let reference = table
        .infer_canonical(Path::new("/mnt/games/demo/game.exe"))
        .unwrap();
    assert_eq!(reference.disk, "AAAA-1111");
    assert_eq!(reference.relative, PathBuf::from("data/@/demo/game.exe"));
    assert_eq!(
        table.resolve(&reference).unwrap(),
        Path::new("/mnt/games/demo/game.exe")
    );
}

#[test]
fn escaped_root_and_point_are_both_decoded() {
    // 子卷名与挂载点都带空格:转义解码不能只认挂载点。
    let mountinfo = format!(
        "{BASE}40 27 8:1 /data/My\\040Sub /mnt/My\\040Games rw,relatime shared:3 - btrfs /dev/sda1 rw\n"
    );
    let table = table(&mountinfo);
    let reference = table
        .infer_canonical(Path::new("/mnt/My Games/demo/game.exe"))
        .unwrap();
    assert_eq!(
        reference.relative,
        PathBuf::from("data/My Sub/demo/game.exe")
    );
    assert_eq!(
        table.resolve(&reference).unwrap(),
        Path::new("/mnt/My Games/demo/game.exe")
    );
}

#[test]
fn two_devices_with_the_same_uuid_are_refused() {
    // 克隆盘/两块盘共用同一个 uuid:谁都没法确定,明确报错而不是猜一个。
    let mountinfo = "\
40 27 8:1 / /media/a rw,relatime shared:1 - ext4 /dev/sda1 rw
41 27 8:2 / /media/b rw,relatime shared:2 - ext4 /dev/sdb1 rw
";
    let table = table_with(mountinfo, |_device, _source| Some("DUP-0000".into()));
    let reference = MountPath {
        disk: "DUP-0000".into(),
        relative: PathBuf::from("games"),
    };
    let error = table.resolve(&reference).unwrap_err();
    assert!(error.contains("DUP-0000"), "{error}");
    assert!(error.contains("多个设备"), "{error}");
}

#[test]
fn a_nested_mount_only_claims_paths_under_its_own_point() {
    // sdb1 挂在 /r/w/nested:它只吞 nested 之下的路径,/r/w/outer 仍归 sda1。
    let mountinfo =
        format!("{BASE}38 36 8:17 / /r/w/nested rw,relatime shared:4 - ext4 /dev/sdb1 rw\n");
    let table = table(&mountinfo);
    let inner = table
        .infer_canonical(Path::new("/r/w/nested/game/save"))
        .unwrap();
    assert_eq!(inner.disk, "BBBB-2222");
    assert_eq!(inner.relative, PathBuf::from("game/save"));

    let outer = table.infer_canonical(Path::new("/r/w/outer/game")).unwrap();
    assert_eq!(outer.disk, "AAAA-1111");
    assert_eq!(outer.relative, PathBuf::from("outer/game"));
}
