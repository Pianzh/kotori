# 云存档同步：从零开始

这份文档写给第一次用 Backblaze B2 的人。跟着做一遍，大约 10 分钟。

同步引擎是 **rclone**，加密**默认关闭**——也就是说 bucket 里的存档就是普通文件，
不装 kotori、不装 rclone、用 B2 网页就能下载回来。加密是可选的额外一层，见文末。

---

## 1. 在 B2 上建 bucket

登录 <https://secure.backblaze.com/b2_buckets.htm>，左侧选 **Buckets** → **Create a Bucket**。

| 字段 | 填什么 | 为什么 |
|------|--------|--------|
| Bucket Unique Name | 自己起，比如 `kotori-saves-你的名字` | 全局唯一，重名要换一个 |
| Files in Bucket are | **Private** | 存档不该公开可读 |
| Default Encryption | **Disable** | 加密由 kotori 可选地做；这里开着也没坏处，只是没必要 |
| Object Lock | **Disable** | 用不上 |

建好之后列表里会显示一个 `Endpoint: s3.us-west-004.backblazeb2.com` 之类的值。

> ⚠️ **这个 Endpoint 我们不需要。** 那是 B2 的 S3 兼容接口，而 kotori 用的是 B2 原生接口
> （rclone 会自己找到正确的地址）。如果你把它填进 kotori 的 API endpoint 框，程序会明确拒绝
> 并告诉你留空——这是刻意的，不然第一次同步会以一个看不懂的 404 结束。

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

⚠️ **applicationKey 只显示这一次**，关掉页面就再也看不到了（看不到就删掉这把 key 重建一把，
不影响已上传的存档）。把两个值复制出来。

> keyID 不是你的 B2 账号 ID。rclone 明确要求用 Application Key ID，填账号 ID 会得到 401。

## 3. 填进 kotori

```bash
cd <kotori 项目目录>
cargo run -- ui
```

在 GUI 左侧选 **设置**，滚到 **云存档同步**：

1. 打开 **启用云同步** 开关
2. **bucket** 填第 1 步建的那个 bucket 名字（不是 endpoint，也不是网址）
3. **prefix** 保持 `kotori`。它是 bucket 里归 kotori 独占的目录，bucket 里的其他东西我们一律不碰
4. **API endpoint 留空**
5. **保留版本数** 保持 `0`（= 永不删除云端快照）
6. 点 **保存设置**
7. 在 **B2 凭据** 里填 keyID 与 applicationKey，点 **保存凭据**（它们只进系统密钥环）
8. 点 **测试连接**

看到 `连接正常：kotori:<你的 bucket>/kotori` 就通了。

命令行同样可以看状态和测连接：

```bash
cargo run -- sync status   # 缺什么会直接写出来
cargo run -- sync test     # 只测凭据 + bucket + 写权限
```

## 4. 告诉 kotori 存档在哪

同步的对象是**你手动指定的存档位置**。在游戏详情页（游戏库 → 点某个游戏）里配置：

| 类型 | 什么时候用 | 例子 |
|------|-----------|------|
| `windows` | 存档在 AppData / 文档 / Saved Games 里（**绝大多数 galgame**） | `%APPDATA%\会社名\游戏名` |
| `relative` | 存档就在游戏目录里 | `savedata` |
| `absolute` | 只在这台机器有效，不参与跨平台映射 | `/home/你/saves/xxx` |

怎么找存档位置：游戏设置里一般能看到，或者去 `~/.wine/drive_c/users/<你的用户名>/AppData/Roaming/`
下面翻。每个位置还可以填排除规则（比如 `*.log`、`cache/`）。

> 用 `windows` 类型时**不要**写死 `C:\users\你的用户名\...`：wine 里的用户名和真实 Windows 上的
> 可能不一样，跨系统会认不出来。用 `%APPDATA%` 这类令牌，两边语义一致。

## 5. 日常怎么用

配好之后基本不用管：

- **启动游戏前**：自动把云端较新的文件取回来（用 `copy --update`，**绝不覆盖本地更新的存档**），
  最多等 30 秒，失败只提示、不挡你玩游戏
- **游戏退出后**：等 3 秒（让 wineserver 落盘）自动上传，被替换掉的旧文件存成一个快照
- 手动：设置页里有 **立即同步全部**，每个游戏后面也有 **同步 / 恢复**

