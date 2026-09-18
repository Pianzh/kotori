# 云存档同步

**这篇讲什么**:双引擎(rclone 一版一 zip / kopia 快照)怎么选、存档以什么形状躺在
B2 里、自动取回与手动恢复为什么语义不同、保留窗口怎么算、以及凭据的存储与挑选顺序。
**什么时候读它**:改同步行为、改引擎、改凭据后端、或者排查"存档没上传 / 传错了 /
恢复没生效"的时候。

核对来源:`src/sync/**`、`src/daemon/sync_rpc/`、`src/secrets/`、`src/config/sync.rs`、
`tests/ipc_e2e/`。

---

## 1. 两个引擎,一个门面

`SyncConfig.engine`(`src/config/sync.rs`)选引擎,**全局一个,不是每个游戏一个**:

| 引擎 | 形态 | 加密 | 默认 |
|------|------|------|------|
| `kopia`(**默认**,要求 ≥0.22) | `<prefix>/kopia`,内容寻址仓库,一版一个快照 | **自带加密**(仓库密码默认 `kotori`) | ✅ |
| `rclone`(备选) | `<prefix>/games/<id>/<stamp>.zip`,一版一个完整 zip | **不加密**,落桶即明文 | — |

- 两个引擎在桶里**各写各的区域**,布局互不相通;同一个桶里混用只会让"有些存档看不见"
  变成一件要靠猜的事。换机器(双系统)也必须选同一个。
- 上层(`sync/runner/`)只说"这一版",不关心它是个 zip 还是 kopia 快照;统一由
  `Backend` 门面(`src/sync/engine/mod.rs`)收口,`send`/`fetch`/`versions`/`remove`
  两边各自实现。
- 两边的版本名是**同一套**(`version_stamp` 的产物;kopia 把它写进快照的 description),
  所以"最新的一版"、"保留最近 N 版"这些语义逐字相同,上层代码一行都不用分叉。

文件分工:`engine/`(rclone.rs / kopia.rs / kopia_args.rs / diagnostics.rs)知道"这一版
怎么上云、怎么取回",`archive/`(gather/pack/unpack/materialize)知道一个包里有什么,
`runner/`(upload/pull/restore/staging/outcome)把两者接起来 —— 解析存档位置、做合并
判定、如实汇报;`rclone_env.rs` / `rclone_args.rs` / `remote_paths.rs` / `snapshots.rs` /
`validate.rs` 是 rclone 路径和纯函数那一半。

---

## 2. 云端布局

```
rclone:<bucket>/<prefix>/                     ← remote_root(),例:kotori:kotori-saves/kotori
└── games/<game_id>/<stamp>.zip               ← 一版一个完整的包,没有别的目录
```

- **一版一个包,整份上、整份下**。没有 `current/` + `versions/` 那一对目录了:
  "最新的一版"就是**名字最大的那个包**,不额外维护指针文件 —— 少一个会写坏的东西。
  "回到某一版"= 把那个包铺下去,不是"先铺最新的、再把当时的 diff 叠上去"。
- `bucket` 与 `prefix` 都来自 `[sync]`;`prefix` 默认 `kotori`。拼接规则见
  `remote_root`(任一个为空都还能用,只是路径短一段)。
- `save_key` 由存档位置的"描述"推导,不是它的下标(`remote_paths::save_key`):
  `<win|rel|abs>-<小写、非字母数字折成下划线、去重下划线、截断 48 字符>`。
  例:`%APPDATA%\Game\save` → `win-appdata_game_save`,`savedata` → `rel-savedata`。
  **在 UI 里调整列表顺序不会打乱云端已有的东西**,三种 kind 之间也不会撞名。
- **包内结构**(`archive/`):每个存档位置占一个顶层目录 `<save_key>/<相对路径>`,包根
  有一份 `kotori-manifest.json`(格式版本 + 打包时刻 + 每个文件的 `size` / `mtime_ms`
  + 这一版包含哪些存档位置)。判"谁新"**只看清单里的这两个数**:zip 条目自带的
  时间戳只有 2 秒精度,不参与任何判断。包内路径必须干净(`safe_rel` 拦 `..` / 绝对
  路径),否则恢复一个被改过的包时覆盖的就不只是存档了。

