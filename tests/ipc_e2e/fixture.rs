//! e2e 的夹具：一个跑在临时目录里的真 daemon。
//!
//! `KOTORI_CONFIG` / `KOTORI_SOCKET` 把它钉在临时目录里，测试通过 Unix socket
//! 驱动它，与 GUI 和 CLI 走的是同一条路。`Drop` 时自己收拾干净。

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::helpers::write_script;

pub(crate) struct Fixture {
    pub(crate) dir: PathBuf,
    pub(crate) config: PathBuf,
    pub(crate) socket: PathBuf,
    pub(crate) log: PathBuf,
    pub(crate) child: Option<Child>,
    /// Extra directory prepended to the daemon's PATH (fake gamescope/wine).
    pub(crate) extra_path: Option<PathBuf>,
    /// Extra environment for the daemon (fake rclone / secret-tool).
    pub(crate) envs: Vec<(String, String)>,
}

impl Fixture {
    pub(crate) fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "kotori-e2e-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let config = dir.join("config.toml");
        let socket = dir.join("kotori.sock");
        let log = dir.join("daemon.log");

        // The config points at a *different* socket than KOTORI_SOCKET, which
        // proves the env override wins.
        let config_body = format!(
            r#"
[daemon]
socket_path = "{}"
log_level = "info"

[games.demo]
name = "Demo"
exe_path = "/games/demo/game.exe"
save_paths = ["/games/demo/save"]
created_at = "2026-01-01T00:00:00Z"

[games.demo.scale_profile]
name = "自定义"
internal_width = 1920
internal_height = 1080
output_width = 2560
output_height = 1440
framerate_limit = 60
force_fullscreen = false

[games.demo.scale_profile.algorithm.Nis]
sharpness = 4
"#,
            dir.join("wrong.sock").display()
        );
        std::fs::write(&config, config_body).unwrap();

        Self {
            dir,
            config,
            socket,
            log,
            child: None,
            extra_path: None,
            envs: Vec::new(),
        }
    }

    /// Stand up fake `rclone` and `secret-tool` binaries plus an on-disk
    /// "bucket", so sync can be exercised end to end without a network or a
    /// real keyring.
    ///
    /// The fake rclone really moves bytes: `kotori:<path>` mirrors to
    /// `<dir>/<path>`, so the returned directory *is* the remote root.
    /// `copyto` copies an object each way, `lsf --files-only` lists the packages
    /// and `deletefile` removes one — so the assertions about versions and
    /// restores are statements about actual files.
    pub(crate) fn enable_fake_sync(&mut self, enabled: bool) -> PathBuf {
        let bin = self.dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let log = self.dir.join("rclone.log");
        let secrets = self.dir.join("secrets");
        std::fs::create_dir_all(&secrets).unwrap();

        write_script(
            &bin.join("rclone"),
            &format!(
                r#"#!/bin/sh
[ "$1" = "--kotori-warmup" ] && exit 0
dir='{dir}'
log='{log}'
bucket='{bucket}'
printf 'argv:%s\n' "$*" >> "$log"

# Map a remote path onto the on-disk bucket; local paths pass through.
remote_path() {{
  case "$1" in
    kotori:*) printf '%s/%s' "$bucket" "${{1#kotori:}}" ;;
    *) printf '%s' "$1" ;;
  esac
}}

cmd="$1"; shift
case "$cmd" in
  mkdir) mkdir -p "$(remote_path "$1")" ;;
  # One version is one package: a transfer is a single object each way.
  copyto)
    sp=$(remote_path "$1"); dp=$(remote_path "$2")
    mkdir -p "$(dirname "$dp")"
    cp "$sp" "$dp"
    ;;
  # `lsf --files-only <remote>`: the file names *are* the version list.
  lsf)
    target=''
    for a in "$@"; do [ "$a" = '--files-only' ] || target="$a"; done
    p=$(remote_path "$target")
    if [ -d "$p" ]; then ls -1 "$p" | grep '\.zip$'; fi
    ;;
  deletefile) rm -f "$(remote_path "$1")" ;;
