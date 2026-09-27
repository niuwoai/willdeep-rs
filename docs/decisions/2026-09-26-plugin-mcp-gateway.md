# 插件 MCP 网关：按需激活插件的 MCP 服务（契约 v1）

> 本文是短剧工坊仓库 `willdeep-video-studio/docs/decisions/0001-plugin-mcp-gateway.md`
> 的副本（2026-09-26，含「没送到」重试规则的修订；2026-09-27 同步修订 1，对应短剧工坊
> 0.32.0-rc2）。三方（macOS 宿主、willdeep-rs、插件）以那一份为准；两边有出入时改那边、
> 再同步到这里。文末「willdeep-rs 实现说明」是本仓库自己的补充。

- 状态：已采纳（2026-09-26）；修订 1（2026-09-27）：插件连接文件按宿主分开，见下文「修订 1」
- 适用：WillDeep macOS（Xedit）≥ 1.405.0-rc1、willdeep-rs ≥ 0.83.0-rc1、短剧工坊 ≥ 0.30.0-rc1（修订 1：短剧工坊 ≥ 0.32.0-rc2）

## 背景

插件的 stdio MCP 进程由宿主拉起，而宿主只在「有人给它发第一个请求」时才拉起：
页面脚本调 `executeCommand`、模型调插件工具、设置页「刷新工具」。外部 MCP 客户端
（Claude Code、Codex……）没有办法触发这一步——它们只能去读插件自己写的
`plugin-data/<插件 ID>/mcp-http.json`，而这份文件只在进程活着时存在、端口和 token
每次重启都变。结果是：必须先在 WillDeep 里点开「短剧工坊」，外部客户端才连得上；
插件进程被宿主结束后连接又断。

## 决定

宿主自己开一个只绑 `127.0.0.1` 的 **插件 MCP 网关**，地址与 token 固定下来写进一份
发现文件。外部客户端只连网关；网关收到第一条需要插件处理的请求时，才经宿主平时用的
那条 MCP 客户端连接把插件拉起来（进程、反向请求能力、环境变量都与页面打开时完全
一致），再把请求交给插件处理。

不采用的方案：

- 宿主启动时预热所有插件：不是按需，白白常驻进程；插件崩了仍然没人拉起。
- 网关只经 stdio 中转：宿主对单个 stdio 请求有 `startup_timeout_sec`（短剧工坊 10 秒）
  的超时并串行执行，出图、审核一次一两分钟，中转会被超时结束进程，也会堵住页面请求。
  所以插件自己公布了 HTTP 入口时优先转发到那里，只有没公布时才退回 stdio 中转。

## 契约 v1

### 发现文件

| 宿主 | 路径 |
|---|---|
| macOS | `~/Library/Application Support/WillDeep/mcp-gateway.json` |
| willdeep-rs | `<WILLDEEP_HOME 或 ~/.willdeep>/mcp-gateway.json` |

权限 `0600`，内容：

```json
{
  "version": 1,
  "host": "willdeep-macos",
  "url": "http://127.0.0.1:47831",
  "token": "<64 位十六进制>",
  "servers": [
    {
      "pluginID": "willdeep-video-studio",
      "server": "video-studio",
      "url": "http://127.0.0.1:47831/plugins/willdeep-video-studio/video-studio/mcp"
    }
  ]
}
```

- `host`：`willdeep-macos` 或 `willdeep-rs`。
- 端口和 token 首次随机生成后持久保存，之后每次启动都先试原端口、沿用原 token，
  外部客户端的配置因此长期有效；原端口被占时换一个新端口并重写文件。
- `servers` 只列已启用插件里的 stdio / HTTP MCP 服务，插件启用、停用、安装、卸载后
  重写。
- 宿主退出时不删这份文件（token 仍有效，下次启动沿用）；连不上即表示宿主没在运行。

### 端点

`POST /plugins/<pluginID>/<server>/mcp`，Streamable HTTP，只回 `application/json`
（不开 SSE）。

