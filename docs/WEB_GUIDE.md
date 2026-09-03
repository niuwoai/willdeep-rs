
# Web 端使用指南

WillDeep 内嵌了一套 React 前端和 JSON API，编译进同一个二进制，不需要额外部署。

## 启动

```bash
willdeep --web --workspace /path/to/project
```

浏览器打开 `http://127.0.0.1:9847`。

| 参数 | 说明 |
|---|---|
| `--web` | 启动 Web UI 与 JSON API，取代 TUI |
| `--listen <IP:PORT>` | 监听地址，默认 `127.0.0.1:9847`。必须写成完整的 `IP:PORT` |
| `-w`, `--workspace <PATH>` | 首选工作区（排在候选列表第一位） |
| `--web-workspace <PATH>` | 额外允许的工作区，可重复。只能写在顶层，不能跟在子命令后 |
| `--project <名称或UUID>` | macOS 上一次载入某个 Swift Project 的全部文件夹 |
| `--language <L>` | 服务端默认语言 |

**没有 `--port` 或 `--host` 参数**，只有 `--listen`。

多工作区：

```bash
willdeep --web \
  --workspace /path/to/main \
  --web-workspace /path/to/frontend \
  --web-workspace /path/to/backend
```

所有候选路径都会被 canonicalize 并去重，无法解析时直接报错。全部为空时回落到当前目录。

### 从 TUI 启动

TUI 中可以用 `/webapp` 拉起一个 Web 子进程：

```text
/webapp               # 默认 127.0.0.1:9847
/webapp start         # 同上
/webapp 127.0.0.1:9900
/webapp status
/webapp stop          # 停止本 TUI 启动的 Web App
```

`/webapp` **只接受回环地址**，比 `--listen` 更严格。日志写入 `~/.willdeep/webapp.log`。

`/webapp stop` 只对**本 TUI 启动并记录在案**的进程发 `SIGTERM`，且先确认它记录的地址仍在应答——状态文件过期时只清理文件，不会误杀继承了同一 pid 的无关进程。

## 界面使用

### 模型与路由设置

设置齿轮里的“模型与路由”打开持久化设置弹窗，可修改 Root Provider/模型、小模型
优先与只读自动派工开关、Deep 调用预算，以及两张表：

- **工种表**：调查、实现、测试与审核、审查、运维执行五个公开职责的
  Provider/模型/上下文窗口，写入 `[subagents.*]`。
- **档位表**：基础、进阶、专家三个模型档位，写入 `[worker_tiers.*]`。专家档带
  🎟 标记——它需要升级票据，绑到多贵的模型都还有那道闸门兜着。

职责选做什么，档位选用多贵的模型。想让某次派工更贵是在 `spawn_agent` 传
`worker_tier`；给某个职责绑贵模型是让它每次都贵。

勾选“推荐默认”会清除该行的显式 Provider/模型：Root 为 some.im 时，职责恢复基础档
`someim-32b`、档位恢复网关默认表，其他 Provider 则继承 Root/Provider 默认模型。

设置直接原子更新启动时选中的 `config.toml`，并用版本指纹防止覆盖用户同时进行的
手工编辑。保存后对新 Harness/子 Agent 生效，正在运行的任务不变。Web 没有应用层
鉴权，因此配置写入仅在服务器监听回环地址时开放；`--listen 0.0.0.0:...` 等非回环
模式只能查看设置。

### 聊天区

用户消息立即显示，助手回复走 Markdown 渲染（支持 GFM 表格、删除线、任务列表、代码块；链接自动在新标签打开）。每轮工具调用在聊天区保留精简轨迹卡片。

运行时 Composer 上方吸附一条单行工作状态；发送按钮切换为停止按钮，点击精确终止对应的 Runtime Turn。

聊天区默认自动跟随底部，向上滚动超过 80 像素后暂停跟随。

### Composer

