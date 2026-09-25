//! 原生测试进程：严格的 rclone 子集与可握手退出的假游戏。
//! 由测试用 rustc 单独编译；所有文件操作仅针对夹具显式提供的位置。

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn main() {
    if let Err(error) = run() {
        eprintln!("fake tool: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args == ["--kotori-warmup"] {
        return Ok(());
    }
    if args.first().is_some_and(|arg| arg == "--game") {
        return game(&args[1..]);
    }
    let mut root = std::env::var_os("KOTORI_FAKE_BUCKET").map(PathBuf::from);
    let mut log = std::env::var_os("KOTORI_FAKE_LOG").map(PathBuf::from);
    let mut fail = std::env::var_os("KOTORI_FAKE_FAIL").map(PathBuf::from);
    while args.first().is_some_and(|arg| arg.starts_with("--rclone-")) {
        if args.len() < 2 {
            return Err("missing helper option value".into());
        }
        let value = PathBuf::from(args.remove(1));
        match args.remove(0).as_str() {
            "--rclone-root" => root = Some(value),
            "--rclone-log" => log = Some(value),
            "--rclone-fail" => fail = Some(value),
            _ => return Err("unknown helper option".into()),
        }
    }
    if args == ["--kotori-warmup"] {
        return Ok(());
    }
    let root = root.ok_or("fake bucket must be explicitly configured")?;
    // 真实 rclone 的本地存储验收：只替换远端位置，命令/选项交给真实程序解析。
    if let Some(binary) = std::env::var_os("KOTORI_REAL_RCLONE") {
        let mapped: Vec<_> = args
            .iter()
            .map(|arg| {
                arg.strip_prefix("kotori:").map_or_else(
                    || std::ffi::OsString::from(arg),
                    |remote| root.join(remote).into_os_string(),
                )
            })
            .collect();
        let status = std::process::Command::new(binary).args(mapped).status()?;
        if !status.success() {
            return Err(format!("real rclone exited: {status}").into());
        }
        return Ok(());
    }
    if let Some(log) = log {
        let mut record = format!("argv:{}\n", args.join(" "));
        for (key, value) in std::env::vars().filter(|(key, _)| key.starts_with("RCLONE_CONFIG")) {
            record.push_str(&format!("env:{key}={value}\n"));
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)?
            .write_all(record.as_bytes())?;
    }
    if let Some(needle) = fail.and_then(|path| fs::read_to_string(path).ok()) {
        if args.join(" ").contains(needle.trim()) {
            return Err(format!("injected failure: {}", args.join(" ")).into());
        }
    }
    let resolve = |text: &str| -> Result<PathBuf, Box<dyn std::error::Error>> {
        if let Some(remote) = text.strip_prefix("kotori:") {
            if remote.split(['/', '\\']).any(|part| part == "..") || Path::new(remote).is_absolute()
            {
                return Err("remote path escapes fake bucket".into());
            }
            Ok(root.join(remote))
        } else {
            Ok(PathBuf::from(text))
        }
    };
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["mkdir", path] => fs::create_dir_all(resolve(path)?)?,
        ["copyto", source, destination] => {
            let destination = resolve(destination)?;
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)?;
            }
            // 模拟对象上传：复制失败不能发布半个对象。
            let tmp = destination.with_extension(format!("part-{}", std::process::id()));
            fs::copy(resolve(source)?, &tmp)?;
            // Windows 的 rename 不覆盖已有文件；测试桶替换时仅移除目标文件。
            if destination.exists() {
                fs::remove_file(&destination)?;
            }
            fs::rename(tmp, destination)?;
        }
        ["cat", path] => {
            io::stdout().write_all(&fs::read(resolve(path)?)?)?;
        }
        ["deletefile", path] => {
            fs::remove_file(resolve(path)?)?;
        }
        // 真 rclone 的 `rmdir` 只删**空**目录，非空时报错 —— 那正说明不该删
        // （云存档那里删掉词条之后收空壳目录就靠它）。
        ["rmdir", path] => {
            fs::remove_dir(resolve(path)?)?;
        }
        [
            command @ ("lsf" | "lsjson"),
            mode @ ("--files-only" | "--dirs-only"),
            path,
        ] => {
            if *command == "lsjson" && *mode != "--files-only" {
                return Err("unsupported lsjson mode".into());
            }
            let path = resolve(path)?;
            let mut entries = match fs::read_dir(path) {
                Ok(entries) => entries.collect::<Result<Vec<_>, _>>()?,
                // 对象存储中尚不存在的前缀视为空集合。
                Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
                Err(error) => return Err(error.into()),
            };
            entries.sort_by_key(|entry| entry.file_name());
            let mut rows = Vec::new();
            for entry in entries {
                let meta = entry.metadata()?;
                if (*mode == "--dirs-only") != meta.is_dir() {
                    continue;
                }
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| "non UTF-8 fixture name")?;
                if *command == "lsjson" {
                    rows.push(format!(
                        "{{\"Path\":{},\"Name\":{},\"Size\":{},\"IsDir\":false}}",
                        json_string(&name),
                        json_string(&name),
                        meta.len()
                    ));
                } else {
                    println!("{name}{}", if meta.is_dir() { "/" } else { "" });
                }
            }
            if *command == "lsjson" {
                println!("[{}]", rows.join(","));
            }
        }
        _ => return Err(format!("unsupported command or arguments: {args:?}").into()),
    }
    Ok(())
}

fn json_string(text: &str) -> String {
    let mut escaped = String::from("\"");
    for ch in text.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            ch if ch <= '\u{1f}' => escaped.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => escaped.push(ch),
        }
    }
    escaped.push('"');
    escaped
}

fn game(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let [ready, release] = args else {
        return Err("game expects ready and release paths".into());
    };
    fs::write(ready, std::process::id().to_string())?;
    let deadline = Instant::now() + Duration::from_secs(120);
    while !Path::new(release).exists() {
        if Instant::now() >= deadline {
            return Err("game release handshake timed out".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}
