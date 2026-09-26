# 插件系统（Web 宿主）

> 状态：v1 已实现，首发 0.50.0-rc1；宿主能力补齐到桥 2.5.0 于 0.73.0-rc1，桥 2.6.0（图片附件）于 0.74.0-rc1。
> Schema：`.willdeep-plugin/plugin.json` schemaVersion 1，与 macOS 版（Xedit）**同一份契约**。
> 上游设计：Xedit `docs/WILLDEEP_PLUGIN_SYSTEM_DESIGN.md`；宿主能力：`docs/PLUGIN_HOST_CAPABILITIES_DESIGN.md`。
> 两端联动全景见 [XEDIT_INTEROP_STATUS.md](XEDIT_INTEROP_STATUS.md)。

## 一句话

同一个插件包，在 macOS 原生宿主和这里的 Web 宿主都能跑，**插件不用为 Web 改一行**。

之所以能这样，是因为插件页面依赖的是宿主注入的 `window.willdeep.*` 和
`willdeep:context-changed` 事件，而不是 WKWebView 的 API。换宿主换的是传输层
（那边是 `webkit.messageHandlers`，这边是 `postMessage` 到父窗口），契约没变。

## 共享什么，不共享什么

| | 共享 | 理由 |
|---|---|---|
| 包内容 `~/.willdeep/plugins/<id>/<version>/` | ✅ | Xedit 装过的插件这里直接看得见，反之亦然 |
| 启用状态 | ❌ 各存各的 | — |
| 权限审批 | ❌ 各存各的 | 两个宿主的沙箱边界不是一回事：这边是 opaque-origin iframe + CSP，那边是每插件独立持久化仓的 WKWebView + 自定义协议。跨宿主复用审批，等于替另一个宿主替用户点了头 |

同一插件装了多个版本时，加载版本号最大的那个，旧版本留着供回滚。比较规则按
SemVer 优先级：`0.2.0-rc1 < 0.2.0-rc2 < 0.2.0-rc10 < 0.2.0`，rc 号按数值比，
`+` 后的构建元数据不参与。这与 Xedit（`AppVersion`）选版本的结果一致。
注意 `minimumWillDeepVersion` 的宿主版本校验是另一回事，**有意**忽略 rc 后缀。

rs 侧的运行状态在 `~/.willdeep/plugin-registry.web.json`（0600，group/other 位
一旦松掉就拒绝整个存储）。文件名里的 `web` 是提醒：这不是 Xedit 那份。

## 装一个插件

```bash
# 从目录安装（不执行包里的任何东西：没有 postinstall、没有 npm install、没有构建）
willdeep plugin install ~/some/plugin-package

# 或者把 macOS 版自带的第一方插件导入共享目录
willdeep plugin import                    # 自动找 WillDeep.app / Xedit.app / ~/Sites/Xedit/PluginExamples
willdeep plugin import <目录> --enable    # 指定来源，导入后直接批准并启用

willdeep plugin list
willdeep plugin info <id>
willdeep plugin approve <id>              # 打印权限、来源、digest、要起的进程，然后记录审批
willdeep plugin enable <id>
willdeep plugin disable <id>
willdeep plugin remove <id> --yes
```

**批准与启用是两步**，不是啰嗦：批准是对**内容**的判断（这个 digest、这些权限、
这个来源），启用是对**此刻要不要跑**的判断。两者失效的条件也不同——插件更新后
审批自动失效，启用状态不会。

审批绑定四样东西：版本、内容 digest、来源、权限集合。任何一样变了都要重新确认。
权限差异排在 digest 之前报，因为改权限必然改 digest，先说"内容变了"只会淹掉
"它现在还想要网络访问"这句真正要紧的话。

## 三种页面在 Web 下怎么跑

| runtime | 文档来自 | iframe 地址 |
|---|---|---|
| `localWeb` | 包内 `entryPath` | `/plugin-host/<plugin>/<entryPath>`，同目录下的相对资源照常加载 |
| `mcpApp` | 插件 MCP 服务的 `ui://` 资源 | `/plugin-page/<plugin>/<pageId>` |
| `declarative` | 包内 JSON Schema | 由宿主原生渲染，不进 iframe |