| 操作 | 行为 |
|---|---|
| `Enter` | 发送 |
| `Shift+Enter` | 换行 |
| 粘贴图片 | 自动转为图片附件并显示缩略图与尺寸 |
| 粘贴长文本 | 含换行或超过 200 字符时自动转为 `.txt` 附件，不塞进输入框 |
| `/` | 命令候选，最多 6 条 |
| `$` | 技能候选，最多 8 条；候选层内另有独立技能搜索框 |

附件条支持逐个删除。归档会话下发送被禁用。

Web 端的 `/` 命令是前端拦截的：`/clear` 清空显示、`/help` 打印帮助、`/skills` 列出技能、`/goal <文本>` 开启目标模式、`/goal off` 关闭。

> **注意**：Web 端的 `/goal` 只存在于浏览器内存中，**刷新页面即失效**，与 TUI 中按 Session 持久保存的 `/goal` 语义不同。

### 会话列表里的标题

标题由两级生成流程产出。还没跑成的会话（标题停在 `New session`、`新对话` 这类占位符）在列表里显示**第一条用户消息的开头**，斜体、淡一档，以示"这是正文不是标题"。

真正的空会话（没有任何用户输入且没有活动 Turn）根本不进列表；带内容但没标题的会话不会被藏起来——藏起来就等于丢东西。

### 会话历史

左栏列出当前工作区的会话，按置顶时间、更新时间倒序，最多显示 20 条，支持标题子串搜索。

只有**已经收到用户输入或仍在运行**的会话才会进入列表，空白会话不占位。

每行悬停出现四个动作：重命名、置顶/取消置顶、归档/取消归档、删除。活动中的会话禁用重命名、归档和删除。删除需要在弹窗中二次确认。

选中会话下方另有 Fork 和 Export；导出为浏览器本地下载的 `willdeep-session-<id>.json`。

"新会话"按钮只清空本地状态，不会在服务端创建空会话。

### Runtime 侧栏

默认收起，展开后持续刷新（每 2 秒轮询一次活动快照和会话列表）：

- **计数行** — 工具数、运行中、产物、Agent、Task、需要处理、运行时事件。Agent 计数只统计侧栏可见的活动 Agent，被折叠的历史记录以 `(+N 已结束)` 附注；
- **运行时事件** — 最多 5 条，只列**还等着人**的那些。每条显示来源、优先级、合并次数、标题摘要，以及模型侧是否已经看过。「Agent 已看过」与「仍需你处理」是两件事，分开显示，免得把 Agent 读过误当成已经办了。「忽略」只结算你这一侧，**不批准任何操作**：审批仍然要在它自己的卡片上回答。事件按当前选中的会话取，没选会话时不请求；
- **创建只读子 Agent** — 选择公开 `reader` / `judge` 加任务描述。需要父会话处于活动状态；命令、写入与 Deep 工种必须由父 Agent 走安全审核、写集审批或升级票据；
- **审批与提问 Gate** — 最多 3 条。审批提供"允许一次 / 拒绝 / 始终允许"（第三项仅在服务端标记可用时出现）；提问支持单选、多选和自由文本回答；
- **Task 列表** — 最多 4 条，显示 Profile、状态、耗时，失败时显示退出码与失败域；
- **Agent 列表** — 活动面板，不是归档：只展示仍在运行的 Agent，以及结束不超过 5 分钟的 Agent；从未执行过轮次（`current_turn = 0`）的已结束根 Agent 直接不显示。最多 4 条，子 Agent 缩进显示，展示模型、Token、时长和 worktree 分支。运行中的时长标注为"已运行"，已结束的标注为"耗时"，避免把会话跨度误读成仍在执行。后台 Agent 运行中可"补充指令 / 停止"，终态可"重试 / 换模型重试"。

点击任意 Task 或 Agent 的"详情"打开详情面板：状态、Profile/模型、轮次、Token、耗时、当前工具、worktree，以及最近 12 条工具时间线和最近 8 条 Diff 产物。

