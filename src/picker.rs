//! 「浏览…」:借**系统自己的**那个对话框挑一个位置,再把路径拿回来。
//!
//! 为什么不自己画一个文件浏览器:那等于把"选路径"这件事重做一遍 —— 而每个桌面上
//! 都已经有一个更好的了:Linux 上是桌面自己的文件对话框(KDE 上就是 KDE 那个,
//! GNOME/GTK 上是 GTK 那个),Windows 上是资源管理器的「打开 / 选择文件夹」框。
//! 于是这里只做两件事:**请它出来**、**读懂它回来的东西**。
//!
//! 两条路都只有"系统里确实有"才通:
//!
//! * Linux 走 `org.freedesktop.portal.FileChooser`(xdg-desktop-portal)。没有
//!   portal、或者没人提供 FileChooser 后端时,[`probe`] 就会给出理由 —— 界面据此把
//!   按钮灰掉并写明原因,而不是让用户点了没反应(用户 2026-09-13:"没有就不能用")。
//! * Windows 走 shell 的 `IFileOpenDialog`(经 `rfd`),它随系统一起装好,永远在。
//!
//! ⚠ 这一层只回答"用户挑了哪个路径",**不判断这个路径合不合适** —— 什么样的选择
//! 能写进档案(相对游戏根目录、前缀里的 %APPDATA% 令牌……)是 `crate::wine` 的事。

use std::path::PathBuf;

/// 探测的上限,见 [`probe`]。
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// 用户想挑什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Want {
    /// 一个目录(游戏根目录、wine prefix、存档目录)。
    Folder,
    /// 一个 Windows 可执行文件。
    Exe,
}

/// 一次选择请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub want: Want,
    /// 对话框的标题(说清在挑什么,而不是干巴巴一个"打开")。
    pub title: String,
    /// 打开时停在哪儿。不存在就交给对话框自己决定。
    pub start: Option<PathBuf>,
}

impl Request {
    pub fn folder(title: impl Into<String>, start: impl Into<Option<PathBuf>>) -> Self {
        Self {
            want: Want::Folder,
            title: title.into(),
            start: start.into(),
        }
    }

    pub fn exe(title: impl Into<String>, start: impl Into<Option<PathBuf>>) -> Self {
        Self {
            want: Want::Exe,
            title: title.into(),
            start: start.into(),
        }
    }
}

/// 这台机器现在能不能弹对话框。`Err` 里是**给用户看**的理由。
///
/// 探测是开机做的,所以给它一个上限:portal 偶尔会卡住(桌面刚起来、后端没应答),
/// 而卡住的表现是"按钮永远灰着",那比"没有对话框"更难查。
pub async fn probe() -> Result<(), String> {
    match tokio::time::timeout(PROBE_TIMEOUT, platform::probe()).await {
        Ok(result) => result,
        Err(_) => Err("探测超时(桌面没有应答)".to_string()),
    }
}

/// 请对话框出来,把用户挑的路径拿回来。
///
/// `Ok(None)` = 用户取消了 —— 调用方什么都不该改(不是"失败")。
pub async fn pick(request: Request) -> Result<Option<PathBuf>, String> {
    platform::pick(request).await
}

#[cfg(unix)]
mod platform {
    use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};

    use super::*;

    pub(super) async fn probe() -> Result<(), String> {
        // 真的去把这个接口点一下,而不是看总线上有没有那个名字:portal 是**按需激活**
        // 的,第一次调用之前 `org.freedesktop.portal.Desktop` 可能根本没有 owner
        // —— "名字不在"说明不了任何事(和密钥环那条坑同一个形状)。
        match ashpd::desktop::file_chooser::FileChooserProxy::new().await {
            Ok(_) => Ok(()),
            Err(e) => Err(format!(
                "这个桌面没有可用的文件选择框(xdg-desktop-portal 的 FileChooser):{e}"
            )),
        }
    }

    pub(super) async fn pick(request: Request) -> Result<Option<PathBuf>, String> {
        let mut dialog = SelectedFiles::open_file()
            .title(request.title.as_str())
            .directory(request.want == Want::Folder)
            .multiple(false)
            .modal(true);

        // 起点只接受**存在**的目录:给一个不存在的会被 portal 直接拒掉。
        if let Some(start) = request.start.as_deref().filter(|path| path.is_dir()) {
            dialog = dialog
                .current_folder(start)
                .map_err(|e| format!("选择框的起始目录不对({}):{e}", start.display()))?;
        }
        if request.want == Want::Exe {
            dialog = dialog
                .filter(FileFilter::new("Windows 程序").glob("*.exe"))
                .filter(FileFilter::new("所有文件").glob("*"));
        }

        let selected = dialog
            .send()
            .await
            .map_err(|e| format!("打不开文件选择框:{e}"))?
            .response()
            .map_err(|e| format!("文件选择框出错:{e}"))?;

        let Some(uri) = selected.uris().first() else {
            return Ok(None); // 取消
        };
        uri_to_path(uri.as_str())
            .map(Some)
            .ok_or_else(|| format!("选择框回来的不是一个本地路径:{}", uri.as_str()))
    }
}

