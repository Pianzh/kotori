# 配置 Backblaze B2 同步：从零开始

这份文档写给第一次用 Backblaze B2 的人，跟着做一遍大约 10 分钟。

同步有两个引擎，在设置页里选：

| 引擎 | 一句话 | 加密 |
|------|--------|------|
| **kopia**（默认） | 内容寻址仓库，去重、增量、快照 | **自带加密**（仓库密码默认 `kotori`） |
| **rclone**（备选） | 一版一个 zip，谁都能用 B2 网页把包拿下来解开 | **不加密**，落桶即明文 |

默认引擎是 kopia：bucket 里的存档在 kopia 仓库里，增量、去重、加密都在这条路上。想要「谁都能用 B2 网页直接解开」的普通 zip 才换 rclone（见 §7）。

**两个引擎各写各的区域，换引擎不会读到对面的存档；换机器（双系统）必须选同一个。**

---

## 1. 在 B2 上建 bucket

登录 <https://secure.backblaze.com/b2_buckets.htm>，左侧选 **Buckets** → **Create a Bucket**。

| 字段 | 填什么 | 为什么 |
|------|--------|--------|
| Bucket Unique Name | 自己起，比如 `kotori-saves-你的名字` | 全局唯一，重名要换一个 |
| Files in Bucket are | **Private** | 存档不该公开可读 |
| Default Encryption | **Disable** | 加密由 kopia 引擎做；这里开着也没坏处，只是没必要 |
| Object Lock | **Disable** | 用不上 |

建好之后列表里会显示一个 `Endpoint: s3.us-west-004.backblazeb2.com` 之类的值。

> ⚠️ **这个 Endpoint 我们不需要。** 那是 B2 的 S3 兼容接口，而 kotori 用的是 B2 原生接口。如果你把它填进 kotori 的 API endpoint 框，程序会明确拒绝并告诉你留空 —— 这是刻意的，不然第一次同步会以一个看不懂的 404 结束。

## 2. 在 B2 上建一把只能访问这个 bucket 的钥匙

左侧 **Account** → **Application Keys** → **Add a New Application Key**。

| 字段 | 填什么 |
|------|--------|
| Name of Key | `kotori` |
| Allow access to Bucket(s) | **只勾你刚建的那个 bucket**（不要选 All） |
| Type of Access | **Read and Write** |
| Allow List All Bucket Names | 不用勾 |
| File name prefix / Duration | 留空 |

点 **Create New Key**。页面会显示两个值：

- **keyID** —— 形如 `005a1b2c3d4e5f6000000001a`
- **applicationKey** —— 一长串

⚠️ **applicationKey 只显示这一次**，关掉页面就再也看不到了（看不到就删掉这把 key 重建一把，不影响已上传的存档）。把两个值复制出来。

> keyID 不是你的 B2 账号 ID。kopia 和 rclone 都要求用 Application Key ID，填账号 ID 会得到 401。

## 3. 填进 kotori

在界面左侧选 **云同步**（不是「设置」那一页）：

1. 打开 **启用云同步** 开关
2. **引擎** 保持 `kopia`（默认，自带加密；想拿明文 zip 再换 `rclone`，见 §7）
3. **bucket** 填第 1 步建的那个 bucket 名字（不是 endpoint，也不是网址）
4. **prefix** 保持 `kotori`。它是 bucket 里归 kotori 独占的目录，bucket 里的其他东西我们一律不碰
5. **API endpoint 留空**
6. **保留版本数** 保持 `0`（= 永不删除云端版本）
7. 点 **保存设置**
8. 在 **B2 凭据** 里填 keyID 与 applicationKey，点 **保存凭据**
9. 点 **测试连接**

看到 `连接正常：kopia …`（显示 kopia 的二进制路径）就通了。换成 rclone 后会显示 `连接正常：rclone … kotori:<你的 bucket>/kotori` —— 这一行标明连的是哪一边。

命令行同样可以看状态和测连接：

```bash
kotori sync status   # 缺什么会直接写出来
kotori sync test     # 只测凭据 + bucket + 读写权限
```