侧栏三种模式（`sessionList` / `declarative` / `none`）与 macOS 一致。声明式文档
无论来自包内还是 MCP 动态 Resource，都走同一套限制：1 MiB、1000 组件、20 层嵌套、
ID 唯一、命令引用必须存在、progress 必须落在 0…1。

动态 Resource 读失败时回落到包内 Schema，并在侧栏顶部明说数据是旧的——一个装作
正常的过期面板比一个报错的面板更容易误导人。

## 菜单贡献点

六个位置全部可用，与 macOS 宿主同一份白名单（定义在 `plugin/manifest.rs`）：

| 位置 | Web 端入口 |
|---|---|
| `commandPalette` | ⌘K / Ctrl+K 命令面板，按插件分组显示来源 |
| `chat.selection` | 聊天正文选中后浮出的气泡。宿主固定传两个参数：`text`（选中原文）与 `source="chat.selection"` |
| `session.context` | 会话列表行右键。没有插件贡献这个位置时不劫持右键，浏览器自带菜单照常可用 |
| `composer.more` | Composer 左下角的「更多」按钮 |
| `plugin.sidebar.row.context` | 声明式侧栏行右键；行自己声明的 `contextCommands` 优先，为空时退回该插件在这个位置贡献的全部命令 |
| `plugin.page.toolbar` | 插件页面顶部工具栏 |

目的地是插件的主入口，菜单是它的**顺手入口**——收藏夹和待办真正的用法是
「聊天里选中一句就能记下」，而不是先切到那个目的地再手打一遍。

## 安全不变量（Web 宿主特有的那几条）

1. **页面是 opaque origin。** iframe 只给 `allow-scripts`，不给 `allow-same-origin`，
   所以它拿不到父页面的 DOM、cookie 与 localStorage，也不给 popups。
2. **CSP 里不能用 `'self'`。** opaque origin 下 `'self'` 不匹配任何东西，用了页面
   连自己的脚本都加载不了。改用请求 `Host` 推出来的显式 origin。
3. **`connect-src 'none'`。** 页面够不着任何网络端点，包括宿主自己的 API——
   要宿主做事只能走 bridge，而 bridge 只认清单里声明过的命令。
4. **只对 `Origin: null` 回 CORS 头。** Vite 产物默认写 `<script type="module" crossorigin>`，
   带 crossorigin 的请求走 CORS，opaque origin 报的正是 `Origin: null`；不给
   `Access-Control-Allow-Origin` 就整个被拒、页面白屏，而且 CSP 面板上什么都看不到。
   放行只限 `null`，不写 `*`：普通网页有真实 origin，读不到本机插件包的内容。
5. **每个插件一套 MCP 连接。** 隔离靠实例边界，不是名字前缀——两个插件的服务重名
   也串不到一起。MCP App 页面报上来的 server 名不作数，只用清单里声明的那一个。
6. **停用即断连。** 被停用的插件不再有活着的子进程，静态资源也一并停供。
7. **secret 不回显。** 设置界面只说"设过没有"。一个能被 GET 回来的密钥等于没存过。
8. **`mcp.json` 里的明文凭据拒载。** 敏感环境变量只能是 `${setting:<id>}` 或 Keychain 引用。
9. **路径三道关。** 词法上拒 `..` 与绝对路径 → 真实符号链接解析 → 包根前缀校验。
   只做最后一步不够：中间某一层是符号链接时，词法拼出来的路径看着很乖。

## Bridge 契约

页面侧（宿主注入，见 `crates/willdeep-cli/src/plugin_bridge.js`）：

桥版本 **2.6.0**，与 macOS 宿主 `AgentPluginPageBridgeVersion` 对齐。页面按
`window.willdeep.capabilities` 降级，而不是猜宿主有什么：

