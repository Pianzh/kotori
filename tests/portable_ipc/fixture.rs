//! 隔离的真 daemon、具备超时的跨平台 RPC，以及失败现场保存。

use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub struct ChildGuard(pub Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub struct Fixture {
    pub dir: PathBuf,
    endpoint: PathBuf,
    child: Option<ChildGuard>,
}

impl Fixture {
    pub fn endpoint(&self) -> &std::path::Path {
        &self.endpoint
    }

    pub fn select_engine(&self, engine: &str) {
        let path = self.dir.join("config.toml");
        let mut config: toml::Value =
            toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        config["sync"]["engine"] = toml::Value::String(engine.into());
        std::fs::write(path, toml::to_string(&config).unwrap()).unwrap();
    }

    pub fn new(tag: &str) -> Self {
        // 先完成 helper 编译，再开始任何进程握手的超时计时。
        let helper = crate::native::executable();
        let dir = crate::native::scratch(tag);
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::copy(
            helper,
            bin.join(format!("rclone{}", std::env::consts::EXE_SUFFIX)),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let wine = bin.join("wine");
            std::fs::write(&wine, "#!/bin/sh\nexec \"$@\"\n").unwrap();
            std::fs::set_permissions(wine, std::fs::Permissions::from_mode(0o755)).unwrap();
            // 假 gamescope：只把自己怎么被调用的记下来，然后立刻退出。这一条是
            // **必须**的 —— 缺了它，daemon 会去 PATH 上找机器里那套真 gamescope，
            // 而它在无显示环境下会 SIGABRT（2026-09-25 用户收到过 DrKonqi 的崩溃
            // 通知），CI runner 上又根本没有它，本机与 CI 于是跑的不是同一条路。
            let probe = dir.join("gamescope.log");
            let gamescope = bin.join("gamescope");
            std::fs::write(
                &gamescope,
                format!("#!/bin/sh\necho \"$*\" >> '{}'\nexit 0\n", probe.display()),
            )
            .unwrap();
            std::fs::set_permissions(gamescope, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let config = json!({
            "daemon": {"auto_watch_migrated":true},
            "sync": {"engine":"rclone", "enabled":true, "bucket":"fixture", "prefix":"saves", "keep_versions":0},
        });
        // TOML 序列化负责 Windows 反斜杠转义。
        std::fs::write(dir.join("config.toml"), toml::to_string(&config).unwrap()).unwrap();
        #[cfg(unix)]
        let endpoint = dir.join("ipc.sock");
        #[cfg(windows)]
        let endpoint = PathBuf::from(format!(
            r"\\.\pipe\kotori-test-{}",
            uuid::Uuid::new_v4().simple()
        ));
        Self {
            dir,
            endpoint,
            child: None,
        }
    }

    pub fn game_exe(&self) -> PathBuf {
        // 每个场景用不同 exe 名，进程名匹配不受并行用例影响。
        let exe = self
            .dir
            .join(format!("game-{}.exe", uuid::Uuid::new_v4().simple()));
        std::fs::copy(crate::native::executable(), &exe).unwrap();
        exe
    }

    pub fn spawn_daemon(&self, log: &str) -> ChildGuard {
        let log = std::fs::File::create(self.dir.join(log)).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_kotori"));
        let mut paths = vec![self.dir.join("bin")];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        command
            .arg("daemon")
            .env("PATH", std::env::join_paths(paths).unwrap())
            .env("KOTORI_CONFIG", self.dir.join("config.toml"))
            .env("KOTORI_SOCKET", &self.endpoint)
            .env("KOTORI_DATA_DIR", self.dir.join("data"))
            .env(
                "KOTORI_SECRETS_FILE",
                self.dir.join("secrets/credentials.enc"),
            )
            .env("KOTORI_SECRET_TOOL", self.dir.join("no-secret-tool"))
            .env(
                "KOTORI_RCLONE",
                self.dir
                    .join("bin")
                    .join(format!("rclone{}", std::env::consts::EXE_SUFFIX)),
            )
            .env("KOTORI_FAKE_BUCKET", self.dir.join("bucket"))
            .env("KOTORI_FAKE_LOG", self.dir.join("rclone.log"))
            .env("KOTORI_FAKE_FAIL", self.dir.join("fail"))
            .env("KOTORI_KOPIA_REPOSITORY", self.dir.join("repository"))
            .env("KOTORI_OUTPUT_RESOLUTION", "1920x1080")
            .env("KOTORI_WINESERVER", self.dir.join("no-wineserver"))
            .env_remove("WINEPREFIX")
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log);
        ChildGuard(command.spawn().expect("spawn isolated daemon"))
    }

    pub fn start(&mut self) {
        assert!(self.child.is_none());
        self.child = Some(self.spawn_daemon("daemon.log"));
        assert!(
            wait_until(Duration::from_secs(15), || {
                self.try_rpc("daemon.status", json!({})).is_ok()
            }),
            "daemon not ready\n{}",
            self.logs()
        );
    }

    fn try_rpc(&self, method: &str, params: Value) -> Result<Value, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            // ⚠ 30 秒，不是 10：这一条是**测试自己**的同步点，不是产品行为。CI 的
            // Windows runner 上，一个请求背后可能起进程（`sync.status` 会碰引擎），
            // 慢起来十几秒很正常 —— 10 秒会把它误判成"daemon 卡死"，`lifecycle::
            // direct_exit_uploads_the_actual_save_bytes` 就这么红过好几轮。
            tokio::time::timeout(Duration::from_secs(30), async {
                #[cfg(unix)]
                let stream = tokio::net::UnixStream::connect(&self.endpoint)
                    .await
                    .map_err(|e| e.to_string())?;
                #[cfg(windows)]
                let stream = {
                    use tokio::net::windows::named_pipe::ClientOptions;
                    loop {
                        match ClientOptions::new().open(&self.endpoint) {
                            Ok(stream) => break stream,
                            Err(error) if error.raw_os_error() == Some(231) => {
                                tokio::time::sleep(Duration::from_millis(10)).await
                            }
                            Err(error) => return Err(error.to_string()),
                        }
                    }
                };
                let id = uuid::Uuid::new_v4().to_string();
                let request = json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params});
                let (reader, mut writer) = tokio::io::split(stream);
                writer
                    .write_all(format!("{request}\n").as_bytes())
                    .await
                    .map_err(|e| e.to_string())?;
                writer.flush().await.map_err(|e| e.to_string())?;
                let mut line = String::new();
                BufReader::new(reader)
                    .read_line(&mut line)
                    .await
                    .map_err(|e| e.to_string())?;
                let response: Value =
                    serde_json::from_str(&line).map_err(|e| format!("{e}: {line}"))?;
                if response["id"] != id || response["jsonrpc"] != "2.0" {
                    return Err(format!("response envelope mismatch: {response}"));
                }
                Ok(response)
            })
            .await
            .map_err(|_| format!("RPC timeout: {method}"))?
        })
    }

    pub fn rpc(&self, method: &str, params: Value) -> Value {
        let result = self.try_rpc(method, params);
        let mut trace = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("rpc.log"))
            .unwrap();
        writeln!(trace, "{method}: transport_ok={}", result.is_ok()).unwrap();
        result.unwrap_or_else(|e| panic!("{method}: {e}\n{}", self.logs()))
    }

    pub fn shutdown(&mut self) {
        let response = self.rpc("daemon.shutdown", json!({}));
        assert!(response.get("error").is_none(), "{response}");
        let child = self.child.as_mut().unwrap();
        assert!(
            wait_until(Duration::from_secs(10), || child
                .0
                .try_wait()
                .unwrap()
                .is_some()),
            "daemon shutdown timed out"
        );
        assert!(child.0.wait().unwrap().success());
        self.child.take();
    }

    pub fn logs(&self) -> String {
        std::fs::read_to_string(self.dir.join("daemon.log")).unwrap_or_default()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // 直启游戏属于 daemon 的子进程，失败时也给 helper 一次正常退出机会。
        let _ = std::fs::write(self.dir.join("release"), b"cleanup");
        self.child.take();
        if std::thread::panicking() {
            eprintln!("--- daemon ---\n{}", self.logs());
        }
        crate::native::cleanup(&self.dir);
    }
}

pub fn wait_until(timeout: Duration, mut check: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if check() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
