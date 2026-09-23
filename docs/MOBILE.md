# 手机中继

用 WillDeep Mobile 在手机上跟进这台电脑上的 Runtime：看所有会话在干什么、批掉卡住的审批、补一句指令。人不在电脑前，任务也不必停下来等你。

中继由 **Runtime Daemon** 托管，不属于某个终端：打开之后，关掉终端、`daemon upgrade` 都不会断，`/mobile off` 才会关。设计与取舍见 [决策单](decisions/2026-09-23-daemon-mobile-relay.md)。

## 使用

在 TUI 中：

```text
/mobile          # 打开中继（必要时先拉起 Runtime）并显示配对二维码
/mobile show     # 再次显示二维码
/mobile hide     # 只隐藏二维码（等价于按 Esc），中继照常在线
/mobile off      # 关闭中继
```

不开 TUI 也行：

```bash
willdeep daemon mobile enable    # 打开并在终端打印二维码
willdeep daemon mobile status    # enabled / connected / phone_active
willdeep daemon mobile disable   # 关闭
```

侧栏「移动中继」显示 Runtime 报的状态：`关闭`、`已连接`、`重连中`，开着时还有一行「手机：在线 / 未连接」（最近 60 秒内收到过手机请求算在线）。Runtime 没在跑时显示「Runtime 未连接」。

## 手机上能看到什么

**整个 Runtime**，不只是开 `/mobile` 的那一条会话：

- 所有未归档会话（最近更新的 50 条），标题、所在工作区、是否正在跑；
- 选中会话的最近 40 条消息（与 Web 端会话详情同一份投影），以及进行中轮次的实时回复；
- **所有**待处理的审批与提问，带归属会话——哪条会话卡在哪，一眼看得到；
- 已登记的工作区列表。

不推 token 级流式：每条助手回复整条到达。

## 手机上能做什么

只有四件事：

| 手机上的操作 | 在 Runtime 里 |
|---|---|
| 发消息（可带图片） | 给目标会话提交一轮（`turn.submit`）；同一会话的轮次严格串行，正在跑就排队 |
| 新建会话 | 在**已登记**的工作区里建一条（`session.create`） |
| 停止 | 停掉会话当前的轮次（`turn.stop`） |
| 批准 / 拒绝、回答提问 | `approval.resolve`（只有「这一次允许」和「拒绝」）/ `question.answer` |

消息发到哪条会话：手机指定了会话就是它；手机按工作区发（没指定会话）时，选中会话恰好在这个工作区且空闲就复用，否则在该工作区新建一条；都没有就用选中会话，再没有就在活跃工作区新建。

图片会限边到 1568 像素、统一转成 JPEG 再交给模型。

**手机上做不到的**：删除、归档、改名、分叉、回退会话；登记、移除、切换工作区；改审批档位或模型（手机端选择器里的这些选项会被忽略）；派生、停止、重试子 Agent；「总是允许」——那是长期规则，只能在桌面上定。

## 与终端之间

- 手机发起的轮次，审批会弹回手机；你回到电脑前，打开同一条会话的 TUI 也会弹，谁先答算谁的。
- 审批在手机上先答掉了，TUI 里对应的对话框会在一秒内自动收起，提示「这条审批已在其他端处理」。
- 手机消息不进 TUI 的键盘队列（侧栏「运行状态」里的「待发队列」），直接进 Runtime 的轮次队列。
- `/local` 进程内轮次不经过 Runtime，手机上看不到它的实时输出；轮次结束落盘后，历史里能看到。

## 连接

| 项目 | 值 |
|---|---|
| 中继地址 | `wss://j.niuwoai.com/ws/broadcast/<room>` |
| 协议版本 | `mobile-gateway.v1`（与 macOS 桌面端同一套信封与事件名） |
| 房间 | `wd-<32 位十六进制>` |
| 认证 | `Authorization: Bearer <token>` |
| 断线重连 | 2 秒间隔自动重试 |

**Runtime 不监听任何新端口**，只主动外连中继。

中继是广播房间：Runtime 只响应手机命令，回声和其它桌面端的回复一律不回，避免互相回复的死循环。手机不在场（60 秒没有请求）时不往中继推事件，回来时先补一份完整快照。