## 4. 告诉 kotori 存档在哪

同步的对象是**你手动指定的存档位置**。在游戏详情页（游戏库 → 点某个游戏 → 存档位置）里配置：

| 类型 | 什么时候用 | 例子 |
|------|-----------|------|
| `windows` | 存档在 AppData / 文档 / Saved Games 里（**绝大多数 galgame**） | `%APPDATA%\会社名\游戏名` |
| `relative` | 存档就在游戏目录里 | `savedata` |
| `absolute` | 只在这台机器有效，不参与跨平台映射 | `/home/你/saves/xxx` |

怎么找存档位置：游戏设置里一般能看到，或者去 `~/.wine/drive_c/users/<你的用户名>/AppData/Roaming/` 下面翻。每个位置还可以填排除规则（比如 `*.log`、`cache/`）。

点「浏览…」挑一个位置，类型会自动识别并改写（识别顺序：relative → windows 令牌 → absolute）。

> 用 `windows` 类型时**不要**写死 `C:\users\你的用户名\...`：wine 里的用户名和真实 Windows 上的可能不一样，跨系统会认不出来。用 `%APPDATA%` 这类令牌，两边语义一致。

## 5. 日常怎么用

先补一句「认人」：云端只有**存档包 + 身份卡**，本机的游戏库（路径、exe 位置、缩放档案）**不上云**。两台机器认「我这一款就是你的那一款」靠 **exe 指纹**（文件大小 + 头尾各 1 MiB 的哈希），**唯一命中才自动绑**，说不清就列出来问你 —— 宁可不动，也不猜。它在你看得见的几个地方出现：

- **添加游戏**页：exe 一填好就自动问一次「云端有没有这一款」。指纹**唯一命中**就替你选上，点「添加游戏」成功后顺手认领；命中**多条**列出来让你挑；**一个都没有**时给一颗 **「自己选…」**，弹一个带搜索的浮层，从云端清单里挑一条绑上。**问不成 / 超时绝不挡添加** —— 那一块灰着说一句「没问成云端」，本机照样建。
- **云端存档**（左侧导航第三项）：像游戏库那样一页 —— 搜索、列表、点开某一款看它每一版（版本名、大小、时间）。显示的是**云端记下的游戏名**（不是本机名字，云端有、本机没装的也在这里），每行还标着本机这一侧认没认。两颗按钮：**刷新**（读索引，这时才真正联网；平时看的是本机缓存，界面上写着「本机缓存 · 什么时候拿到的」）与**深度扫描云端**（读所有身份卡重建索引，慢；还会顺手把指纹唯一命中的绑上）。
- **启动游戏前**：如果这一款在云端**认不出号**，会先弹一次问清楚（没问题就同步这一款 / 新建一条身份 / 关掉这一款的同步），**答完才起**。选「关掉这一款的同步」只关这一款的自动同步，手动「立即同步 / 恢复」不受影响。

配好之后基本不用管：

- **启动游戏前**：自动把云端**较新的文件**取回来（逐文件比修改时间，**绝不覆盖本地更新的存档**），最多等 30 秒，失败只提示、不挡你玩游戏
- **游戏退出后**：等 3 秒（让写入落盘）自动把改动同步成一版上传
- 手动：游戏库里每个游戏后面有 **同步 / 恢复**

命令行：

```bash
kotori sync now                  # 全部同步一次
kotori sync now <游戏id>         # 只同步一个
kotori sync versions <游戏id>    # 看云端版本列表
kotori sync cloud                # 云端有哪些游戏（含本机没装的）
kotori sync restore <游戏id>                          # 恢复到最新
kotori sync restore <游戏id> --version 20260911T101500123Z-1a2b3c4d  # 回滚到某个版本
```

**恢复是安全的**：它直接铺那一版，而**每一版都是一个完整的快照** —— 所以回滚到上一版就是撤销，不需要「先给当前状态做安全快照」那一手。恢复后本机多出来的、不在那一版里的文件**只列出来、绝不删除**。

**回滚某个版本**的语义是「回到那一次上传的时刻」：那一版自带那一刻所有位置的所有文件，铺下去就是那个时刻，不需要拼差量。

