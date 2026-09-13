# 云存档同步

**这篇讲什么**:存档以什么形状躺在 B2 里、rclone 怎么被调用、自动取回与手动恢复为什么
语义不同、快照滚动窗口怎么算、以及**凭据的四级存储与挑选顺序**。
**什么时候读它**:改同步行为、改凭据后端、或者排查"存档没上传 / 传错了 / 恢复没生效"的时候。

核对来源:`src/sync/{mod,runner}.rs`、`src/daemon/sync_rpc.rs`、`src/secrets/{mod,plain,encrypted}.rs`、
`src/config/mod.rs`、`src/wine.rs`、`tests/ipc_e2e.rs`。

---

## 1. 云端布局

远端名字由 `sync::remote_name` 决定:加密关闭时是 `kotori`,开启时是 `kotorienc`
(后者是 rclone 的 `crypt` 层,套在前者外面)。

```
<remote>:<bucket>/<prefix>/                    ← remote_root(),例:kotori:bucket/prefix
└── games/<game_id>/
    ├── current/<save_key>/…                   ← 每个存档位置的"最新一份"
    └── versions/<stamp>/<save_key>/…          ← 被替换掉的文件,按上传时刻归档
```

- `bucket` 与 `prefix` 都来自 `[sync]`;`prefix` 默认 `kotori`。两者拼接规则见
  `remote_root`(任一个为空都还能用,只是路径短一段)。
- **`save_key` 由存档位置的"描述"推导,不是它的下标**(`sync::save_key`):
  `<win|rel|abs>-<小写、非字母数字折成下划线、去重下划线、截断 48 字符>`。
  例:`%APPDATA%\Game\save` → `win-appdata_game_save`,`savedata` → `rel-savedata`。
  **在 UI 里调整列表顺序不会打乱云端已有的东西**,三种 kind 之间也不会撞名。
- **`stamp` 由 `version_stamp` 生成**:`%Y%m%dT%H%M%SZ` + `-<8 位随机 hex>`。
  前 16 个字符是秒级 UTC,所以**字典序 == 时间序**,裁剪就是一个排序;随机后缀是必需的:
  同一秒内的两次上传(游戏刚退出 + 用户点了「立即同步」,或恢复前的安全快照)否则会共用
  一个目录,后一次会**悄悄毁掉**前一次的记录。

---

## 2. rclone:原生 `b2` + 可选 `crypt`

**为什么是 rclone 而不是 kopia**:加密于是变成一个可叠加的 `crypt` 层而非硬性要求 ——
不加密时 bucket 里的存档就是普通文件,不装 kotori、不装 rclone、用 B2 网页就能取回。

### 凭据怎么交出去

凭据**只经子进程环境**传递(`sync::rclone_env`),绝不进 argv(`ps` 全世界可读),
也不写任何配置文件:

| 环境变量 | 值 |
|----------|-----|
| `RCLONE_CONFIG` | `/dev/null`(Windows 上是 `NUL`)—— 忽略机器上任何 `rclone.conf`,包括用户自己的 |
| `RCLONE_CONFIG_KOTORI_TYPE` | `b2` |
| `RCLONE_CONFIG_KOTORI_ACCOUNT` / `_KEY` | keyID / applicationKey |
| `RCLONE_CONFIG_KOTORI_ENDPOINT` | 仅当 `[sync].endpoint` 非空 |
| `RCLONE_CONFIG_KOTORIENC_TYPE` | `crypt`(仅加密时) |
| `RCLONE_CONFIG_KOTORIENC_REMOTE` | `kotori:<bucket>/<prefix>` |
| `RCLONE_CONFIG_KOTORIENC_PASSWORD` | **obscure 形态**,绝不明文 |
| `RCLONE_CONFIG_KOTORIENC_PASSWORD2` | 常量 `kotori`(第二因子,跨机器稳定) |
| `RCLONE_CONFIG_KOTORIENC_FILENAME_ENCRYPTION` / `_DIRECTORY_NAME_ENCRYPTION` | `standard` / `true` |

**没有 region**:原生 `b2` 后端从凭据自己发现 API 主机。

**`endpoint` 的规矩**(`sync::validate_endpoint`):默认留空。填了必须是完整 URL(含
`https://`);`s3.<region>.backblazeb2.com` 被**明确拒绝** —— 那是 B2 的 S3 兼容接口,
和原生 b2 后端不是同一个 API,发过去只会得到费解的 404。

