# Kotori - 项目总目标

> 本文档为项目最终目标的完整描述，不可随意修改。所有开发工作以此为准。

---

## 1. 产品定位

构建一款**跨平台**的 Galgame 集中化管理器：

- **Linux（主平台，x86_64 / ARM64）**：完整功能——游戏启动、原生缩放增强（gamescope）、云存档同步
- **Windows（x86_64）**：以**云存档同步为主**的管理器，复用同一套配置模型与同步引擎；游戏启动沿用系统原生方式，缩放增强为 V2 可选项（Magpie）

**平台能力矩阵**：

| 能力 | Linux (Niri/Wayland) | Linux (其他/浮动) | Windows |
|------|----------------------|-------------------|---------|
| 游戏库管理 | ✅ | ✅ | ✅ |
| 游戏启动 (wine) | ✅ | ✅ | ➖ 原生启动 |
| 缩放增强 | ✅ gamescope | ✅ gamescope | 🔜 V2 Magpie |
| 运行时热键调参 | ✅ Niri 绑定 / gamescope 快捷键 | 部分 | 🔜 |
| 云存档同步 | ✅ | ✅ | ✅ **首要功能** |

**核心价值**：
- 一站式管理所有 Galgame，统一启动入口
- **Linux 端的「启动 + 缩放」体验对标 Windows 的 Magpie**：点一下就能玩，不需要记 gamescope 命令行；不追求 Magpie 的全部能力，只服务 wine 下的老游戏
- 游戏由用户手动添加（不做目录自动扫描），启动方式支持两种：由 kotori 启动（可套用缩放），或仅观测用户自己启动的进程（只做存档同步）
- 原生缩放增强，让老游戏在高分辨率屏幕上焕发新生
- 云存档同步，跨设备无缝衔接游戏进度（Windows 端以此为核心）

---

## 2. 核心功能

### 2.1 游戏中心化管理
- 统一 UI 管理所有游戏；**游戏由用户手动添加**（不做目录扫描作为主路径，CLI 保留批量导入供 agent/特殊情况使用）
- 每款游戏独立元数据：
  - **游戏根目录**（启动时的工作目录，也是相对存档路径的基准）
  - 可执行文件路径 + 启动参数
  - **Wine 目录（prefix）**：可留空，按「全局配置 → 自动探测 → `~/.wine`」解析
  - **存档位置**（用户手动指定，见 §2.3）
  - 绑定的缩放方案
  - 启动模式：**由 kotori 启动**（可套用缩放）/ **仅观测进程**（kotori 不启动，只跟随 `process_name` 判断运行状态，用于用户自行启动的游戏与 Windows 端；此模式不提供缩放）
- UI 关闭后守护进程持续运行，不影响已启动游戏

### 2.2 分游戏缩放策略（核心能力，Linux）
- 缩放方案与游戏强绑定，切换游戏自动加载配置
- Linux 基于 gamescope 微合成器实现缩放
- 输出分辨率自动取自显示器实际分辨率（不再硬编码），可用 `KOTORI_OUTPUT_RESOLUTION` 覆盖
- 支持 FSR / NIS / Integer / Bilinear（gamescope 无 Lanczos，故不提供）
- 通过热键实时调整缩放参数
- 启动时自动注入解析出的 `WINEPREFIX`，并以游戏根目录为 CWD（老游戏常按相对路径找资源）
- Windows 调用原生 Magpie API（V2）

### 2.3 云存档同步
- **存档位置由用户手动指定**，三种形态（存储时即区分，跨平台可解析）：
  - **Windows 令牌路径**：`%APPDATA%\Game\save`、`%USERPROFILE%\Documents\...`、`%SAVEDGAMES%\...`
    —— AppData / 文档 / 存档目录里的存档用这种；**不要存 `C:\users\<用户名>\...` 字面量**，
    因为 prefix 里的用户名不固定（普通 wine 用 Linux 账户，Proton 通常是 `steamuser`）
  - **相对游戏根目录**：其他位置只要能用相对路径就用相对路径
  - **绝对路径**：仅本机有效，不参与跨平台映射
  - 每条可带排除规则（如 `*.log`、`cache/`）
- 基于 Backblaze B2 实现增量同步（kopia，走 S3 兼容后端，见 AGENTS.md ADR-007）
- 支持多游戏独立索引与滑动窗口版本保留
- 启动前自动拉取（30s 超时），退出后异步上传
- 失败不阻断流程，仅提示
- **Windows 端的主要交付内容**，同步模块平台无关（不依赖 gamescope/wine）

### 2.4 跨平台适配
- Linux ARM64：主交付平台，内嵌 kopia + ludusavi 静态二进制
- Linux x86_64：复用 ARM64 全部逻辑
- Windows x86_64：复用配置模型 + 同步模块；缩放替换为 Magpie（V2）
  - 待办：IPC（Unix Socket → Named Pipe）与进程管理（进程组 → Job Object）需抽象层，见 §8

---

## 3. 架构约束

### 3.1 双进程分离模型
- **前端 UI**：仅负责展示与用户交互，通过 IPC 向守护进程发送指令
- **后台守护进程**：独占所有运行时能力，支持配置热加载