命令行：

```bash
cargo run -- sync now                  # 全部同步一次
cargo run -- sync now <游戏id>          # 只同步一个
cargo run -- sync versions <游戏id>     # 看云端快照列表
cargo run -- sync restore <游戏id>                        # 恢复到最新
cargo run -- sync restore <游戏id> --version 20260911T101500Z-1a2b3c4d  # 回滚到某个快照
```

**恢复是安全的**：它用 `copy` 而不是 `sync`（坏备份删不掉你的本地存档），
而且在覆盖之前会先把当前状态传成一个新快照——所以误恢复也能再恢复回来。

**回滚某个快照**的语义是「回到那次上传之前的状态」：快照里存的是当时**被替换掉**的文件，
所以恢复时会先取回最新状态补齐其他文件，再叠加快照。

## 6. 存在哪、怎么自己拿回来

明文模式下（默认），bucket 里的结构是：

```
<bucket>/kotori/games/<游戏id>/current/<存档位置>/...     ← 最新
<bucket>/kotori/games/<游戏id>/versions/<时间戳>/...      ← 快照
```

登录 B2 网页就能直接下载这些文件——**不需要 kotori，也不需要 rclone**。

| 东西 | 存在哪 | 你怎么取回来 |
|------|--------|-------------|
| B2 keyID / applicationKey | 系统密钥环 | `secret-tool lookup service kotori account b2-key-id`（applicationKey 换 `b2-app-key`） |
| 同步密码（只有开加密才用） | 系统密钥环 | `secret-tool lookup service kotori account sync-password` |

`~/.config/kotori/config.toml` 里**没有**任何密钥：只有 bucket 名、prefix、保留版本数这些非敏感设置。

> 如果这台机器没有可用的系统密钥环，kotori 会把密码**只放在内存里**（重启后要重新输入），
> 而不是退回明文写文件。设置页会明确提示这一点。

## 7. 加密（可选）

默认关闭。开启后 rclone 会套一层 `crypt`，bucket 里的文件名和内容都是密文。

**开启前请注意**：

- 密码**由你自己设定**，我们不会替你生成（你从没见过的密码，等于把备份锁在别人手里）
- 开启会让 bucket 里**已有的明文存档读不出来**，所以只有在 bucket 是空的时候才开；
  否则请把 **prefix 换成一个新名字**，让两种数据分开
- 密码丢了就真的打不开了。它存在系统密钥环里，也能用上面那条 `secret-tool` 命令取回来

双系统（Linux + Windows 双启动）时：密钥环是每个系统各自一份，不会同步，
所以两边各输入一次同一个密码即可。

## 8. 出问题先看这里

```bash
cargo run -- sync status   # 远端、rclone、密钥环、每个游戏上次同步的结果
cargo run -- sync test     # 一次验证凭据 + bucket + 写权限
cargo run -- sync --help
```

常见错误与对策（kotori 会把 rclone 的原话附在后面）：

| 提示 | 原因 |
|------|------|
| **B2 不认这组凭据** | keyID 填成了账号 ID；或者 applicationKey 没复制全 |
| **这个 key 没有这个 bucket 的权限** | 建 key 时没勾这个 bucket，或 Type of Access 不是 Read and Write |
| **找不到这个 bucket** | bucket 名字拼错 |
| **这是 B2 的 S3 兼容接口地址** | API endpoint 填了 `s3.<region>.backblazeb2.com`，留空即可 |
| **连不上 B2** | 网络 / 代理 / DNS |
| **本地没有这个目录** | 存档位置写错了，或那个盘没挂载（这不算失败，会跳过） |

## 9. 费用

B2 免费额度是 10 GB 存储，存档通常只有几 MB，正常用不会产生费用。
两点留意：

- **B2 自己也会保留被覆盖文件的旧版本**（隐藏版本），这同样占空间。
  在意的话可以在 bucket 的 Lifecycle Settings 里设 `daysFromHidingToDeleting`（比如 30 天）。
  ⚠️ 只设这一项；`daysFromUploadingToHiding` 会连 kotori 的版本快照一起清掉。
- kotori 自己的滑动窗口（**保留版本数**）默认关闭，开启后**只删云端的自家快照**，绝不碰本地存档。