**obscure 交给 rclone 自己做**(`Runner::obscure` → `rclone obscure <password>`),因为
自己实现差一个字节就会推出另一个密钥、把用户锁在自己的备份外面。代价:这一次性调用会把
密码放进 argv;同步运行本身只从存储里读 obscure 形态,命令行上什么都没有。

### 结构校验 vs 凭据校验

- `sync::validate`:必须 `enabled`、`bucket` 非空、`endpoint` 合法。
- `sync::validate_secrets`:**主密码文件锁着** → 报"已锁定"而**不是**"还没有凭据";
  B2 两个键缺任一 → 报错;开了加密还额外要求 `SyncPassword` 与 `SyncPasswordObscured`
  两个形态都在。
- `sync.status` 只在 `enabled` 时把这两条的结果放进 `problem`;`ready` 还额外要求
  `rclone` 找得到。

---

## 3. 三个动作,三种语义(不要合并)

rclone 的实际调用由 `sync/runner.rs` 组装,`sync/mod.rs` 只提供参数构造函数(都有单测)。

### 上传 `upload()` —— 「覆盖」

```
rclone copy <local> <remote>/current/<key>
     --create-empty-src-dirs --backup-dir <remote>/versions/<stamp>/<key> --suffix ""
     [--exclude …]×N
```

- 用 **`copy` 而不是 `sync`**:`copy` 永远不删目标端的东西。
- `--backup-dir` + **空 `--suffix`**:被替换的文件原样搬到这一笔的快照目录里 —— 这就是
  "没有仓库格式也有历史"的全部实现。
- 一次 `upload` 里**所有位置共用同一个 stamp**,但快照目录带各自的 `key`,所以两个位置
  不会互相覆盖。
- 本地目录不存在 → 该位置 `skipped`(**不是失败**),不做任何 rclone 调用。
- 收尾时如果 `keep_versions > 0`,再跑一次 `prune`,而且是**尽力而为**:清理失败只记
  warning,绝不把一次成功的上传变成失败。

### 自动取回 `pull()` —— 「只取新的」

```
rclone copy <remote>/current/<key> <local> --create-empty-src-dirs --update
```

- `--update` 只在**源比目标新**时才覆盖。这就是它和恢复的区别:如果上一次上传失败
  (断网、机器崩了),本地存档比云端新,一次普通的恢复会把用户刚打出来的进度丢掉。
- 先 `lsf --dirs-only <remote>/current` 列出云端已有的 key,本地没在列表里的位置报
  `skipped("云端还没有这个位置的存档")` —— 云端从没见过这个游戏不是错误。
- **绝不带 `--backup-dir`**(拉取不该在本地造版本)。

### 手动恢复 `restore()` —— 「覆盖,且可撤销」

1. 校验 `version` 形如快照名(`is_snapshot`),否则直接失败,**一个 rclone 调用都不发**。
2. **先给现在的本地状态拍一张快照**(`snapshot_now`,尽力而为):所以恢复本身是可撤销的。
   `snapshot_now` 刻意**不复用** `upload` —— 后者会 prune,而恢复不能顺手过期掉一份快照。
3. `copy <remote>/current/<key> <local>`(**不带 `--update`**,显式恢复就是要有输出)。
4. 若指定了 `version`,再叠加
   `copy <remote>/versions/<version>/<key> <local>`。
   快照目录里存的是**当时被替换掉的文件**,是那一笔上传的 diff;所以"回到那个时刻"=
   先铺最新一份、再把它盖上去,而不是只拷快照。

---

## 4. `keep_versions` 滚动窗口

- **默认 `0` = 全部保留**。悄悄丢掉一份旧存档,比多占点空间糟糕得多。
- 上限 `MAX_KEEP_VERSIONS = 100`(`daemon/sync_rpc.rs`,超了在 `sync.set_settings` 就拒)。
- 计划由 `sync::prune_plan(versions, keep)` 算:
  `0` → 空;**只考虑看起来像我们自己快照的名字**(`is_snapshot`:16 字节底 + 可选
  `-<字母数字>` 后缀,`T`/`Z` 位置与数字位都要对);按字典序排序;**删最旧的**
  `len - keep` 个。
- 执行是 `rclone purge <versions>/<stamp>` —— 整个快照目录。**只删云端的快照目录**,
  本地文件永远不碰,别人放进 bucket 的目录也永远不碰。

---

## 5. 什么时候会自动同步