esac
exit 0
"#,
                dir = self.dir.display(),
                log = log.display(),
                bucket = self.dir.display()
            ),
        );

        // Mirrors the parts of secret-tool kotori relies on; entries are files
        // named after the `account` attribute.
        write_script(
            &bin.join("secret-tool"),
            &format!(
                r#"#!/bin/sh
[ "$1" = "--kotori-warmup" ] && exit 0
dir='{secrets}'
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
exit 0
"#,
                secrets = secrets.display()
            ),
        );

        self.extra_path = Some(bin.clone());
        self.envs.push((
            "KOTORI_RCLONE".to_string(),
            bin.join("rclone").display().to_string(),
        ));
        self.envs.push((
            "KOTORI_SECRET_TOOL".to_string(),
            bin.join("secret-tool").display().to_string(),
        ));

        let mut config = std::fs::read_to_string(&self.config).unwrap();
        // ⚠ `engine` 必须**显式**写成 rclone:这个夹具造的是**假 rclone**,而缺这个键时
        // serde 会用当下的默认值(2026-09-18 起是 kopia)—— 不写的话这些端到端测试会
        // 全部转去找 kopia,假 rclone 一次都不会被调用。
        config.push_str(&format!(
            "\n[sync]\nenabled = {enabled}\nengine = \"rclone\"\nbucket = \"test-bucket\"\nprefix = \"kotori\"\n"
        ));
        std::fs::write(&self.config, config).unwrap();

        // Where the remote root mirrors to: `<dir>/<bucket>/<prefix>`.
        self.dir.join("test-bucket").join("kotori")
    }

    /// Add fake `gamescope`/`wine` that record how they were invoked and exit.
    pub(crate) fn enable_fake_display(&mut self) -> PathBuf {
        let bin = self.dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let probe = self.dir.join("probe.txt");
        let script = format!(
            "#!/bin/sh\n[ \"$1\" = \"--kotori-warmup\" ] && exit 0\n{{ echo \"argv:$*\"; echo \"cwd:$(pwd)\"; echo \"WINEPREFIX:${{WINEPREFIX:-}}\"; }} >> '{}'\nexit 0\n",
            probe.display()
        );
        for name in ["gamescope", "wine"] {
            write_script(&bin.join(name), &script);
        }
        self.extra_path = Some(bin);
        probe
    }

    pub(crate) fn start(&mut self) {
        let stdout = std::fs::File::create(&self.log).unwrap();
        let stderr = stdout.try_clone().unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_kotori"));
        command
            .arg("daemon")
            .env("KOTORI_CONFIG", &self.config)
            .env("KOTORI_SOCKET", &self.socket)
            // 数据目录也搬到临时目录里：同步要在这儿放打包用的临时包，
            // 而一次测试绝不该往用户真正的 `~/.local/share/kotori` 里写东西。
            .env("KOTORI_DATA_DIR", self.dir.join("data"))
            // Keep display detection out of the test: the daemon must use this
            // value for new games regardless of the machine it runs on.
            .env("KOTORI_OUTPUT_RESOLUTION", "2560x1440")
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        if let Some(bin) = &self.extra_path {
            let existing = std::env::var("PATH").unwrap_or_default();
            command.env("PATH", format!("{}:{existing}", bin.display()));
        }
        for (key, value) in &self.envs {
            command.env(key, value);
        }
        let child = command.spawn().expect("failed to spawn kotori daemon");
        self.child = Some(child);

        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if UnixStream::connect(&self.socket).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("daemon socket never became reachable\n{}", self.logs());
    }

    pub(crate) fn logs(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    pub(crate) fn rpc(&self, method: &str, params: Value) -> Value {
        let mut stream = UnixStream::connect(&self.socket)
            .unwrap_or_else(|e| panic!("connect failed: {e}\n{}", self.logs()));
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();

        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        stream.write_all(request.to_string().as_bytes()).unwrap();
        stream.write_all(b"\n").unwrap();
        stream.flush().unwrap();

        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("bad response {line:?}: {e}"))
    }

    pub(crate) fn wait_for_exit(&mut self, timeout: Duration) -> bool {
        let child = self.child.as_mut().expect("not started");
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            match child.try_wait().unwrap() {
                Some(_) => return true,
                None => std::thread::sleep(Duration::from_millis(50)),
            }
        }
        false
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        std::fs::remove_dir_all(&self.dir).ok();
    }
}
