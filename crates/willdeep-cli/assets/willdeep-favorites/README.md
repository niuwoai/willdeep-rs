# 收藏夹（Favorites）

在聊天正文里选中一段文字，气泡上就多一个「收藏」；点一下这段文字进收藏夹，
左侧「收藏夹」页面里可以搜索、加标签、写备注、置顶、删除，删错了还能撤销；
新收藏还可用「查看原文」打开对应的完整会话消息，包括主对话已收拢的中间过程。

这是 `chat.selection` 菜单挂载点的参考实现——插件往聊天选区气泡与右键菜单里加动作，
宿主把选中的原文交给插件命令执行。**它随 App 一起发行**（`BundledPlugins/favorites`），
默认出现在左侧插件分组里，不需要用户手工安装。

## 它由三块拼起来

| 文件 | 作用 |
| --- | --- |
| `.willdeep-plugin/plugin.json` | 声明目的地（`defaultPinned`）、页面、命令，以及 `menus["chat.selection"] = ["favorites.add"]` |
| `server/favorites.rb` | MCP stdio 服务端：收藏、备忘、图片读取、编辑、删除与撤销 |
| `ui/index.html` | 收藏夹页面（localWeb 运行时），用 `window.willdeep.executeCommand` 调上面的命令 |

页面没有构建步骤，改完直接生效；也不引任何外部资源——插件页跑在 CSP 收紧的
WKWebView 里（`connect-src 'none'`）。配色从宿主注入的 `--willdeep-*` 变量派生，
深浅色自动跟随；收藏列表使用 CSS 多列瀑布流按页面当前可用宽度排卡，
单条收藏不会横向拉满。布局只认插件内容区，`vw` 在插件分栏里会算错。

## 工具

| 工具 | 参数 | 说明 |
| --- | --- | --- |
| `favorites.add` | `text` / `note` / `tags` / `source` / `source_session_id` / `source_message_id` / `source_turn_id` | 收藏一段文字。同一段文字重复收藏会合并到已有那条并顶到最前，返回 `duplicate: true` |
| `favorites.add` | `text` + 可选 `images[]` | 新建独立备忘；页面支持直接粘贴截图、拖入图片或选择图片文件，允许图片备忘没有正文 |
| `favorites.list` | `query` / `tag` / `limit` | 置顶优先、其余按更新时间倒序；附带标签直方图与存储路径 |
| `favorites.read_image` | `item_id` + `image_id` | 按需读取一张已保存图片，返回 data URL，避免列表一次性传输所有大图 |
| `favorites.add` | 可选 `content: {version: 1, nodes: [...]}` | 保存受限富文档树；服务端生成搜索用纯文本，不接受 HTML/CSS |
| `favorites.read_content` | `id` | 读取全文；列表仅返回摘要、`contentPreview`、`richContent` 和 `hasMoreText` |
| `favorites.open_link` | `url` | 显式点击后打开 HTTP/HTTPS 链接，拒绝凭据和其他协议 |
| `favorites.update` | `id` + `note` / `tags` / `pinned` | 只改传进来的字段 |
| `favorites.remove` | `id` | 删一条，并把删掉的整条原样回传（页面据此做撤销） |
| `favorites.restore` | `item` | 把整条塞回去，保留原 id 与创建时间 |
| `favorites.clear` | `confirm: true` | 清空。不带 `confirm` 直接拒绝，免得被 Agent 顺手调掉 |

标签会被规范化：去空白、忽略大小写去重、单个最长 24 字、每条最多 8 个。

## 数据存在哪

单个 JSON 文件，默认：

```
~/Library/Application Support/WillDeep/plugin-data/favorites.json
```