kopia 那条路:**整个仓库**落在 `<prefix>/kopia`(`engine::repo_prefix`),一个游戏一版 =
一个快照,快照的 description 就是 `version_stamp`。恢复后的目录结构与 zip 里的
一模一样,`archive::plan` 的合并判定原样复用。

---

## 3. rclone 引擎:一版一个 zip

桶里只有 `copyto`(上传一个对象)/ `deletefile`(删掉一个对象)/ `lsf --files-only`
(列目录)三种调用,不存在"目录对目录地合并"。

### 凭据怎么交出去

凭据**只经子进程环境**传递(`rclone_env::rclone_env`),绝不进 argv(`ps` 全世界可读),
也不写任何配置文件:

| 环境变量 | 值 |
|----------|-----|
| `RCLONE_CONFIG` | `/dev/null`(Windows 上是 `NUL`)—— 忽略机器上任何 `rclone.conf`,包括用户自己的 |
| `RCLONE_CONFIG_KOTORI_TYPE` | `b2` |
| `RCLONE_CONFIG_KOTORI_ACCOUNT` / `_KEY` | keyID / applicationKey |
| `RCLONE_CONFIG_KOTORI_ENDPOINT` | 仅当 `[sync].endpoint` 非空 |

**只有这两个 B2 值**:没有同步密码、没有 crypt 层 —— rclone 这条路**不提供任何
加密**,要加密就选 kopia。

**没有 region**:原生 `b2` 后端从凭据自己发现 API 主机。

**`endpoint` 的规矩**(`sync::validate_endpoint`):默认留空。填了必须是完整 URL(含
`https://`);`s3.<region>.backblazeb2.com` 被**明确拒绝** —— 那是 B2 的 S3 兼容接口,
和原生 b2 后端不是同一个 API,发过去只会得到费解的 404。

### 结构校验 vs 凭据校验

- `sync::validate`:必须 `enabled`、`bucket` 非空、`endpoint` 合法(没有 `encryption`
  字段 —— 它随 crypt 层一起删了;磁盘上的旧配置多带这个键也能正常加载,只是下次写回
  不再带上)。
- `sync::validate_secrets`:**主密码文件锁着** → 报"已锁定"而**不是**"还没有凭据";
  B2 两个键缺任一 → 报错。不再检查任何同步密码。
- `sync.status` 只在 `enabled` 时把这两条的结果放进 `problem`;`ready` 还额外要求
  **当前选中的引擎二进制找得到**(rclone 找不到不拦 kopia,反之亦然)。

### 失败解释

`engine/diagnostics.rs::explain_failure` 把 rclone 的 stderr 归成几条"你该去检查什么",
并保留原文:`bad_auth_token`/`401`/`unauthorized` → keyID 是不是 Application Key ID、
applicationKey 有没有复制全;`403`/`forbidden` → 凭据有效但这个 key 没有该 bucket
权限(要勾 bucket + Read and Write);bucket + `not found` → 检查 bucket 名与授权;
`no such host`/`connection refused`/`timeout`/`tls` → 网络 / 代理 / DNS。日志前缀
(`2026/09/11 23:54:35 CRITICAL: `)会被剥掉。

---

## 4. kopia 引擎:内容寻址的仓库

`engine/kopia.rs`。kopia 快照的是**目录树**,而一个游戏的存档位置散在好几个地方 ——
所以这一版先按 zip 那条路的老规矩摆成一个目录(`archive::materialize`:`<key>/<相对路径>`
+ manifest),再对它拍**一次**快照。一个游戏一个版本 = 一个快照,回退是"恢复那一个",
不是"拼好几个"。

几个必须知道的坑(都实测过):