```js
window.willdeep.version                            // "2.6.0"
window.willdeep.capabilities                       // 字符串数组，见下表

window.willdeep.getContext()                       // 目的地上下文
window.willdeep.selectItem(itemID)
window.willdeep.refresh()
window.willdeep.executeCommand(commandID, args)    // → Promise<JSON 字符串>
window.willdeep.openConversation(sessionID)        // 需要 conversation.read

window.willdeep.ai.providers()                     // providers.read 或 ai.chat
window.willdeep.ai.complete(request)               // ai.chat（request.skills 另需 skills.read）
window.willdeep.ai.cancel(streamID)                // 按 complete 里传的 streamID 停
window.willdeep.ai.generateImage(request)          // ai.image
window.willdeep.skills.list()                      // skills.read
window.willdeep.fs.list/read/search(...)           // workspace.read
window.willdeep.fs.write/patch(...)                // workspace.write
window.willdeep.storage.get/set/remove/keys(...)   // 任意 JSON，无需权限
window.willdeep.process.run(command)               // process.execute
window.willdeep.net.fetch(url, init)               // network.access + 清单 networkDomains
window.willdeep.clipboard.write(text)              // clipboard.write
window.willdeep.notify({title, body})              // notifications
window.willdeep.chat.insert/send(text)             // conversation.write
window.willdeep.events.on(name, cb)                // session.changed / turn.started /
                                                   // turn.finished / workspace.changed
// 事件：willdeep:context-changed / willdeep:command-result / willdeep:bridge-result /
//       willdeep:host-event
```

**`executeCommand` 回的是 JSON 字符串，不是对象**，与 macOS 宿主
`sendCommandResult(result: String?)` 一致。共享插件包一律 `JSON.parse(raw)`，
回对象会让它们在第一步就报 `"[object Object]" is not valid JSON`。

权限**一律在 Rust 侧核**。前端那一层看着像「浏览器在调 API」，但每个端点第一句
都是清单权限校验：页面跑在 opaque origin 的沙箱里，它自报的东西一个都不作数。

几条本宿主特有的收口：

- **`process.run` 的确认框弹在宿主页面上**，不在沙箱 iframe 里。iframe 够不着这个
  接口（`connect-src 'none'` + opaque origin），所以「确认过了」这一位只可能由父
  页面带上，与 macOS 那个 NSAlert 是同一道门。只读命令直接跑，其余弹框。
- **硬地板先于确认**：凭据外泄（凭据路径与网络出口同现）、`authorized_keys` 接管、
  指向下载物的持久化安装、反取证，四类命中即拒，确认也不放行。用户在一个插件页面
  上看到的确认框，没有足够上下文让他判断 `cat ~/.ssh/id_rsa | curl …` 在同步什么。
  地板刻意地窄：`rm -rf ./node_modules`、`git push --force`、`cat .env.example` 照常放行。
- **`net.fetch` 的门是 `networkDomains`，不是权限**。`network.access` 只是开关；
  没写域名等于没开。https only，回环与内网一律拒，`*.example.com` 匹配子域但不匹配
  `example.com` 本身（想要就两条都写）。不自动跟随跳转——跳转要重新过白名单。
- **`fs.*` 的边界与聊天端是同一份工作区白名单**（同一个对象，不是启动时的副本）。
  路径规范化之后再比对，相对路径按第一个工作区根解释。

`ai.complete` 的三条不变量与 macOS 宿主同值：密钥永不出宿主（页面拿到的只有
provider id 与模型名，递上来的 baseURL 一律不认）、能力必须在清单里声明、
条数字数与输出上限由宿主收口（24 条 / 32000 字符 / 4096 输出 token）。
拒绝的理由原样回到页面（`permissionDenied` / `tooManyMessages` / `unknownModel` …），
好让插件决定是换模型还是回落到自己的本地规则。