- 鉴权：`Authorization: Bearer <token>`，不对回 401。比较用常量时间。
- 只接受 Host 为 `127.0.0.1` / `localhost` 的请求（防 DNS rebinding），带 `Origin`
  且不是本机来源的回 403。
- 未知或未启用的插件 / 服务回 404；`GET` / `DELETE` 回 405；请求体超过 8 MB 回 413；
  不是单条 JSON-RPC 对象（含批量数组）回 JSON-RPC `-32600`。
- `initialize`：网关自己应答，不转给插件——第三方客户端的能力声明不能覆盖宿主在 stdio
  那头宣告的反向请求能力（插件 0.29.0-rc3 之前会被覆盖）。返回客户端请求的
  `protocolVersion`（缺省 `2025-06-18`）、`capabilities: {"tools": {}}`、
  `serverInfo: {"name": "<pluginID>/<server>", "version": "<插件版本>"}`，并在响应头给
  `Mcp-Session-Id`。
- 通知（没有 `id` 的消息，含 `notifications/initialized`）：回 202、空响应体，不转发。
- `ping`：网关直接回 `{}`。
- 其他请求（`tools/list`、`tools/call`、`resources/*`……）：
  1. 确保插件已启动：经宿主的 MCP 客户端发一次 `tools/list`（插件没在运行就会按平常的
     方式被拉起）。这次结果顺手刷新聊天用的插件工具目录。
  2. 读插件数据目录下的 `mcp-http.json`（`{"url","token"}`，url 必须是
     `http://127.0.0.1:<端口>/…`）。存在就把原始请求体原样 POST 过去，带插件自己的
     Bearer token，超时 15 分钟，响应体原样回给客户端。
  3. 文件不存在（插件没有自己的 HTTP 入口）、或「没送到」且重读一次后仍失败：经宿主的
     stdio 客户端中转这条请求，结果包成 JSON-RPC 响应。
  4. 「没送到」只指插件还没开始执行的失败：连接被拒、插件入口回 401 / 404（旧 token、
     旧端口）。超时、连接中途断开等可能已经在执行的失败**不重发、不中转**，直接回
     JSON-RPC `-32603` 并附原因——出图、提交视频不是幂等的，重发会执行两遍。
- 插件数据目录：macOS 为 `~/Library/Application Support/WillDeep/plugin-data/<pluginID>/`；
  willdeep-rs 先找自己的插件数据目录（`<WILLDEEP_HOME 或 ~/.willdeep>/plugin-data/<pluginID>/`），
  找不到再找 macOS 路径（插件 ≤ 0.32.0-rc1 不论哪个宿主拉起都固定写 macOS 路径）。
  两个宿主拉起的插件进程各写哪一处见修订 1。

### 插件侧

- 插件按拉起它的宿主写连接文件（修订 1）；不需要知道网关存在。
- 外部客户端文档一律指向网关地址，不再让用户去读插件的 `mcp-http.json`。

## 结果

- 外部客户端配一次网关地址即可；宿主运行着，插件就按需拉起，不用先点开插件页。
- 插件进程被结束后，下一条请求会重新拉起它。
- 两个宿主行为一致，差别只在发现文件路径与 `host` 字段。

## 修订 1（2026-09-27，短剧工坊 0.32.0-rc2）：连接文件按宿主分开

### 起因

两个宿主拉起插件时都不传 `VIDEO_STUDIO_DATA_DIR`，插件进程共用 macOS 数据目录，各自把
随机端口和 token 写进同一份 `mcp-http.json`，后写的赢。2026-09-27 实测：willdeep-rs
（`willdeep --web`）拉起的插件覆盖了 WillDeep macOS 拉起的那份（文件里的端口经 `lsof`
对上 willdeep-rs 那个插件进程，macOS 的插件在另一个端口），macOS 网关转发的
`video.generate` 报「Video API key is not configured.」——Key 是 macOS 宿主经 `mcp.json`
注入的插件设置，只在它拉起的进程里；反向请求（出图、审核）也会发到另一个宿主。停掉
willdeep-rs 那边的插件后文件随之删掉，macOS 网关才退回 stdio 中转、回到自己的插件。