详情面板只使用已脱敏的 Runtime DTO。**Workspace、Prompt、命令、工具参数、输出、报告、路径、模型配置、PID 和内部错误都不会下发到浏览器。**

运行时事件同样只走白名单投影：**事件正文不下发**，标题是按命令审批同一套规则打码、截断到 120 字符的摘要。浏览器端投影独立于协议的 `PublicKernelEvent` 再做一次，所以协议将来往信封上加字段不会自动流到浏览器。事件端点按会话取，且先校验该会话的工作区在服务端白名单里，与会话详情同一条门。

### 进度卡片

一轮里最多显示最近 5 条进度，更早的折成一行「前面还有 N 步」。一次长任务动辄几十步，全列出来会把聊天区顶满，而人真正关心的只有现在在干什么、刚才几步是什么。

输入框上方那条正在思考的横条是**当前状态**，与卡片里的历史不是一回事。它绝对定位在输入框上沿，此前会盖住聊天区最后一行；现在带不透明背景，聊天区底部也留出了它的高度。

### 工作区选择器

左栏下拉，选项文案为「名称 · 访问模式 · 当前」。切换工作区会中止当前流、清空会话与聊天，然后重载会话列表与候选数据。

选择器只影响当前浏览器视图，**不会**改变 Runtime 的全局默认工作区（Web 端没有这个接口，需要用 TUI 的 `/workspace switch` 或 `willdeep daemon activate-workspace`）。

右上角的「添加」可以往这份列表里加一个目录（支持开头的 `~`）。它**只在回环监听时开放**，与模型路由设置同一条既有语义：Web 模式没有应用层鉴权，能连到端口的人就是「这个用户」。对外暴露的实例上，能加工作区就等于能让 Agent 读写机器上任意目录——那条边界必须留在启动命令里：

```bash
willdeep --web --web-workspace ~/Sites/another-project
```

加进来的工作区同样只影响当前浏览器视图的白名单。

### 语言切换

左栏下拉切换简体中文 / 英语 / 日语。首次进入按 localStorage 记忆，其次探测浏览器语言，默认简体中文。语言同时随每次请求发送给服务端，决定事件流里的标签语言。

### 插件入口

最左侧是一级入口栏：「对话」固定在最上，下面是已启用插件贡献的目的地（最多固定
5 个，多出来的收进「更多插件」），最下面是插件中心。选中一个插件目的地时，
它的配套侧栏与中央页面一起切换——入口、侧栏、中央页永远来自同一份描述，
不会出现半切换状态。

当前入口写在 URL hash 里（`#plugin/<插件 id>:<目的地 id>`、`#plugins`），
刷新和分享链接都会回到同一个位置。

插件的命令还有五个顺手入口：**⌘K / Ctrl+K** 打开命令面板，**聊天正文选中**后浮出气泡
（收藏夹的「加到收藏」、待办的「记一条」走的就是这里），会话行右键、Composer 左下角
的「更多」、以及声明式侧栏的行右键。没有插件贡献某个位置时，那里不会多出任何东西——
比如没人贡献会话右键，浏览器自带的右键菜单照常可用。

插件包与 macOS 版共享 `~/.willdeep/plugins/`，但**启用状态与权限审批各管各的**：
在这边用得上一个插件，要先在插件中心（或 `willdeep plugin approve`）看过它要什么
权限并点头。装插件、导入 macOS 版自带的那几个、以及三种页面运行时的差异，
见[插件系统](PLUGINS.md)。

## 断线与刷新

浏览器在 localStorage 中保存两类游标：

- `willdeep.web.last-session.<工作区路径>` — 该工作区最后打开的会话；
- `willdeep.web.runtime-cursor.<会话>.<Turn>` — 该 Turn 已消费到的事件序号。

