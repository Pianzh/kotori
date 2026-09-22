//! 打包：把 [`gather`] 收来的东西写成一个 zip，并在包根放一份清单。
//!
//! 只做"装进去"这一半：要不要覆盖、谁更新，全在 `unpack` 那边——那边才是会动
//! 本机数据的地方。收集规则在 `gather`，与 kopia 那条路（摆成目录）共用一份。

use std::fs::File;
use std::io::{BufReader, Write};
use std::path::Path;

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use super::gather::gather;
use super::{Entry, FORMAT, MANIFEST, Manifest};
use crate::sync::SaveTarget;
use crate::sync::cloud::PackIdentity;

/// 打包过程中攒下来的东西：一个包的内容，以及它没能包含谁。
#[derive(Debug, Clone)]
pub struct PackReport {
    /// 进了包的文件，按存档位置和相对路径排好序。
    pub entries: Vec<Entry>,
    /// 这一版包含哪些存档位置（目录在，哪怕空着）。
    pub locations: Vec<String>,
    /// 本机根本没有的存档位置。报告成 `skipped`，不是错误：一台机器上没装
    /// 某个位置（只在 Windows 上才有的那个目录）是正常的。
    pub missing: Vec<String>,
    /// 打包时被排除规则挡下的文件数，只用于日志。
    pub excluded: usize,
}

/// 把 `targets` 里存在的每个存档位置打进 `zip_path`。
///
/// `now` 只用来写清单里的 `created`，由调用方传进来，测试才好断言。
/// `identity` 是**这一版是谁传的**：它有值，取回时才有资格比对；没有就如实留空
/// （那种包在自动取回那条路上会被拒绝，见 [`crate::sync::cloud::identity_match`]）。
pub fn pack(
    zip_path: &Path,
    targets: &[SaveTarget],
    now: chrono::DateTime<chrono::Utc>,
    identity: Option<&PackIdentity>,
) -> Result<PackReport, String> {
    let gathered = gather(targets)?;
    let manifest = Manifest {
        format: FORMAT,
        created: now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        locations: gathered.locations,
        identity: identity.cloned(),
        entries: gathered.entries,
    };

    let file = File::create(zip_path)
        .map_err(|e| format!("无法创建存档包 {}: {e}", zip_path.display()))?;
    let mut writer = ZipWriter::new(file);
    // deflate：Windows 资源管理器双击就能打开，压缩率也够。加密在 zip 这条路上
    // 不存在——想保护就选 kopia。
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    for (key, absolute, relative) in &gathered.files {
        let name = format!("{key}/{relative}");
        writer
            .start_file(name.clone(), options)
            .map_err(|e| format!("打包 {name} 失败: {e}"))?;
        let mut source = BufReader::new(
            File::open(absolute).map_err(|e| format!("读取 {} 失败: {e}", absolute.display()))?,
        );
        std::io::copy(&mut source, &mut writer).map_err(|e| format!("写入 {name} 失败: {e}"))?;
    }

    let text =
        serde_json::to_string_pretty(&manifest).map_err(|e| format!("清单序列化失败: {e}"))?;
    writer
        .start_file(MANIFEST, options)
        .map_err(|e| format!("写入清单失败: {e}"))?;
    writer
        .write_all(text.as_bytes())
        .map_err(|e| format!("写入清单失败: {e}"))?;
    writer
        .finish()
        .map_err(|e| format!("收尾存档包 {} 失败: {e}", zip_path.display()))?;

    Ok(PackReport {
        entries: manifest.entries,
        locations: manifest.locations,
        missing: gathered.missing,
        excluded: gathered.excluded,
    })
}