| 时点 | 动作 | 预算 | 失败会怎样 |
|------|------|------|-----------|
| `game.launch` 里、spawn 游戏**之前** | `pull`(`--update`) | `PULL_TIMEOUT` 30s | 写进回包的 `sync_pull` 与日志,**照常启动** |
| 会话广播 `SessionKind::Ended` 之后 | 等 `SETTLE_DELAY` 3s,再 `upload` | 每次 rclone 调用 300s | 记 `SyncRecord` + warning |
| `sync.now`(UI「立即同步」/ CLI) | `upload`(缺 `id` = 所有配了存档位置的游戏) | 同上 | 每个游戏各自一条 outcome |
| `sync.restore` | 快照 + `restore` | 同上 | 逐位置报告 |
| 每次 `upload` 之后(若 `keep_versions > 0`) | `prune` | 同上 | 只 warning |

两个设计约束值得盯住:

- **同步永远不拦游戏**。取回有硬超时,任何失败都只是"报告",不是"抛出去"。
- **`Ended` 是退出后上传的唯一触发**,而它的前提是"会话真的结束了" —— 所以退出看门狗
  (见 [architecture-scaling.md](architecture-scaling.md) §6)和云同步是同一件事的两半。
- 每个游戏"上次同步是什么时候、成没成"存在 `SyncState::records` 里,**只在内存**:
  daemon 一重启就回到"还没同步过"。它是给设置页看的状态,不是审计日志。

`sync.status` 的 `games[]` 会**现在就**试着解析每个存档位置,解析不了就报
`location_problem`(盘没挂、prefix 没了)—— 比等到同步时才炸强。

---

## 6. 凭据的四级存储与挑选顺序

**策略(用户 2026-09-13 拍板)**:默认就是**明文凭据文件(0600)**;密钥环"有就用、没有不
强求";主密码加密文件是给想更严的人的选项。**绝不再要求用户为了存一个 B2 key 去输主密码
或配置密钥环** —— 日常工具(opencode 的 `auth.json`、gh 的 `hosts.yml`)都是这么做的,
保护交给文件权限。

挑选顺序的唯一实现是 `secrets::Keyring::open_default`,从"最严"到"最省事":

| 序 | 这一级 | 什么时候选中 | 落点 |
|----|--------|--------------|------|
| ① | **主密码加密文件** | 文件存在就用(不需要密码就能"选它",解锁是后面的事) | `<config 目录>/secrets.json` |
| ② | **系统密钥环** | 探测通过(装了 **且** 在跑) | Secret Service,经 `secret-tool` |
| ③ | **明文凭据文件**(默认) | 上面两条都不成立 | `<config 目录>/credentials.json` |
| ④ | **内存** | 连文件都写不下去 | 进程内,退出即失 |

几条硬性规矩:

- **"装了" ≠ "在跑"**:`Keyring::system()` 必须真发一次查询(`lookup` 一个不可能存在的
  account)才算数。容器/最小桌面里 `secret-tool` 在、D-Bus 上没人应答,这时**不能**报
  "密钥环可用",也不能把读取失败伪装成"没存过"。
- **② 接管时会把明文搬进去**:`adopt_plain_entries` 逐条写进密钥环,**全搬完了才删**明文
  文件;搬不全就保留明文 —— 宁可留一份明文,也不能让凭据凭空少一条。
- **④ 是过渡态,不是落点**:UI 在 `store.kind == "session-only"` 时**拒绝保存**凭据与同步
  密码(`CredentialStore::needs_master_password`),两个保存按钮禁用并给出理由。
  ⚠ 但见文末"实现与文档不一致"一节:`open_default` 现在**不可能**返回内存那一级。
- **"锁定" 与 "没存过" 必须分开**:`StoreKind::EncryptedFile { locked }`,锁着时
  `validate_secrets` 报"已锁定,请先用主密码解锁"。
- **凭据绝不写进 `config.toml`**。
- **绝不自研密码学**:主密码文件用 RustCrypto 的 Argon2id(m=19MiB,t=2,p=1)+
  ChaCha20-Poly1305,信封头作为 associated data 参与认证(防降级)。
- **密码用户自设,我们绝不生成**;最短 `MIN_MASTER_PASSWORD = 8`。
- **取回提示跟着当前生效的那一级走**(`Keyring::lookup_hint`):密钥环给
  `secret-tool lookup …`;明文文件说"你自己打开就看得见";主密码文件说"只有主密码,
  忘了就只能删掉重设";内存说"取不回来"。说错比不说更坏。