## 凭据

WillDeep CLI 使用**独立于 macOS 桌面端**的 room 与 token，保存在 `$WILLDEEP_HOME/mobile-relay.toml`（默认 `~/.willdeep/mobile-relay.toml`）：

```toml
relay_base_url = "https://j.niuwoai.com"
room = "wd-<32 位十六进制>"
token = "<32 位十六进制随机值，即 128 位熵>"
enabled = true   # 中继开关；Runtime 启动时据此自动重连
```

Unix 权限为 `0600`。首次生成时先写临时文件并设好权限再 rename，不存在权限窗口。已存在的文件在使用前会校验权限，不合规则拒绝打开中继并提示 `chmod 600`。开关中继只改 `enabled`，room 与 token 不变，已配对的手机不用重新扫码。

旧版凭据（`willdeep-cli-<uuid>` room + 64 位 token）会在下次加载时自动重新生成为紧凑格式并覆盖原文件——手机重新扫一次码即可。0.81 及更早写出的文件没有 `enabled`，按关闭处理。

## 配对二维码

二维码里装的是 `mobile-gateway.v1` 的**紧凑配对 URL**，不是完整配对 JSON：

```text
https://j.niuwoai.com/pair?r=<room>&t=<token>&d=<桌面名>
```

| 参数 | 含义 | 缺省行为 |
|---|---|---|
| `r` | relay room | 必填 |
| `t` | relay token | 必填 |
| `d` | 桌面名（≤16 字符） | 手机端显示为 `WillDeep Mac` |
| `u` | relay base url | 缺省即 `https://j.niuwoai.com`，自建中继才下发 |
| `v` | 协议版本 | 缺省即 `mobile-gateway.v1` |

手机端把这几个参数补全成完整配对 JSON：`base_url` 由 `u` 推出，`pairing_token` 等于 `t`，`expires_at` 取远期常量。所以 `base_url`/`pairing_token`/`expires_at` 不进二维码——它们要么是中继字段的副本，要么是常量，每重复一份都要多烧几十个模块。

尺寸：二维码在终端里每个模块占一个字符格，只由载荷字节数和纠错等级决定。纠错取 L 级（屏幕显示不存在印刷污损，7% 冗余足够）。ASCII 主机名下载荷 114 字节、二维码 41×41 模块，加静区即 **49 列 × 25 行**（Dense1x2 渲染，一个字符格装两行模块）；桌面名顶满 16 字节且全部需要百分号转义（如中文主机名）时是上界 45×45 模块 / **53 列 × 27 行**。`mobile.rs` 的 `pairing_qr_fits_the_terminal_popup` 用最坏情况的桌面名把上界钉死为 `MAX_QR_WIDTH` × `MAX_QR_HEIGHT`，并断言当前环境的真实二维码不超过它——任何加长配对内容的改动都会先撞到这条测试。

配对 URL 由本机 Runtime 控制面的 `mobile.enable` 返回；这个操作是设值语义，不进幂等缓存，所以 token 不会落进 `idempotency.json`。该文件不会写入仓库。

## 安全提醒

**配对二维码中明文携带 relay token，而中继现在能看整个 Runtime、给任何会话发消息。** 扫码即等于交出这些能力：

- 只对自己的手机扫码；
- 不要把二维码截图分享或上传；
- 二维码里的过期时间实际上是一个远期常量，等同于**永不过期**。需要作废时先 `/mobile off`，再删除 `mobile-relay.toml`，下次打开会生成新的 room 与 token，旧手机随之失效。

**中继服务端看得到往来的明文**（会话标题、消息、审批描述），与 macOS 桌面端同一边界。上中继前，消息与审批描述中疑似凭据的词会被打码，失败原因只下发闭集合分类，本机路径不会出现在错误里；端到端加密另立项。

## 相关文档

- [决策单：手机中继从 TUI 挪进 Runtime Daemon](decisions/2026-09-23-daemon-mobile-relay.md)
- [Runtime Daemon 与工作区](RUNTIME_DAEMON.md)
- [TUI 使用指南](TUI_GUIDE.md)
- [认证与凭据](AUTHENTICATION.md)