### 3.2 缩放抽象层
- 统一 `ScaleEngine` trait：应用预设、热更新参数、注册/注销热键
- Niri 后端：通过 `niri msg action spawn` 触发守护进程命令
- KDE 后端：通过 D-Bus 接口（V2）
- 两者共享 gamescope 命令行构建逻辑

### 3.3 数据与通信契约
- 配置存储：单一 JSON/TOML 文件，以游戏 ID 为键
- IPC 协议：JSON-RPC over Unix Socket
- B2 凭证加密存储于系统密钥环

### 3.4 外部工具集成
- 运行时硬依赖：gamescope (Linux) / Magpie (Windows) + wine + kopia
- 单文件分发：内嵌对应架构静态二进制，总体积 ~35MB
- 统一执行器封装：带超时、重试、结构化错误输出

---

## 4. 技术选型

| 组件 | 选择 | 理由 |
|------|------|------|
| 语言 | Rust | 内存安全、单文件编译、Wayland 生态成熟 |
| GUI | Iced | Elm 架构、Wayland+Windows 支持、MIT 许可 |
| 异步 | tokio | Rust 异步标准 |
| IPC | Unix Socket + JSON-RPC | 简单可靠 |
| 缩放 | gamescope | Linux 原生微合成器 |
| 游戏运行 | wine | Windows 游戏兼容层 |
| 云同步 | kopia + B2 | 增量备份、加密、云存储 |
| 存档发现 | ludusavi (可选) | 预填存档路径 |

---

## 5. 分阶段目标

### Phase 1: 缩放核心（4-6 周）
**目标**：实现 gamescope 缩放的完整流程

- 项目骨架与配置系统
- Unix Socket IPC 服务器
- CLI 基础命令（list, launch, scale）
- 游戏扫描器（扫描指定目录）
- Gamescope 参数构建器
- ScaleEngine trait + Niri 后端
- 游戏启动流程（Wine + gamescope）
- 热键处理（通过 Niri 配置）
- Iced GUI 基础框架
- 游戏列表与缩放配置界面

### Phase 2: 云同步（3-4 周）
**目标**：实现基于 B2 的增量云存档同步

- 内嵌 kopia 二进制
- Kopia 命令封装
- B2 客户端
- 启动前同步（30s 超时）
- 退出后异步同步
- 同步状态查询与手动触发

### Phase 3: UI 打磨与跨平台（2-3 周）
**目标**：完善 GUI 体验，准备 Windows 支持

- TUI 快捷操作
- 完整 GUI 界面
- 游戏库浏览与搜索
- 实时缩放状态显示
- Windows 端：**存档同步管理器优先**（IPC 抽象 + 同步模块先行）
- Windows Magpie 缩放后端（V2）
- KDE/KWin 后端（V2）

---

## 6. 非功能需求

### 6.1 性能
- 守护进程内存占用 < 50MB
- UI 响应时间 < 100ms
- 同步速度受限于网络带宽

### 6.2 可靠性
- UI 崩溃不影响游戏和后台任务
- 同步失败不阻断游戏启动
- 配置文件损坏时使用默认值

### 6.3 可维护性
- 模块边界清晰，解耦设计
- 所有异常包含完整上下文（game_id, action, platform, timestamp）
- 云存档模块完全平台无关

### 6.4 分发
- 零依赖单文件可执行程序
- 按 (os, arch) 分发：Linux 完整版；Windows 存档同步版
- 总体积控制在 ~35MB 以内（注意：内嵌 CJK 字体已占 ~17MB，见 AGENTS.md §5）

---

## 7. 测试策略

### 7.1 单元测试
- 配置加载/保存
- 游戏 ID 生成
- Gamescope 参数构建
- IPC 消息序列化

### 7.2 集成测试
- 守护进程启动/停止
- IPC 客户端/服务器通信
- 游戏启动/停止流程

### 7.3 端到端测试
- 使用 BTL 目录游戏验证完整流程
- 缩放效果验证
- 热键响应测试
- 错误场景测试

---

## 8. 已知限制与未来规划

### 当前限制
- 仅支持 Wine 运行的 Windows 游戏
- gamescope 不支持运行时热切换参数（需重启会话）
- KDE 后端尚未实现
- **当前代码仅能编译于 Unix**：IPC 用 `tokio::net::UnixStream`、进程管理用 `libc::kill` / `process_group`。
  Windows 版落地前需要抽象层（Named Pipe + Job Object），同步模块本身保持平台无关。

### 未来规划
- **Windows 端存档同步管理器**（首要跨平台目标）
- 支持原生 Linux 游戏
- 支持 Proton/Steam 游戏
- 实现 ext-global-shortcuts-v1 协议级热键
- Windows Magpie 深度集成
- 多显示器智能适配（当前已自动探测聚焦输出分辨率）
- 游戏封面/元数据自动获取

---

## 9. 参考资料

- [gamescope 文档](https://github.com/ValveSoftware/gamescope)
- [Iced 文档](https://iced.rs/)
- [Niri 配置](https://niri-wm.github.io/niri/Configuration:-Introduction)
- [Kopia 文档](https://kopia.io/)
- [Magpie 项目](https://github.com/Blinue/Magpie)

---

> 本文档为项目最终目标参考，所有开发工作以此为准。如有变更需经评审后更新。