### 云端的删除

「云端存档」页里能删三样东西，每一个都会先弹一次确认：

| 操作 | 在哪 | 结果 |
|------|------|------|
| 删除某一版 | 云端存档 → 某一款 → 某一版 | 只删这一版，本机不动 |
| 清空这一款 | 云端存档 → 某一款 → 详情页最下面 | 这款的存档全没了，**游戏条目留着** —— 之后同步还传到同一条 |
| 删除条目 | 同上 | 这条身份连它的全部存档一起删，不留没人认领的数据 |

想删自己机器上的存档（覆盖本机），在**游戏库 → 单游戏设置 → 云存档**那一块做，那里的「取回」是明确的动作。

## 6. 存在哪、怎么自己拿回来

rclone 引擎下（备选），bucket 里的结构是：

```
<bucket>/kotori/games/<游戏id>/20260911T101500123Z-1a2b3c4d.zip   ← 一版一个完整 zip
<bucket>/kotori/games/<游戏id>/kotori-game.json                   ← 这一款的身份卡
<bucket>/kotori/index/kotori-index.json                           ← 一个桶一份的索引
```

登录 B2 网页就能直接下载这些 zip 解开 —— **不需要 kotori，也不需要 rclone**。版本名是 UTC 毫秒时间戳 + 随机后缀，字典序就是时间序，「最新」= 名字最大的那个，没有别的指针文件。每个游戏目录里那份 `kotori-game.json` 是身份卡（两台机器靠它认「这一款是同一款」，还带着 exe 指纹），`index/` 里是云端索引（加速用的镜像，身份卡才是真相）；它们看不懂也不影响你拿包，别删就行。

kopia 引擎（默认）下：整个仓库在 `<bucket>/kotori/kopia`，是 kopia 自己的格式，要用 `kopia` 客户端（或 kotori 的 `sync restore`）来读，不能直接在网页里解包。

| 东西 | 存在哪 | 你怎么取回来 |
|------|--------|-------------|
| B2 keyID / applicationKey | 系统密钥环 / 明文文件 / 主密码文件（按顺序挑） | `secret-tool lookup service kotori account b2-key-id`（applicationKey 换 `b2-app-key`） |
| kopia 仓库密码（默认引擎用得上） | 同上 | `secret-tool lookup service kotori account kopia-password`（留空 = 默认 `kotori`） |

`config.toml` 里**没有**任何密钥：只有 bucket 名、prefix、保留版本数这些非敏感设置。

> **凭据默认存明文文件而不是内存**：密钥环有就用、没有不强求，主密码文件是给想更严的人的选项。本机没有可用密钥环时，凭据落到配置目录下的 `credentials.json`（**权限 0600**，创建就带），重启不用重填。

### 本机没有密钥环怎么办

凭据优先存进**系统凭据库**（Linux 是 Secret Service，也就是 KDE 钱包 / GNOME keyring）。但有些环境**不会自动启动它**：niri / sway 这类只有窗管没有桌面的会话、容器、纯 TTY。此时 kotori 会自动**退回明文文件（0600）** —— 没有密钥环也照常能用。

`kotori sync status` 看「密钥环」那一行，会如实告诉你当前是哪种：

| 状态 | 含义 | 怎么办 |
|------|------|--------|
| `Secret Service (libsecret)` | 正常 | 不用管 |
| `明文凭据文件`（默认回退） | 本机没有运行中的密钥环 | 不用管，权限 0600；怕丢就拿走整个文件 |
| `主密码加密文件 …（已锁定）` | 用主密码保的凭据，需要解锁 | `kotori sync unlock` 或界面里解锁 |

**想更严**（跨平台、不依赖桌面环境）：在云同步页的「凭据存储」一节里输入主密码点「加密保存凭据」，或者：

```bash
kotori sync master-password   # 提示输入两次，密码不回显、不进 shell 历史
kotori sync unlock            # 下次开机解锁一次即可
```

