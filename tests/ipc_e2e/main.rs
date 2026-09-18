//! End-to-end test of the daemon IPC contract.
//!
//! ⚠ 整套 e2e 都建立在 Unix socket 上(`fixture` 直接连 `KOTORI_SOCKET`)。
//! Windows 上传输是 named pipe,这一套要另写一份,所以这里先整体关掉 ——
//! 留在这里编译不过比"假装跑过了"诚实。

#![cfg(unix)]
//!
//! Spawns the real `kotori daemon` binary against a throw-away config/socket in
//! the temp dir (via `KOTORI_CONFIG` / `KOTORI_SOCKET`) and drives it over the
//! Unix socket, exactly like the GUI and CLI do.
//!
//! 文件分工：本文件只声明子模块；`fixture` 是那个临时 daemon 本身，`helpers` 是
//! 不依赖夹具的小工具，其余按**被测的那件事**分：守护进程自己的生命周期、
//! 游戏库的增删改、启动与跟随会话、云同步。

mod fixture;
mod helpers;

mod daemon;
mod library;
mod session;
mod sync;
