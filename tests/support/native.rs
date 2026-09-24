// 原生测试工具的编译与临时目录管理；单测和 IPC E2E 共用。
// rustc 来自运行 cargo test 的同一工具链，helper 仅用 std，不引入产品依赖。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

pub fn scratch(tag: &str) -> PathBuf {
    // Unix socket 路径上限很短，不能直接放进 runner 的深层 workspace。
    let tag: String = tag.chars().take(20).collect();
    let dir = std::env::temp_dir().join(format!("kt-{tag}-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub fn cleanup(dir: &Path) {
    if std::thread::panicking() {
        eprintln!("测试现场保留在 {}", dir.display());
        if let Some(root) = std::env::var_os("KOTORI_TEST_ARTIFACTS") {
            let target = PathBuf::from(root).join(dir.file_name().unwrap());
            if let Err(error) = copy_files(dir, &target) {
                eprintln!("复制测试现场失败: {error}");
            }
        }
    } else if let Err(error) = std::fs::remove_dir_all(dir) {
        eprintln!("无法清理测试目录 {}: {error}", dir.display());
    }
}

fn copy_files(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = destination.join(entry.file_name());
        if kind.is_dir() {
            copy_files(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), target)?;
        }
        // socket、symlink 不是可归档的诊断文件。
    }
    Ok(())
}

pub fn executable() -> &'static Path {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY.get_or_init(|| {
        // 每个测试进程只编译一次；用独立目录避免并行 cargo 测试争抢输出文件。
        let dir = scratch("native-helper");
        let binary = dir.join(format!("test-helper{}", std::env::consts::EXE_SUFFIX));
        let output = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
            .arg("--edition=2024")
            .arg("--crate-name=kotori_test_helper")
            .arg("-Dwarnings")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/support/tool.rs"
            ))
            .arg("-o")
            .arg(&binary)
            .output()
            .expect("运行 rustc 编译测试 helper");
        assert!(
            output.status.success(),
            "helper 编译失败: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        binary
    })
}