结构是 `{"version": 5, "items": [...]}`，每条收藏带
`id` / `text` / `note` / `tags` / `source` / `sourceSessionID` / `sourceMessageID` /
`sourceTurnID` / `pinned` / `images` / `content` / `createdAt` / `updatedAt`。
图片实际保存在 `favorites.json.media/<item-id>/`，记录中只保存图片元数据；v1/v2/v3/v4 的老文件读进来会自动补齐新字段，不需要单独迁移。写盘走「临时文件 + rename」，
中途被杀不会留下半截文件；文件被写坏时服务端按空列表继续跑，不会把插件页面拖死。
最多保留 500 条，单条正文上限 100KB（UTF-8 字节）；每条最多 8 张图片，单张最多 8MB，单条图片总量最多 24MB。新写入超限拒绝，不会静默截断正文。

## 富文本与安全边界

备忘编辑区支持剪贴板 `text/html` 的段落、标题、粗体、斜体、下划线、删除线、列表、引用、代码、HTTP/HTTPS 链接、表格与本地图文混排。纯文本粘贴可通过按钮或 Cmd/Ctrl+Shift+V 选择；只有 `text/plain` 的内容（包括 Markdown 源码）保留原文，不自动转换。

原始 HTML 只在不活动的 template 中解析，再转换成 `{version: 1, nodes: [...]}`；不持久化 HTML，也不把原始节点插入可见 DOM。网页 CSS、任意 DOM 属性、脚本、事件、SVG、表单、iframe、远程图片均不保留。链接不得使用 `javascript:` / `data:` / `file:` 或携带账号密码。服务端再次校验文档树；展示只创建白名单元素，使用收藏夹自己的样式。

原始 HTML 最多 1MB，正文最多 100KB，文档 JSON 最多 256KB，最多 2000 个节点、16 层嵌套及 1000 个表格单元格。拒绝粘贴时保留原草稿；长正文显示摘要，展开时读取全文；宽表格局部滚动，长代码和链接换行。图片最多并发读取 4 张，缓存限制约 45MB 的 data URL 字符串、最多 64 张。

用 `WD_FAVORITES_FILE` 可以改存储位置（测试与沙箱用）。

## 选区动作的契约

宿主执行 `chat.selection` 里的命令时，固定传选区参数，并在能定位来源时附带稳定标识：

- `text`：选中的原文（已按聊天导出的净化规则处理）
- `source`：`"chat.selection"`，插件据此区分入口
- `source_session_id` / `source_message_id` / `source_turn_id`：原会话、原消息和所属用户回合；旧记录没有这些字段时保持兼容

命令的 handler 是 `mcpTool` 时，这些参数原样进工具调用的 `arguments`。
执行完气泡会就地变成「✓ 收藏」或「⚠︎ 收藏」停一下再收起，所以点完不会毫无反应。

## 随 App 发行

`Xedit.xcodeproj` 的 **Bundle built-in plugins** 构建阶段把 `.codex-plugin`、
`.willdeep-plugin`、`mcp.json`、`server/` 与 `ui/index.html` 复制进
`WillDeep.app/Contents/Resources/BundledPlugins/favorites`。`bundled` 来源的插件
自动获批，目的地清单里的 `defaultPinned` 也只对这类第一方插件生效；它们执行自己的
MCP 工具时不再逐条弹确认框（第三方插件仍然每次都要点头）。

改了插件里的任何文件都要重新构建 App，`BundledPlugins` 才会更新。

## 回归

```bash
ruby scripts/rich_content_test.rb /tmp/favorites-rich-reports

# 本机安装 Playwright 并安装其 WebKit 运行时后：
PLAYWRIGHT_MODULE_PATH=/path/to/node_modules/playwright BROWSER_ENGINE=webkit node scripts/rich_content_browser_test.cjs /tmp/favorites-rich-reports
```

覆盖版本与命令契约、富文档保存/读取/删除/撤销、v1–v4 兼容、全文搜索、恶意协议与 HTML、超限正文/表格/嵌套、图文顺序和窄窗口/大字号布局。浏览器测试调用隔离存储中的真实 Ruby MCP 服务，输出 JSON、Markdown 报告及渲染截图。