#[cfg(windows)]
mod platform {
    use super::*;

    pub(super) async fn probe() -> Result<(), String> {
        // shell 的对话框是系统自带的,没有"装没装"这回事。
        Ok(())
    }

    pub(super) async fn pick(request: Request) -> Result<Option<PathBuf>, String> {
        // ⚠ rfd 的同步 API 会**阻塞当前线程**(Windows 上它自己跑消息循环),所以丢进
        //   blocking 线程池 —— 别占着 tokio 的 worker。COM 的初始化由 rfd 自己负责。
        tokio::task::spawn_blocking(move || {
            let mut dialog = rfd::FileDialog::new().set_title(request.title.clone());
            if let Some(start) = request.start.clone().filter(|path| path.is_dir()) {
                dialog = dialog.set_directory(start);
            }
            match request.want {
                Want::Folder => dialog.pick_folder(),
                Want::Exe => dialog.add_filter("Windows 程序", &["exe"]).pick_file(),
            }
        })
        .await
        .map_err(|e| format!("打开文件选择框的任务失败:{e}"))
    }
}

/// `file:///a%20b` → `/a b`。
///
/// portal 回来的是 URI,不是路径:非 ASCII(中文游戏目录!)在 URI 里是 UTF-8 的
/// `%XX`,不还原就会拿到一串乱码目录名。
fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // `file:///path`(主机名是空的)与 `file://localhost/path` 都要吃下:
    // 路径从第一个 `/` 开始。
    let encoded = &rest[rest.find('/')?..];
    Some(PathBuf::from(percent_decode(encoded)?))
}

/// `%XX` 还原成字节,再按 UTF-8 解(URI 里的非 ASCII 就是 UTF-8 的百分号编码)。
fn percent_decode(encoded: &str) -> Option<String> {
    let bytes = encoded.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            out.push(bytes[index]);
            index += 1;
            continue;
        }
        let hex = encoded.get(index + 1..index + 3)?;
        out.push(u8::from_str_radix(hex, 16).ok()?);
        index += 3;
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_file_uri_becomes_the_path_behind_it() {
        assert_eq!(
            uri_to_path("file:///games/demo/game.exe"),
            Some(PathBuf::from("/games/demo/game.exe"))
        );
    }

    #[test]
    fn a_localhost_host_is_not_part_of_the_path() {
        assert_eq!(
            uri_to_path("file://localhost/games/demo"),
            Some(PathBuf::from("/games/demo"))
        );
    }

    /// 中文目录名在 URI 里是 UTF-8 的百分号编码 —— 不还原就选完变成乱码。
    #[test]
    fn non_ascii_names_come_back_readable() {
        let uri = "file:///games/%E5%B5%8C%E5%85%A5%E7%9A%84%E5%AD%A6%E5%9B%AD/save";
        assert_eq!(
            uri_to_path(uri),
            Some(PathBuf::from("/games/嵌入的学园/save"))
        );
    }

    #[test]
    fn anything_that_is_not_a_local_file_is_refused() {
        assert_eq!(uri_to_path("https://example.com/x"), None);
        assert_eq!(uri_to_path("file:///bad%2"), None);
        // 不是合法 UTF-8 的百分号解码结果:宁可说"读不懂",也不要编一个路径出来。
        assert_eq!(uri_to_path("file:///%FF%FE"), None);
    }

    #[test]
    fn a_request_says_what_it_wants() {
        let folder = Request::folder("挑游戏根目录", Some(PathBuf::from("/games")));
        assert_eq!(folder.want, Want::Folder);
        assert_eq!(folder.start, Some(PathBuf::from("/games")));
        assert!(Request::exe("挑 exe", None).start.is_none());
    }
}
