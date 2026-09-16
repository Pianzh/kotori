//! 测试夹具：一个由文件支撑的假 `secret-tool`，测试永远不碰用户真正的密钥环。

use std::path::{Path, PathBuf};

use super::{Keyring, SecretKey};

/// Argument every fake helper answers with an immediate, side-effect-free
/// exit. Used by [`write_executable`].
pub(crate) const WARMUP_FLAG: &str = "--kotori-warmup";

/// Write a helper script and prove the kernel will actually run it.
///
/// Writing a file and exec'ing it are each safe on their own, but tests run
/// in parallel: another thread can fork between our write and our first
/// exec and inherit the still-open write handle, which makes the kernel
/// report `ETXTBSY` for that inode until the child execs. Retrying
/// converges, because once our own write handle is closed nothing can open
/// the file for writing again — and every fake script exits immediately on
/// [`WARMUP_FLAG`], so the warm-up has no side effects.
pub(crate) fn write_executable(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();

    for _ in 0..200 {
        match std::process::Command::new(path).arg(WARMUP_FLAG).output() {
            Ok(_) => return,
            Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(e) => panic!("cannot execute {}: {e}", path.display()),
        }
    }
    panic!("{} stayed busy", path.display());
}

pub(crate) struct FakeTool {
    dir: PathBuf,
    tool: PathBuf,
}

impl FakeTool {
    pub(crate) fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "kotori-secrets-{tag}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let tool = dir.join("secret-tool");

        // Mirrors the parts of secret-tool's interface kotori relies on:
        // entries are keyed by their `account` attribute, `store` reads the
        // secret from stdin, `lookup` prints it, `clear` removes it.
        let script = format!(
            r#"#!/bin/sh
[ "$1" = "{WARMUP_FLAG}" ] && exit 0
dir='{dir}'
name=''
prev=''
for a in "$@"; do
  if [ "$prev" = "account" ]; then name="$a"; fi
  prev="$a"
done
file="$dir/$name"
case "$1" in
  store) read -r v; printf '%s' "$v" > "$file" ;;
  lookup) [ -s "$file" ] && cat "$file" || exit 1 ;;
  clear) rm -f "$file" ;;
  *) exit 2 ;;
esac
"#,
            dir = dir.display()
        );
        write_executable(&tool, &script);

        Self { dir, tool }
    }

    pub(crate) fn keyring(&self) -> Keyring {
        Keyring::with_tool(&self.tool)
    }

    /// A `secret-tool` that exists but has no backend behind it — exactly
    /// what a session that never started a Secret Service provider looks
    /// like (the binary is installed, the D-Bus name answer is not).
    pub(crate) fn broken(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "kotori-secrets-broken-{tag}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let tool = dir.join("secret-tool");
        write_executable(
            &tool,
            "#!/bin/sh\n\
                 [ \"$1\" = \"--kotori-warmup\" ] && exit 0\n\
                 echo 'secret-tool: The name is not activatable' >&2\nexit 1\n",
        );
        Self { dir, tool }
    }

    /// Where the fake tool keeps one account's secret.
    pub(crate) fn stored(&self, key: SecretKey) -> PathBuf {
        self.dir.join(key.account())
    }
}

impl Drop for FakeTool {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}