### 明文凭据文件(`secrets/plain.rs`)

- JSON:`{"<account>": "<value>"}`,`account` 就是 `SecretKey::account()`
  (`b2-key-id` / `b2-app-key` / `sync-password` / `sync-password-obscured`)。
- **创建时就带 `OpenOptions::mode(0o600)`**(不是先建 0644 再 chmod,那会留一个窗口);
  读到更宽的权限**就地收紧并 warning**。
- 写入是**原子的**:同目录临时文件 + `rename`(同文件系统内才原子)。
- **坏文件是错误,不是"空"**:解析失败必须响亮地失败,否则用户会以为凭据丢了去重填。

### 主密码加密文件(`secrets/encrypted.rs`)

- `lock()` 只丢掉内存里的密钥,文件不动;`create()` 同时用于首次设置与改密码(整份重新封)。
- `daemon` 侧:`sync.set_master_password` 把当前生效存储里的所有条目封进文件,并**接住刚刚
  封好的那个句柄**(新开一个会是锁着的);`sync.clear_master_password` **不需要先解锁**
  —— 忘了主密码时它是唯一出路。

### Windows 现状

只有 Linux 的 Secret Service 后端实现了;`Keyring::system()` 在非 Linux 上返回
`BackendUnsupported`,于是按顺序落到**明文文件**(Windows 上是 `%APPDATA%` 的用户 ACL)。
凭据管理器后端属于 Windows 移植,尚未开始。`lookup_hint` 也**不会**把 Windows 用户指去
凭据管理器 —— 那里现在没有条目,承诺一条不存在的取回路径更坏。

---

## 7. 失败怎么说给人听

`sync/runner.rs::explain_failure` 把 rclone 的 stderr 归成几条"你该去检查什么",并**保留
原文**:`bad_auth_token`/`401`/`unauthorized` → keyID 是不是 Application Key ID(`005a…`)、
applicationKey 有没有复制全;`403`/`forbidden` → 凭据有效但这个 key 没有该 bucket 权限(要勾
bucket + Read and Write);bucket + `not found` → 检查 bucket 名与授权;
`no such host`/`connection refused`/`timeout`/`tls` → 网络 / 代理 / DNS。
日志前缀(`2026/09/11 23:54:35 CRITICAL: `)会被剥掉。位置级结果是 5 种之一:
`uploaded` / `pulled` / `restored` / `skipped` / `failed`(`runner::LocationOutcome`)。
**一个位置失败不会中断其余位置**,但整个游戏的 `ok` 会变成 false 并带上第一条失败的原因。

---

## 8. 已知未做 / 实现与文档不一致

- **真实的 B2 闭环没跑完**:上传 → 留快照 → 滚动删除 → 回滚,目前只用假 rclone 测过
  (真搬文件、断言桶内容);真机只验过 `sync test`。
- **GUI 看不到历史版本**:CLI 有 `sync versions` / `sync restore --version`,单游戏页只有
  「恢复」(恢复最新那一份)。**存档位置覆盖也极少**:绝大多数游戏没有配 `save_paths`,
  同步实际上没有东西可传。
- ⚠ **`open_default` 永远返回 `retry = false`**,所以 `SyncState::keyring()` 里那段
  "密钥环后来起来了 → 重新探测并把内存里的凭据搬过去"的逻辑在生产路径上**不可达**
  (`retry_backend` 只会被 `adopt()` 清掉、从没被置起)。四级存储里的第 ④ 级
  (**内存**)因此只在测试里出现;"四级"是设计,当前实现只有三级。后果:先在无密钥环会话里
  存了明文,再回到有密钥环的会话,凭据要等 daemon 重启才会迁移。
- `[sync]` 的六个字段(`enabled` / `endpoint` / `bucket` / `prefix` / `encryption` /
  `keep_versions`)在 `validate`、`rclone_env`、`remote_root`、`prune_plan` 里都被读到,
  没有"形同虚设的开关";相比之下 `ScaleProfile::follow_window` 确实是死字段
  (见 scaling 篇 §2)。

`TODO(未核对)`:B2 的 `b2` 后端在真实账号上的行为(端点发现、crypt 目录名加密的往返)、
以及 Windows 的 `%APPDATA%` ACL 是否等价于 0600 —— 都只从代码与注释推断,没有实测。