刷新时先重载持久历史，再用 `GET /api/sessions/{id}/stream?after=<cursor>` 重新附着到活动 Turn。普通网络断开按有界指数退避续接（初始 500 ms，翻倍至上限 5 秒，成功后重置），**不会重复提交 Prompt**。

服务端会把客户端游标夹紧在本 Turn 的合法区间内，客户端无法通过伪造游标读到其他 Turn 的历史事件。SSE 每 10 秒发送一次保活。

首次发送失败时，只要已经拿到 Session/Turn ID 且不是服务端终态失败，前端会切换到重连恢复而**不是重发 Prompt**。

## 安全模型

### 没有应用层鉴权

**Web 模式是单用户模式，不实现任何应用层身份认证**——没有 token、cookie、session 校验，也没有 CSRF 防护。能连到端口的人就是"这个用户"。

监听地址不是回环时，启动会打印警告。跨机器访问必须由 **Nginx、VPN 或 SSH Tunnel** 提供认证与 HTTPS：

```bash
# 推荐：SSH 隧道，Web 只监听回环
ssh -L 9847:127.0.0.1:9847 user@remote
```

**不要把端口直接暴露到公网。**

### 工作区 allowlist

工作区准入是两层取交集：

1. **启动白名单** — `--workspace` / `--web-workspace` / `--project` 规范化后的路径集合；
2. **Runtime 注册表** — Daemon 当前已注册的工作区（提供名称、访问模式、active 标记）。

交集之外的目录一律不可用。从 Runtime 中被移除的工作区不会因为浏览器请求重新开放；启动时没授权的目录也不会因为 Runtime 里有注册而可用。

请求中的 `workspace` 参数只做**精确字符串匹配**，匹配不上返回 400 `workspace is not in the server allowlist`。会话详情、恢复流、置顶等操作同样校验会话所属工作区，即使猜到别的会话 UUID 也读不到白名单外的内容。

### 其他边界

- 审批、提问、Agent 操作会二次校验目标对象确实属于该工作区，跨工作区操作返回 404；
- 停止 Turn（`POST /api/turns/{id}/stop`）请求体里没有 `workspace`，服务端先按 Turn id 反查所属 Session，再校验该 Session 的工作区在 allowlist 内；Turn 不存在、会话读不到、工作区不在白名单三种情况统一返回 404；
- 所有请求体都是 `deny_unknown_fields`，客户端夹带 `workspace_root` / `task_id` / `agent_id` 等额外作用域字段会直接反序列化失败；
- 子 Agent Spawn 只公开 `reader` / `judge` 两种无 Shell、无写入 Profile，父级、Task、Workspace 和 Child ID 全部由 Runtime 推导；
- 模型路由设置可读取但只允许回环监听实例写入；保存携带配置版本指纹，陈旧请求返回 409；
- 静态资源附带 `X-Content-Type-Options: nosniff` 和限制到 `'self'` 的 CSP；
- 请求体上限 1 MiB；
- 技能描述中出现 `password` / `api_key` / `secret` / `token=` 时替换为 `[sensitive description hidden]`；
- 删除会话需要请求体携带同一个 UUID 作为确认。

## JSON API

所有接口在 `/api` 下，健康检查除外。