- **连接状态**由 `KOPIA_CONFIG_PATH` 指向的那份配置决定(`<data_dir>/kopia/
  repository.config`,旁边 `target.txt` 记着当时连的是哪个桶/prefix,桶或 prefix 一换就
  重连)。它不在时先 `connect` 再 `create`(桶里可能已有仓库,也可能没有)。
- **默认仓库密码 `kotori`**(`DEFAULT_PASSWORD`),所有端一致 —— 双系统/多机互通,
  "自己下载 kopia 读"的人知道该试什么。用户可以在设置页设一个更强的
  (`sync.set_kopia_password`,留空 = 清掉并回到默认值;UI 在回话里收到
  `using_default` 会如实提醒"任何拿到这个 bucket 的人都能解开仓库")。改它等于把
  所有老仓库锁在门外,有测试盯着。
- `snapshot delete` **默认只演练**,必须带 `--delete`(见 `kopia_args`)。
- kopia 默认往 `~/.cache/kopia` 写日志,目录不存在时每一步都吐 `write error`。
  所以 `KOPIA_LOG_DIR` 必须指到自己的数据目录下;`KOPIA_CACHE_DIRECTORY` 同理。
- **B2 的 keyID / applicationKey 只能进 argv**(kopia 的 b2 后端不像 rclone 那样
  吃环境变量注册远端),这是"秘密绝不进命令行"那条规矩唯一的、上游强制的例外;
  仓库密码始终只走 `KOPIA_PASSWORD` 环境变量。`KOPIA_USE_KEYRING=false` 是刻意的:
  让 kopia 自己去翻 gnome-keyring 会在没有桌面会话的地方报一堆用不上的错。
- `KOTORI_KOPIA_REPOSITORY` 可以把仓库放到一个**本地目录**(NAS、挂载盘)而不是 B2:
  布局完全一致,只是仓库在哪不同(测试也靠它)。
- 保留窗口到期要删快照时,`remove` 先按 description 找到快照 id 再删;找不到就当
  "已经不在了"直接返回(保留窗口是 best-effort)。

`find_kopia()` 顺序(`executables::find`,rclone 同款):设置页里填的位置 >
`KOTORI_KOPIA`(rclone 是 `KOTORI_RCLONE`) > **kotori 可执行文件旁边** > `PATH`,
两个引擎各找各的,互不干扰。前三步都要求那里**真的有那个文件**,填错不会静默退到
PATH。「旁边」正是 Windows 发布包的落点:release 把官方 **kopia.exe(版本锁 0.23.1)**
一起打进发布目录,放在 kotori.exe 旁边(`.github/workflows/release.yml`)——
Windows 上开箱即用靠的就是这一步。

---

## 5. 版本包:命名、识别与保留窗口

`snapshots.rs`。包名是整个保留策略的锚点:

- **`stamp` 由 `version_stamp` 生成**:`%Y%m%dT%H%M%S%3fZ-<8 位随机 hex>`
  (毫秒精度 UTC)。两个性质都承重:
  - 字典序 == 时间序(等长、零填充),"最新的一版"就是 `max`,裁剪就是一个排序;
  - 名字唯一(随机后缀)。从前只有秒精度,于是**同一秒内的两次上传**(游戏刚退出就点
    「立即同步」)谁新谁旧全看随机后缀 —— e2e 里"恢复到最新"因此拿到过上一版;
    毫秒精度把撞车压到"同一毫秒还要两次上传"。
- **`is_snapshot` 认名字**:19 字符(毫秒)或 16 字符(秒)+ 可选 `-<字母数字>` 后缀,
  `T`/`Z` 位置与数字位都要对。**秒精度的老包照样认**,所以一个旧桶升级之后不会
  "看不见自己的包"。列出 `lsf --files-only` 输出时,不像我们自己的名字一律丢弃
  (`parse_packages`)—— 这正是 `prune_plan` 敢下刀的前提。
- **`keep_versions` 滚动窗口**:默认 `0` = 全部保留(悄悄丢掉一份旧存档,比多占点
  空间糟糕得多);上限 `MAX_KEEP_VERSIONS = 100`(`sync.set_settings` 就拒)。
  `prune_plan` 按字典序排序,**删最旧的** `len - keep` 个;执行是 rclone `deletefile`
  那个包 / kopia `snapshot delete`。**只删云端的自家包**,本地文件永远不碰,别人放进
  bucket 的东西也永远不碰。