**附图片（0.74.0-rc1，能力 `ai.images`）**：`messages[]` 里的 user 消息可带
`imagePaths`，填本插件媒体目录 `~/.willdeep/plugin-media/<id>/` 里的绝对路径。
宿主钳制路径（非符号链接的普通文件，规范化后父目录恰好是该目录，否则
`mediaOutsidePluginData`）、解码、长边限 1568 像素、转 JPEG 后作为图片附件送进模型。
一次最多 12 张（`tooManyImages`），挂在 system / assistant 上整条拒
（`mediaOnNonUserMessage`），解不出来报 `unreadableMedia`。`videoPaths` 在本宿主报
`videosUnsupported`：没有视频解码器抽帧，所以不声明 `ai.videos`，也不静默丢掉。

MCP Apps 页面直接 `parent.postMessage` 标准 JSON-RPC：`ui/initialize` →
`ui/notifications/initialized` → `tools/call` / `resources/read`。宿主在 initialized
之前对后两者回 `-32002`。

**结构化存储与 localStorage 垫片是两套**，存在同一个文件里但键空间分开
（结构化的那套带一个控制字符前缀）：`window.willdeep.storage.*` 存任意 JSON、
异步、跨刷新；垫片只存字符串，给那些本来就在用 `localStorage` 的插件。不分开的话，
一个插件同时用两套 API 就会互相覆盖，而且垫片会把 JSON 当字符串吐回去。

**localStorage 垫片**：opaque origin 里 `window.localStorage` 直接抛
SecurityError，而插件在原生宿主里本来是有存储可用的（经典游戏厅的最高分就是
一例）。宿主注入一个垫片：读走随页面下发的快照，写回 `~/.willdeep/plugin-web-storage/<id>.json`，
每插件隔离，上限 256 KiB。这不是给插件加新能力，是补回它在另一个宿主本来就有的那份。

## 认不出的东西一律降级，不拒装

三张词汇表（权限、host action、菜单挂载点）两端各自实现校验，一侧先支持的
一项原本会让另一侧把**整个包**判非法。2026-09-07 实测：这边因此装不上 Xedit
自带的三个插件——待办的 `conversation.write`、短剧工坊的 `ai.image`、历史回溯的
`session.open`。用户看到的是「装不上」，真相只是这个宿主还没实现其中一项。

现在补齐了那几项，并把规矩改成：`schemaVersion` 不变的前提下，认不出的词汇
与字段记下来并降级。

| 认不出的东西 | 处理 |
|---|---|
| 权限 | 照收照显示，授不出任何能力（宿主的每道门问的都是已知常量） |
| host action | 命令保留，菜单引用因此不悬空；执行时回 `UnsupportedHandler` |
| 菜单挂载点 | 这一条菜单不显示，插件其余部分照常 |
| 清单字段 | 忽略并记录 |

仍然拒装的只有结构性错误：`schemaVersion` 不认识、引用悬空、重复 ID、页面
缺必填字段、`resourceURI` 不是 `ui://`、`networkDomains` 写法不合共享 schema
的 pattern。

最后一条是刻意不降级的：那份名单是 `net.fetch` 唯一的门，不是可选展示项。
`https://example.com`、`example.com:443` 这类写法装得上却永远匹配不中任何主机，
插件作者只会拿到一个解释不了的 `hostNotDeclared`。

记录下来的条目由 `plugin install` / `info` / `approve` 打成一行
`unsupported here: …`，Web 端命令列表把它们的 handler 标成 `unsupported`。
落点：`plugin/manifest.rs` 的 `UnsupportedItems`。

## 能力探测

桥注入 `window.willdeep.capabilities` 与 `window.willdeep.version`。0.74.0-rc1 起
这边报的是桥 **2.6.0** 的 23 项（见上面的 Bridge 契约），与 macOS 宿主的差集是
`ai.reasoning`（流式思考增量，本宿主的 `ai.complete` 一次性返回）与 `ai.videos`
（视频附件要抽帧，本宿主没有解码器），这两项都不报。
插件先问再用：

```js
if ((window.willdeep.capabilities || []).includes('fs.write')) { … }
```