### 决定（插件侧；两个网关的现有实现不改即生效）

连接文件写在拉起本进程的宿主的网关**先读**的那一处。宿主身份看 stdio 那头 `initialize`
的 `clientInfo.name`：

| 拉起插件的宿主 | `clientInfo.name` | 连接文件 | `host` 字段 |
|---|---|---|---|
| WillDeep macOS | `WillDeep Desktop (some.im)`（以 `WillDeep Desktop` 开头；早期版本不带 clientInfo，同样算 macOS） | `<数据目录>/mcp-http.json`，默认 `~/Library/Application Support/WillDeep/plugin-data/<pluginID>/`，与修订前一致 | `willdeep-macos` |
| willdeep-rs | `willdeep` | `<WILLDEEP_HOME 或 ~/.willdeep>/plugin-data/<pluginID>/mcp-http.json`（`WILLDEEP_HOME` 由 willdeep-rs 注入） | `willdeep-rs` |
| 其他 MCP 客户端 | 其他 | `<数据目录>/mcp-http.<客户端名>.json`（小写、非字母数字折成 `-`），不占任何网关读的位置 | `other` |

- 只有连接文件分开；`dramas.json` 等存档仍在同一个数据目录，两个宿主看到同一批短剧。
- 端口在进程启动时就开好，连接文件等 stdio `initialize` 才写：网关第 1 步总是先经宿主的
  MCP 客户端确保插件已启动，读文件时 `initialize` 已经应答过。HTTP 客户端的
  `initialize` 不挪文件。
- 写法：同目录临时文件（建时即 `0600`）写完再 `rename`，网关读不到半份。
- 不按 `VIDEO_STUDIO_HOST_MODE` 判断：那是媒体根的强制开关，macOS 宿主下强制 Web 媒体
  不该把 macOS 网关要读的文件挪走。
- 退出时仍只删 token 还是自己的那份；同一进程被重新 `initialize` 成另一个宿主时，先删旧
  位置上自己的那份再写新位置。

内容在 `url`、`token` 之外新增三个字段（两个网关都只读 `url` / `token`，多出的字段被
忽略，向后兼容）：

```json
{
  "url": "http://127.0.0.1:63105/mcp",
  "token": "<64 位十六进制>",
  "host": "willdeep-rs",
  "pid": 41235,
  "parentPID": 41102
}
```

- `pid`：写这份文件的插件进程。
- `parentPID`：拉起它的进程。两个宿主都直接 spawn `mcp.json` 里的 `/usr/bin/ruby`，所以
  这就是宿主进程，也是网关所在的进程。

### 兼容

- 新插件 + 现有网关：macOS 网关读的那一处只有 macOS 拉起的进程会写；willdeep-rs 网关先读
  自己的目录，找到的就是自己拉起的进程。
- 旧插件（≤ 0.32.0-rc1）不论哪个宿主拉起都写 macOS 路径。两个宿主装的插件版本不同（例如
  willdeep-rs 装的比 macOS 内置的旧）时，旧进程仍会覆盖 macOS 那份——插件这边挡不住，
  要靠下面的网关校验。

### 宿主侧建议（可选加固，尚未实现；Xedit 与 willdeep-rs 的 ADR 副本同步本节）

> 本仓库注：willdeep-rs 自 0.84.0-rc2 起已实现这两条，见文末「willdeep-rs 实现说明」。

1. 文件带 `parentPID` 且不等于网关所在进程的 pid：当作文件不存在（走 stdio 中转）。不带
   `parentPID` 的旧文件照旧接受。这一条同时挡住跨宿主串线和混装的旧插件。