---

## 6. 三个动作,三种语义(不要合并)

协调在 `runner/`(upload.rs / pull.rs / restore.rs),都是由 `Runner` 对外提供的。汇报
类型 `outcome.rs`:`LocationOutcome { configured, local, action, detail }`,action 六种:
`uploaded` / `pulled` / `kept` / `restored` / `skipped` / `failed`。

### 上传 `upload()` —— "我说了算"

1. `version_stamp` 取一个当前时刻的包名。
2. `archive::pack` 把所有**本机有**的存档位置打成一个 zip(排除规则、软链接不跟随
   都在 `gather` 里),再 `copyto` 到 `<games/<id>/<stamp>.zip`。
3. **本地没有的目录报 `skipped`,不是失败**(一台机器上只在 Windows 才有的目录很正常);
   整个游戏一个存档目录都没有 → **空包根本不上传**(只会往版本列表里塞垃圾)。
4. 一个位置失败不中断其余位置,但整个游戏的 `ok` 变 false 并带上第一条失败的原因。
5. 上传成功后若 `keep_versions > 0`,再跑一次 `prune`,**尽力而为**:清理失败只记
   warning,绝不把一次成功的上传变成失败。

> 全量上传,**不做"内容没变就跳过"**:想省空间的人用 kopia(用户 2026-09-15 明确)。
> 换来的是"一个包 = 一个时间点的完整存档"。

### 自动取回 `pull()` —— 「只取新的」

启动前自动取回。**从前这条不变量是 `rclone --update` 保证的** —— 它逐文件比较、只
覆盖更新的那些;改成一版一包之后 rclone 不再看文件,所以这条保证搬到了我们自己手里:

1. 取"最新一版"的包名(`max`),云端从没见过这个游戏 → 每个位置 `skipped("云端还
   没有这个游戏的存档")`,**不是错误**。
2. 下载包、解开、读 manifest。
3. `archive::plan(manifest, targets, Merge::Newer)`:按 `mtime_ms` 逐文件比较,**只铺
   比本机新的**;本机更新的一律不动(`kept`,detail 说"本机更新,保持不动")。
4. 超时预算 `PULL_TIMEOUT` 30s,失败只报告不拦启动 —— 但**报价必须诚实**:超时或
   失败要报成"没取完",绝不报成"云端没有存档"。

期间的东西落在 `work_dir`(`<data_dir>/sync`,不在存档目录旁边 —— 那儿多出来的临时
文件会被下一次打包收进去),经 `staging`(`stage-<uuid>`,`Drop` 收尾)铺回存档目录。
上次 daemon 被杀/崩了留下的 `stage-*` 残骸由 **daemon 启动时清掉**
(`runner::sweep_stale`,拿到进程锁之后才动手)—— Windows 上尤其要紧:`%TEMP%` 不像
Linux 那样有人定期扫,而这些目录就在数据目录下,一个几 MB、大的上百 MB,攒着白占
磁盘。

### 手动恢复 `restore()` —— 「覆盖,且以包为准」

1. 校验 `version` 形如包名(`is_snapshot`),否则直接失败,**一个引擎调用都不发**。
2. 不带 `version` = 恢复最新一版;带 = 恢复那**一版**(它自带那一刻所有位置的所有文件,
   回退就是铺它,不是拼 diff)。
3. `archive::plan(manifest, targets, Merge::Replace)`:**以包为准,本机更新的也盖掉**
   (用户点了"恢复"就是他说了算)。
4. **恢复前不再把本机推上云**——那一手（自保快照）正是从前"恢复到最新"变成空操作的
   根因。恢复的可撤销性由"上一版还在"保证。
5. 回退之后本机可能还剩这一版里没有的文件(游戏照旧可能读到它们):**只报不删**,detail
   里列出最靠前的几个(`本机另有 N 个文件不在这一版里（保留未动）`),删本地数据永远是
   用户点头才做的事。