判据是 capabilities，不是 `version`：后者只说各自这套桥的迭代，两端号段
互不比较大小。

## 远程选文件

Web 界面在浏览器里，服务可能跑在另一台机器上，所以插件 MCP 服务里那种
「`osascript` 弹原生框」的选文件路子在这里不成立——框会弹在没人看的屏幕上，
然后超时。这类命令由宿主接管：浏览器弹文件框 → 上传到每插件隔离的
`~/.willdeep/plugin-media/<id>/` → 把落地的**服务端绝对路径**当作选择结果交回插件，
外面照样包一层 MCP 的 `content[0].text`。插件一行不用改。

要接管哪些工具是一张可声明的表（`plugin_web.rs` 的 `FILE_PICKER_TOOLS`，
三元组 `(插件 ID, MCP 服务, 工具名)`），新增插件只加一行。宿主侧会校验回来的路径
确实落在这个插件的媒体目录里——页面自报一个 `/etc/passwd` 就能把任意文件喂给
后续工具，这道门关在服务端。

## 插件的 MCP 服务：反向请求、聊天工具、网关

插件自带的 stdio MCP 服务由宿主按需拉起（`PluginHost::mcp`）。页面、聊天、外部
MCP 客户端三条路共用同一套连接规则：`${pluginRoot}` / `${setting:<id>}` 展开、
`WILLDEEP_HOME` 注入（插件自己声明了就不覆盖）、同一插件同时只建一次连接、进程
退出后下一次使用重新拉起。

### 反向请求（插件 → 宿主）

插件在处理请求期间（或空闲时）往 stdout 写一条带 `id` 的 JSON-RPC 请求，宿主处理完
把响应写回 stdin。与 macOS 宿主 `AgentMCPHostRequests` 同一份约定：

- `initialize` 的 `capabilities.extensions["io.willdeep/host-requests"].methods` 只列
  **真的实现了**的方法。本宿主宣告 `willdeep/images/generate`（需 `ai.image`，形状同
  `ai.generateImage`，图落在 `<WILLDEEP_HOME>/plugin-media/<id>/`）与
  `willdeep/ai/complete`（需 `ai.chat`，形状同 `ai.complete`，不流式）。
  `willdeep/audio/synthesize`（宿主代管 TTS）两个宿主都没实现，**不宣告**。
- 错误码：-32601 方法不存在 / 没宣告、-32602 参数不对、-32000 处理失败（含缺权限、
  没凭据）、-32001 处理超过 600 秒。
- 权限读插件清单，不读请求；用户在 `config.toml` 手配的 MCP 服务不宣告扩展，
  反向请求一律回 -32601。
- stdout 由常驻读任务独占：插件在宿主没有在途请求时发来的反向请求（短剧工坊的本机
  HTTP 入口在工作线程里出图就是这样）也会被接住。
- 超时衡量的是「插件多久没动静」：宿主处理反向请求期间不计时，处理完从头计。

`clientInfo.name` 保持 `willdeep`：短剧工坊按它认出 Web 宿主、切换媒体地址。

### 聊天里的插件工具

已启用插件的 MCP 工具进聊天的 `list_mcp_tools` / `call_mcp_tool`（名字
`mcp__<服务>__<工具>`，与配置里的 MCP 工具同一套审批与只读模式拦截）。

- 定义来自持久化目录 `<WILLDEEP_HOME>/plugin-mcp-tools.json`，建工具表**不拉进程**。
  目录在这些时机刷新：宿主连上插件（页面、命令）、网关确保插件在跑、模型调
  `list_mcp_tools` 时给还没条目的服务补一次 `tools/list`、Web 的
  `POST /api/plugins/<id>/mcp/refresh-tools`。插件停用或卸载时它的条目作废。
- 只暴露已启用、声明了 `process.execute`、且在 `dependencies.mcpServers` 里的服务；
  启用状态按注册表文件现读，Web 里一停用，聊天这边立刻看不到。
