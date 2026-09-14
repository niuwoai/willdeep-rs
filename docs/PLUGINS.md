# 插件系统（Web 宿主）

> 状态：v1 已实现，首发 0.50.0-rc1；宿主能力补齐到桥 2.5.0 于 0.73.0-rc1。
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
| 权限审批 | ❌ 各存各的 | 两个宿主的沙箱边界不是一回事：这边是 opaque-origin iframe + CSP，那边是非持久化 WKWebView + 自定义协议。跨宿主复用审批，等于替另一个宿主替用户点了头 |

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

桥版本 **2.5.0**，与 macOS 宿主 `AgentPluginPageBridgeVersion` 对齐。页面按
`window.willdeep.capabilities` 降级，而不是猜宿主有什么：

```js
window.willdeep.version                            // "2.5.0"
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

## 认不出的清单词汇怎么办

权限、宿主动作、菜单位置三张表在两端各自用白名单校验。一侧先加了一项，
另一侧**不再**把整个包判非法——用户看到的是「装不上」，真相只是这个宿主还没实现
其中一项。现在一律记进 `unsupported`（形如 `permission:x` / `hostAction:y` /
`menu:z`）照装，插件中心显示「本宿主不支持 · 某项」：

- 认不出的权限授不出任何东西——宿主的每道门问的都是「有没有这一项**已知**权限」；
- 认不出的宿主动作在**执行时**才拒（`UnsupportedHostAction`），任意 selector 永远
  变不成一条能执行的动作；
- 认不出的菜单位置不显示。

`networkDomains` 是例外，写法不合规直接拒装：它是 `net.fetch` 唯一的门，
一条写法含糊的规则等于一扇关不上的门。

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

## 与 macOS 宿主的已知差异

| 项 | macOS | Web |
|---|---|---|
| `ai.reasoning` | 有，流式思考增量 | **没有**。本宿主的 `ai.complete` 一次性返回，所以这一项不出现在 `capabilities` 里——声明一个自己不发的事件，插件会白等 |
| `process.run` 确认 | NSAlert，可勾「以后不再询问」 | 宿主页面的确认框，**不记住**。另有一条 macOS 没有的硬地板（见上） |
| 选文件 | 插件自己弹原生框 | 宿主接管：浏览器选 + 上传（见上） |
| `defaultPinned` | `bundled` 来源可占住入口，用户不能取消 | 只影响排序建议。rs 没有 bundled 来源，插件一律来自共享目录 |
| MCP 工具执行确认 | 非 bundled 来源每次执行都要用户点头 | 目前不逐条确认；边界由启用前的权限审批把住 |
| secret 存储 | Keychain | `plugin-registry.web.json`（0600）。**没有系统钥匙串加持**，敏感度高的凭据请仍然放 Keychain 并用引用 |
| 图标 | SF Symbols | `web/src/sfSymbols.tsx` 的等价线性图标；认不出的名字回落成圆点 |
| 安装来源 | 目录 / ZIP / Git / Codex 缓存 / AI 草案 | 目录（`install`）、批量导入（`import`）；ZIP 与 Git 尚未接 |
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
| Web API、CSP、资源服务 | `crates/willdeep-cli/src/plugin_web.rs` |
| 注入页面的宿主桥 | `crates/willdeep-cli/src/plugin_bridge.js` |
| CLI 子命令 | `crates/willdeep-cli/src/plugin_cmd.rs` |
| 一级入口 / 页面 / 侧栏 / 插件中心 | `web/src/PluginRail.tsx`、`PluginPage.tsx`、`PluginSidebar.tsx`、`PluginCenter.tsx` |
| 菜单贡献点 | `web/src/PluginMenus.tsx`（浮层）、`pluginMenuModel.ts`（命令收集与选中监听） |

一级入口写进 URL hash：`#plugin/<plugin-id>:<destination-id>`、`#plugins`。
刷新和分享链接都会回到同一个目的地。