---

## 7. 什么时候会自动同步

| 时点 | 动作 | 预算 | 失败会怎样 |
|------|------|------|-----------|
| `game.launch` 里、spawn 游戏**之前** | `pull`(`Merge::Newer`) | `PULL_TIMEOUT` 30s | 写进回包的 `sync_pull` 与日志,**照常启动** |
| 会话广播 `SessionKind::Ended` 之后 | 等 `SETTLE_DELAY` 3s,再 `upload` | 每次引擎调用 300s | 记 `SyncRecord` + warning |
| `sync.now`(UI「立即同步」/ CLI) | `upload`(缺 `id` = 所有配了存档位置的游戏) | 同上 | 每个游戏各自一条 outcome |
| `sync.restore` | `restore` | 同上 | 逐位置报告 |
| 每次 `upload` 之后(若 `keep_versions > 0`) | `prune`(best-effort) | 同上 | 只 warning |

两个设计约束值得盯住:

- **同步永远不拦游戏**。取回有硬超时,任何失败都只是"报告",不是"抛出去"。
- **`Ended` 是退出后上传的唯一触发**,而它的前提是"会话真的结束了" —— 所以退出
  看门狗(见 [architecture-scaling.md](architecture-scaling.md) §6)和云同步是同一件事
  的两半。
- 每个游戏"上次同步是什么时候、成没成"存在 `SyncState::records` 里,**只在内存**:
  daemon 一重启就回到"还没同步过"。它是给设置页看的状态,不是审计日志。

`sync.status` 的 `games[]` 会**现在就**试着解析每个存档位置,解析不了就报
`location_problem`(盘没挂、prefix 没了)—— 比等到同步时才炸强。

---

## 8. 凭据的存储与挑选顺序

**策略(用户 2026-09-13 拍板)**:默认就是**明文凭据文件(0600)**;密钥环"有就用、没有
不强求";主密码加密文件是给想更严的人的选项。**绝不再要求用户为了存一个 B2 key 去
输主密码或配置密钥环** —— 日常工具(opencode 的 `auth.json`、gh 的 `hosts.yml`)都是
这么做的,保护交给文件权限。

挑选顺序的唯一实现是 `secrets::Keyring::open_default`,从"最严"到"最省事":

| 序 | 这一级 | 什么时候选中 | 落点 |
|----|--------|--------------|------|
| ① | **主密码加密文件** | 文件存在就用(不需要密码就能"选它",解锁是后面的事) | `<config 目录>/secrets.json` |
| ② | **系统密钥环** | 探测通过(装了 **且** 在跑) | Secret Service,经 `secret-tool` |
| ③ | **明文凭据文件**(默认) | 上面两条都不成立 | `<config 目录>/credentials.json` |
| ④ | **内存** | 连文件都写不下去 | 进程内,退出即失 |

⚠ 见文末第 10 节:`open_default` 现在**不可能**返回内存那一级,"四级"是设计,实现是三级。

认得的密钥只有三个(`secrets::SecretKey::{B2KeyId, B2AppKey, KopiaPassword}`),
`account()` 分别是 `b2-key-id` / `b2-app-key` / `kopia-password` —— **同步密码
(`sync-password` / `sync-password-obscured`)已随 crypt 层整个删掉**。kopia 密码是
存量用户会读到新东西:它**有默认值**(`kotori`),清空 = 回到默认。

几条硬性规矩:

- **"装了" ≠ "在跑"**:`Keyring::system()` 必须真发一次查询(`lookup` 一个不可能
  存在的 account)才算数。容器/最小桌面里 `secret-tool` 在、D-Bus 上没人应答,这时
  **不能**报"密钥环可用",也不能把读取失败伪装成"没存过"。
- **② 接管时会把明文搬进去**:`adopt_plain_entries` 逐条写进密钥环,**全搬完了才删**
  明文文件;搬不全就保留明文 —— 宁可留一份明文,也不能让凭据凭空少一条。
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