- 调用**优先经插件 MCP 网关**：聊天 harness 在 daemon / CLI 进程，网关在 `willdeep web`
  进程，走网关就和页面共用一个插件进程，不会有两个进程抢写同一份数据文件。网关不在
  （没开 Web）时才用本进程自己的插件宿主拉起插件。
- Runtime 任务显式限定了 MCP 服务白名单时不带插件工具。

### 插件 MCP 网关

契约见 [decisions/2026-09-26-plugin-mcp-gateway.md](decisions/2026-09-26-plugin-mcp-gateway.md)。
`willdeep web` 启动时在 `127.0.0.1` 上另开一个监听，发现文件
`<WILLDEEP_HOME>/mcp-gateway.json`（0600）给出地址、token 与每个已启用插件服务的端点。
外部客户端连 `POST <url>/plugins/<pluginID>/<server>/mcp`，带
`Authorization: Bearer <token>`；网关按需拉起插件，插件公布了 `mcp-http.json` 就原样
转发（15 分钟超时），没有就经 stdio 中转。例如给 Claude Code 配：

```bash
claude mcp add --transport http video-studio \
  "$(jq -r '.servers[0].url' ~/.willdeep/mcp-gateway.json)" \
  --header "Authorization: Bearer $(jq -r .token ~/.willdeep/mcp-gateway.json)"
```

### 页面主题变量

`compose_page` 在每个页面 `<head>` 最前面注入与 macOS 宿主同名的变量
`--willdeep-bg / -fg / -secondary / -accent / -body-font-size` 与 `color-scheme`，
首帧按 `prefers-color-scheme` 取 macOS 的同一组色值，不闪白；父页面随后推
`{type:'theme', theme:{colorScheme, variables}}`，桥把变量写到根元素内联样式上
（只收 `--willdeep-` 开头的字符串值）。插件自己写了同名规则就是插件说了算。

## 与 macOS 宿主的已知差异

| 项 | macOS | Web |
|---|---|---|
| `ai.reasoning` | 有，流式思考增量 | **没有**。本宿主的 `ai.complete` 一次性返回，所以这一项不出现在 `capabilities` 里——声明一个自己不发的事件，插件会白等 |
| `ai.videos` | 有，宿主用 AVFoundation 抽 8 帧当图片送审 | **没有**。带 `videoPaths` 报 `videosUnsupported`；图片附件（`ai.images`）两端都有 |
| `process.run` 确认 | NSAlert，可勾「以后不再询问」 | 宿主页面的确认框，**不记住**。另有一条 macOS 没有的硬地板（见上） |
| 选文件 | 插件自己弹原生框 | 宿主接管：浏览器选 + 上传（见上） |
| `defaultPinned` | `bundled` 来源可占住入口，用户不能取消 | 只影响排序建议。rs 没有 bundled 来源，插件一律来自共享目录 |
| MCP 工具执行确认 | 非 bundled 来源每次执行都要用户点头 | 页面命令不逐条确认，边界由启用前的权限审批把住；聊天里调插件工具走与配置 MCP 工具同一套审批（`mcp:<工具名>`，可 Always Allow） |
| 聊天里的插件工具 | 一等工具，参数是 `arguments_json` 字符串，直接进每回合工具表 | 进 `list_mcp_tools` / `call_mcp_tool` 两个元工具（与配置的 MCP 工具一致），schema 按需搜索，不随每回合进上下文 |
| 插件 MCP 网关 | App 进程内，随 App 常驻 | `willdeep web` 进程内；没开 Web 就没有网关 |
| 网关 stdio 中转的错误 | 一律包成 -32603 | 插件回的 JSON-RPC 错误对象原样带回；宿主侧失败才 -32603 |
| 反向请求 | `willdeep/images/generate`、`willdeep/ai/complete` | 同（均不含 `willdeep/audio/synthesize`）；`ai/complete` 带 `videoPaths` 报 `videosUnsupported` |
| stdio 服务需要 `process.execute` | 缺了整个包拒载 | 包照载、页面命令照跑；网关与聊天工具不暴露这个服务 |
| `minimumWillDeepVersion` | 校验 | 解析但**不校验**（`PluginPackage::check_minimum_version` 没有调用方）。有意为之：插件写的是 macOS 版号（短剧工坊 `1.343.0-rc1`），拿 rs 的 `0.8x` 去比会把所有插件拒掉；要校验得先有按宿主区分的字段 |
| secret 存储 | Keychain | `plugin-registry.web.json`（0600）。**没有系统钥匙串加持**，敏感度高的凭据请仍然放 Keychain 并用引用 |
| 图标 | SF Symbols | `web/src/sfSymbols.tsx` 的等价线性图标；认不出的名字回落成圆点 |
| 安装来源 | 目录 / ZIP / Git / Codex 缓存 / AI 草案 | 目录（`install`）、批量导入（`import`）；ZIP 与 Git 尚未接 |
| 页面桥能力 | 桥 2.6.0，25 项 | 桥 2.6.0，23 项（0.74.0-rc1 起）。差集是 `ai.reasoning`（本宿主的 `ai.complete` 一次性返回，不发思考增量）与 `ai.videos`（没有视频解码器抽帧） |
| `process.run` 确认 | NSAlert，可勾「以后不再询问」 | 宿主页面的确认框，**不记住**；另有一层 macOS 没有的硬地板（外泄 / 接管 / 持久化 / 反取证，确认也不放行） |
| 选文件 | 插件自己弹原生框 | 宿主接管：浏览器选 + 上传（见下） |
| 调度 (`schedules`) | 设计中，未实现 | 同 |