| 方法 | 路径 | 用途 |
|---|---|---|
| GET | `/health` | `{"status":"ok","version":"..."}` |
| POST | `/api/chat/stream` | 提交一轮对话，返回 SSE 流 |
| GET | `/api/sessions` | 列出会话 |
| GET | `/api/sessions/{id}` | 会话详情 |
| DELETE | `/api/sessions/{id}` | 删除会话（需 `confirmation`） |
| GET | `/api/sessions/{id}/stream?after=` | 断线重连，重新附着活动 Turn |
| POST | `/api/sessions/{id}/rename` | 重命名 |
| POST | `/api/sessions/{id}/fork` | Fork |
| POST | `/api/sessions/{id}/archive` · `/unarchive` | 归档 / 取消归档 |
| POST | `/api/sessions/{id}/pin` · `/unpin` | 置顶 / 取消置顶 |
| GET | `/api/sessions/{id}/export` | 脱敏 JSON 导出 |
| POST | `/api/turns/{id}/stop` | 停止 Turn |
| GET | `/api/workspaces` | 工作区列表与访问模式 |
| GET | `/api/runtime/activity?workspace=` | 工具/产物/Agent/Task/待处理快照 |
| POST | `/api/runtime/approvals/{id}/resolve` | 解决审批 |
| POST | `/api/runtime/questions/{id}/answer` | 回答提问 |
| POST | `/api/runtime/agents/{id}/stop` · `/retry` · `/prompt` | 控制子 Agent |
| POST | `/api/runtime/agents/spawn` | 创建只读子 Agent |
| GET / PUT | `/api/settings/model-routing` | 读取或持久更新模型路由；PUT 仅回环监听开放 |
| GET | `/api/composer?workspace=` | 命令与技能候选 |

所有响应带 `x-app-version` 头。未命中的路径回落到 `index.html`（SPA 路由）。

### 输入限制

- Prompt 1–100000 字符；
- 附件最多 12 个；文本 ≤ 200000 字符；图片仅 PNG/JPEG/WebP/GIF；
- **请求体总上限 1 MiB**——附件以 base64 塞在 JSON body 里，实际能传的图片远小于单张图片的校验上限。

### SSE 事件

客户端可见的事件类型：`submitted`、`resumed`、`turn_started`、`tool_requested`、`tool_completed`、`compression_started`、`compression_completed`、`usage`、`thought`、`completed`、`error`。

事件标签按语言本地化，且**剥离工具参数和输出**。`thought` 文本截断到 240 字。

传输全部使用 SSE，没有 WebSocket（聊天是 POST + SSE 响应，所以前端手写 SSE 解析而非用 `EventSource`）。

## 前端开发

前端产物通过 `rust-embed` 在编译期打进二进制，构建顺序不能颠倒：

```bash
cd web && yarn install --frozen-lockfile && yarn build && cd ..
cargo build --release
```

`web/dist` 为空时单元测试和 `willdeep doctor` 的 `web_assets` 检查都会失败。构建脚本显式跟踪 `web/dist`，前端变化会触发二进制重新嵌入。

开发模式用 Vite dev server + 代理：

```bash
# 终端 1：后端占用 9847
willdeep --web --workspace /path/to/project

# 终端 2：前端热更新
cd web && yarn dev
```

Vite 把 `/api` 和 `/health` 代理到 `http://127.0.0.1:9847`。**代理目标端口硬编码为 9847**，如果用 `--listen` 改了后端端口，需要同步修改 `web/vite.config.ts`。

技术栈：React + TypeScript + Chakra UI v3 + Vite，纯客户端渲染。Markdown 走 `react-markdown` + `remark-gfm`，不渲染原始 HTML。

## 与 TUI 的能力差异

**Web 独有**：图形化附件预览、Markdown 富文本渲染、下拉即时切换语言、会话一键 JSON 导出、下拉切换多工作区视图。

**TUI 独有**：`/mobile` 二维码中继、`/local` 强制进程内执行、`/diff` Diff Review Center（Web 详情面板只显示 Diff 产物的标题和变更条数，看不到具体内容）、`/workspace switch` 切换 Runtime 默认工作区、`/session` 的多维组合搜索（Web 只有前端标题过滤）、Git 项目状态栏、按 Session 持久化的 `/goal`、Attention Inbox 的键盘操作。

## 相关文档

- [认证与凭据](AUTHENTICATION.md)
- [Runtime Daemon 与工作区](RUNTIME_DAEMON.md)
- [TUI 使用指南](TUI_GUIDE.md)
- [Xedit 工具能力对照](XEDIT_TOOL_PARITY.md)