- JSON:`{"<account>": "<value>"}`,account 就是 `SecretKey::account()`。
- **创建时就带 `OpenOptions::mode(0o600)`**(不是先建 0644 再 chmod,那会留一个窗口);
  读到更宽的权限**就地收紧并 warning**。
- 写入是**原子的**:同目录临时文件 + `rename`(同文件系统内才原子)。
- **坏文件是错误,不是"空"**:解析失败必须响亮地失败,否则用户会以为凭据丢了去重填。

### 主密码加密文件(`secrets/encrypted.rs`)

- `lock()` 只丢掉内存里的密钥,文件不动;`create()` 同时用于首次设置与改密码(整份
  重新封)。
- `daemon` 侧:`sync.set_master_password` 把当前生效存储里的所有条目封进文件,并
  **接住刚刚封好的那个句柄**(新开一个会是锁着的);`sync.clear_master_password`
  **不需要先解锁** —— 忘了主密码时它是唯一出路。

### Windows 现状

只有 Linux 的 Secret Service 后端实现了;`Keyring::system()` 在非 Linux 上返回
`BackendUnsupported`,于是按顺序落到**明文文件**(Windows 上是 `%APPDATA%` 的用户
ACL)。Windows 凭据管理器后端**尚未实现** —— 移植的其他部分已经能跑(IPC、单实例锁、
UI 都验过了),只有 keyring 这一块没接。`lookup_hint` 也**不会**把 Windows
用户指去凭据管理器 —— 那里现在没有条目,承诺一条不存在的取回路径更坏。

---

## 9. 位置级与游戏级汇报

`runner/outcome.rs`:

- **位置级**六种动作之一:`uploaded` / `pulled` / `kept` / `restored` / `skipped` /
  `failed`,附带一行给人看的 `detail`。
- **游戏级**:`GameOutcome { game_id, name, ok, locations[], error? }`。**一个位置
  失败不会中断其余位置**,但整个游戏的 `ok` 会变 false 并带上第一条失败的原因。

---

## 10. 已知未做 / 实现与文档不一致

- **真实的 B2 闭环没跑完**:上传 → 留版本 → 滚动删除 → 回滚,目前只用假引擎测过
  (真搬文件、断言桶内容);真机只验过 `sync test`。
- **GUI 看不到历史版本**:CLI 有 `sync versions` / `sync restore --version`,单游戏页
  只有「恢复」(恢复最新那一份)。**存档位置覆盖也极少**:绝大多数游戏没有配
  `save_paths`,同步实际上没有东西可传。
- ⚠ **`open_default` 永远返回 `retry = false`**,所以 `SyncState::keyring()` 里那段
  "密钥环后来起来了 → 重新探测并把内存里的凭据搬过去"的逻辑在生产路径上**不可达**
  (`retry_backend` 只会被 `adopt()` 清掉、从没被置起)。三级存储里的第 ④ 级
  (**内存**)因此只在测试里出现。后果:先在无密钥环会话里存了明文,再回到有密钥环的
  会话,凭据要等 daemon 重启才会迁移。
- **kopia 引擎没有真机闭环**:`connect` → `create` → 快照 → 恢复 → 滚动删除只在
  本地目录仓库(`KOTORI_KOPIA_REPOSITORY`)与假二进制上测过,B2 仓库的真实往返
  (包括密码与 `target.txt` 失效时的重连)没有实测。
- `[sync]` 的五个字段(`enabled` / `engine` / `endpoint` / `bucket` / `prefix` /
  `keep_versions`)在 `validate`、`rclone_env`、`remote_paths`、`prune_plan` 里都被
  读到,没有"形同虚设的开关";相比之下 `ScaleProfile::follow_window` 确实是死字段
  (见 scaling 篇 §2)。

`TODO(未核对)`:B2 的 `b2` 后端在真实账号上的行为(端点发现)、kopia B2 仓库的往返、
以及 Windows 的 `%APPDATA%` ACL 是否等价于 0600 —— 都只从代码与注释推断,没有实测。