## 代码落点

| 层 | 位置 |
|---|---|
| 清单解析与校验 | `crates/willdeep-core/src/plugin/manifest.rs` |
| 包发现、路径安全、digest | `crates/willdeep-core/src/plugin/package.rs` |
| 启用与审批状态 | `crates/willdeep-core/src/plugin/registry.rs` |
| 声明式 UI 限制 | `crates/willdeep-core/src/plugin/declarative.rs` |
| 运行时与每插件 MCP 隔离 | `crates/willdeep-core/src/plugin/host.rs` |
| MCP `resources/*` | `crates/willdeep-core/src/mcp.rs` |
| stdio 传输（常驻读任务、反向请求、超时语义） | `crates/willdeep-core/src/mcp/stdio.rs` |
| 反向请求接口 / 实现 | `crates/willdeep-core/src/plugin/host_requests.rs`、`crates/willdeep-cli/src/plugin_host_requests.rs` |
| 聊天工具目录 / 按需调用 | `crates/willdeep-core/src/plugin/tool_catalog.rs`、`chat_tools.rs`；挂载在 `crates/willdeep-cli/src/harness.rs` |
| 网关发现文件与客户端 | `crates/willdeep-core/src/plugin/gateway.rs` |
| 网关服务端 | `crates/willdeep-cli/src/plugin_gateway.rs`（启动在 `web.rs`） |
| 测试用假插件 MCP 服务 | `crates/willdeep-core/tests/fixtures/fake_plugin_mcp.py` |
| Web API、CSP、资源服务 | `crates/willdeep-cli/src/plugin_web.rs` |
| 页面能力（fs / process / net / storage / skills / 生图 / 宿主动作） | `crates/willdeep-cli/src/plugin_capabilities.rs` |
| 注入页面的宿主桥 | `crates/willdeep-cli/src/plugin_bridge.js` |
| CLI 子命令 | `crates/willdeep-cli/src/plugin_cmd.rs` |
| 一级入口 / 页面 / 侧栏 / 插件中心 | `web/src/PluginRail.tsx`、`PluginPage.tsx`、`PluginSidebar.tsx`、`PluginCenter.tsx` |
| 菜单贡献点 | `web/src/PluginMenus.tsx`（浮层）、`pluginMenuModel.ts`（命令收集与选中监听） |

一级入口写进 URL hash：`#plugin/<plugin-id>:<destination-id>`、`#plugins`。
刷新和分享链接都会回到同一个目的地。