凭据会用 Argon2id 派生 + ChaCha20-Poly1305 加密后写到配置目录下的 `secrets.json`（权限 0600），**里面没有明文**。主密码由你自己保管，我们不会存它 —— 忘了就打不开这个文件（重新填一次 B2 凭据即可，云端数据不受影响）。

> Windows 端也还没有接上系统凭据管理器，所以那边同样走明文凭据文件 / 主密码文件这条路。

## 7. 加密与引擎（kopia 默认自带）

**kopia（默认）自带加密**：仓库没有密码读不出来。rclone 那条路才**没有加密**（一版一个 zip，zip 里就是明文）。想换到明文 zip：

1. 云同步页的**引擎**选 `rclone`，保存设置（会有「引擎已切换」的提示 —— 两个引擎各写各的区域，换过去之后原来 kopia 的仓库还在桶里，只是不再被读到，**数据没丢**）。
2. 装 kopia（默认引擎用得上）：
   - **Linux**：走发行版仓库。Arch 官方仓库没有，用 `sudo pacman -S archlinuxcn/kopia`（Debian/Ubuntu: `sudo apt install kopia`，Fedora: `sudo dnf install kopia`）。
   - **Windows**：**不用装** —— 发布包里已经内置 kopia.exe（就在 kotori.exe 旁边）。
3. **仓库密码默认 `kotori`**，所有端一致，双系统直接互通。想设自己的：在 kopia 那一组里填一个新密码点保存（留空 = 清掉并回到默认）。

**换之前请注意**：

- 密码**由你自己设定**（默认 `kotori` 谁都能猜到 —— 拿到 bucket 的人就能解开仓库，界面会如实提醒），我们不会替你生成更随机的。
- **同一个 bucket 里不要 rclone 和 kopia 混用**：两者各写各的区域，混用只会让「有些存档看不见」。要换就全局换，换机器也保持一致。
- 仓库密码丢了，已上传的存档读不回来（B2 凭据还在也只能再写新的）。它存在凭据库里，也能用上面那条 `secret-tool` 命令取回来。
- 想省空间不用多选：kopia（默认）就是增量 + 去重；rclone 那条路是全量上传。

## 8. 出问题先看这里

```bash
kotori sync status   # 远端、引擎二进制、凭据、每个游戏上次同步的结果
kotori sync test     # 一次验证凭据 + bucket + 读写权限
kotori sync --help
```

常见错误与对策（kotori 会把引擎的原话附在后面）：

| 提示 | 原因 |
|------|------|
| **B2 不认这组凭据** | keyID 填成了账号 ID；或者 applicationKey 没复制全 |
| **这个 key 没有这个 bucket 的权限** | 建 key 时没勾这个 bucket，或 Type of Access 不是 Read and Write |
| **找不到这个 bucket** | bucket 名字拼错 |
| **这是 B2 的 S3 兼容接口地址** | API endpoint 填了 `s3.<region>.backblazeb2.com`，留空即可 |
| **连不上 B2** | 网络 / 代理 / DNS |
| **本地没有这个目录** | 存档位置写错了，或那个盘没挂载（这不算失败，会跳过） |
| **找不到 kopia** | Linux：选了 kopia 但没装，走发行版仓库 —— Arch 用 `sudo pacman -S archlinuxcn/kopia`（Debian/Ubuntu: `sudo apt install kopia`，Fedora: `sudo dnf install kopia`）。Windows 不会出现这条：发布包内置了 kopia.exe |
| **在 Windows 上调不了缩放** | 缩放归 Magpie，kotori 在那边只做启动与观察 |

## 9. 费用

B2 免费额度是 10 GB 存储，存档通常只有几 MB，正常用不会产生费用。两点留意：

- **B2 自己也会保留被覆盖文件的旧版本**（隐藏版本），这同样占空间。在意的话可以在 bucket 的 Lifecycle Settings 里设 `daysFromHidingToDeleting`（比如 30 天）。
  ⚠️ 只设这一项；`daysFromUploadingToHiding` 会连 kotori 的版本包一起清掉。
- kotori 自己的滑动窗口（**保留版本数**）默认关闭，开启后**只删云端的自家版本**，绝不碰本地存档。