2. willdeep-rs 退到 macOS 路径时，只接受不带 `host` 的旧文件；`host` 为 `willdeep-macos`
   的属于 macOS 拉起的进程，转发过去就是反方向的串线。自己的插件没写出文件（HTTP 入口
   绑端口失败、写文件失败）时会走到这一步。

## willdeep-rs 实现说明

- **网关在哪个进程**：`willdeep web` 进程（`crates/willdeep-cli/src/plugin_gateway.rs`），
  因为插件宿主 `PluginHost` 与页面正在用的插件进程都在这里。它是一个**单独的**
  `127.0.0.1` 监听，不是 Web 服务上的一条路由：Web 服务可以绑在非回环地址上给
  nginx / VPN 用，而网关只该给本机进程，端口还要跨重启保持不变。没开 `willdeep web`
  时没有网关（「连不上即表示宿主没在运行」）。
- **端口**：先试发现文件里的端口，再试 6 个 41000–48999 的随机端口（避开系统临时端口段），
  最后交给系统挑；与 macOS 宿主同一套。
- **插件数据目录**：`<WILLDEEP_HOME>/plugin-data/<pluginID>/`，再
  `$HOME/Library/Application Support/WillDeep/plugin-data/<pluginID>/`（Linux 上也找，
  因为短剧工坊不设 `VIDEO_STUDIO_DATA_DIR` 时把这条路径写死）。
- **只转发给自己拉起的插件（修订 1 的两条加固，0.84.0-rc2 起）**：
  `gateway::read_plugin_endpoint` 逐处校验，不合格的当作不存在、接着找下一处，都没有就
  走 stdio 中转。
  - 带 `parentPID` 的必须等于 `willdeep web` 进程自己的 pid。前提是插件宿主直接 spawn
    `mcp.json` 的 `command`（`tokio::process::Command`，不经 shell）。`/usr/bin/ruby`、
    xcrun 的 `/usr/bin/python3` 这类 shim 是 exec，父进程号不变。端到端用例在 unix 上
    核对假插件看到的父进程号。Windows 上 `command` 若是 `.cmd` / `.bat`，会经 `cmd.exe`
    起，父进程号对不上，只会退到 stdio 中转。
  - macOS 那一处只收不带 `host` 的旧文件。契约原文点名的是 `host` 为 `willdeep-macos`
    的；按修订 1，新插件只有 macOS 拉起时才写那里，所以带任何 `host` 都不收。
  - 顺带挡住本机另一个 willdeep 进程：没开 Web 时聊天进程（daemon / CLI）自己拉起的
    插件也写 `<WILLDEEP_HOME>/plugin-data/<pluginID>/mcp-http.json`，`parentPID` 是那个
    进程，网关不再转发给它。两个进程的插件都活着时这份文件只能属于其中一个，Web 这边
    可能整段时间都走 stdio 中转。
  - 挡不住的：不带 `parentPID` 的旧文件照旧接受。另一个宿主跑的是 ≤ 0.32.0-rc1 的插件、
    本宿主的插件又没写出自己那份时，退到 macOS 那一处仍可能读到那边的进程。
- **stdio 中转的错误**：插件回的 JSON-RPC 错误对象原样带回（macOS 宿主包成 -32603）；
  宿主侧失败（进程起不来、超时）回 -32603。
- **能运行的服务**：与 macOS 宿主 `AgentMCPServerDirectory.loadPluginServers` 同一套过滤——
  清单 `dependencies.mcpServers` 声明过（没有 WillDeep 清单的包取 `mcp.json` 全部），
  且声明了 `process.execute`。本宿主只支持 stdio 插件服务，所以 `servers` 里不会出现 HTTP 服务。
- **重写发现文件的时机**：Web 进程启动、在 Web 里启用 / 停用 / 卸载插件。CLI
  `willdeep plugin install|enable|disable` 在另一个进程，Web 进程的插件宿主要重启才看得见
  新状态（这是 Web 插件宿主既有的限制），发现文件随之在下次启动时重写。
