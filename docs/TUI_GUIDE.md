# TUI 使用指南

WillDeep 的终端界面基于 Ratatui + crossterm，键盘和鼠标都是一等公民。在终端里不带 Prompt 启动即进入 TUI：

```bash
willdeep --profile some-im --workspace .
```

## 界面分区

界面分四个可聚焦区域，`Ctrl+W` 在它们之间循环：

| 区域 | 内容 |
|---|---|
| Prompt（输入区） | 多行输入、`/` 命令候选、`$` 技能候选、附件行 |
| 聊天区 | 用户消息与 AI 最终回复 |
| 活动区 | 最近三条可验证进度：当前轮次、工具调用/完成、上下文压缩、后台结果 |
| 状态栏（右栏，**默认隐藏**） | 项目/分支/变更、Attention Inbox、Agent 状态、Runtime 任务、版本号 |

聊天区只显示用户消息和 AI 最终回复。轮次、Task/Agent ID、工具活动和提交状态只进入活动状态层，不污染对话记录。活动区不展示也不伪造模型的私有思维链。

状态栏**默认隐藏**——它是查阅面板而非常驻面板，聊天区才是主体。`/sidebar` 或 `Ctrl+B` 随时调出；宽终端会在右侧额外显示 Agent、Mobile Relay、手机队列和工具完成情况，窄终端下改为打开覆盖层。隐藏时焦点自动回到输入框。

Attention Inbox 会自动回收陈旧条目：顺利完成的后台任务停留 60 秒，失败/超时/被杀的保留 24 小时——那些需要人处理，但一天前失败的命令只是噪音，会把真正待办的挤出视野。运行中的任务永不回收。要复盘去任务详情与历史里找。

不想等自动回收，可以手动忽略：在 Inbox 上按 `M`，或打开详情弹窗后按 `M`。忽略按 Session 持久保存，重启后不再出现。运行中的条目不能忽略——那是你对它的唯一抓手。

状态栏的「Runtime 智能体」是活动面板，不是归档：只列出仍在运行的 Agent 和结束不超过 5 分钟的 Agent，从未执行过轮次的已结束根 Agent 不再占位，被折叠的历史以 `(+N 已结束)` 附注；`J`/`K`/`R` 等 Agent 操作只作用于这份可见列表。时长按状态区分——运行中标「已运行」，已结束标「耗时」。规则与 Web 侧栏一致。

## 快捷键

聊天记录默认自动跟随最新内容；手动向上翻阅后暂停跟随，回到底部自动恢复。

### 全局

| 按键 | 行为 |
|---|---|
| `F1`（空 Prompt 时也可按 `?`） | 打开全局快捷键帮助；`F1`、`?` 或 `Esc` 关闭 |
| `Ctrl+P` | 全局命令面板：模糊搜索命令、Skills、会话、Agent/任务和工作区文件 |
| `Ctrl+W` | 在 Prompt、聊天区、活动区与状态栏之间循环焦点 |
| `Ctrl+B` | 显示或隐藏右侧状态栏（默认隐藏，等价于 `/sidebar`） |
| `Ctrl+S` | 进入终端原生文本选择模式（详见下文「鼠标」） |
| `Ctrl+C` | 退出并恢复终端 |

### 输入

| 按键 | 行为 |
|---|---|
| `Enter` | 发送 Prompt |
| `Shift+Enter` / `Alt+Enter` / `Ctrl+J` | 插入换行 |
| `/` | 打开命令候选 |
| `$` | 打开技能候选 |
| `↑` / `↓` | 在多行之间移动光标；候选层打开时用于选择候选 |
| `Enter` / `Tab` | 插入选中候选；`Esc` 关闭候选层 |
| `←` / `→`、`Home` / `End` | 移动编辑光标 |
| `Ctrl+Shift+V` 或 `Cmd+V` | 从本机系统剪贴板附加图片 |
| `Ctrl+D` | 删除当前（最近）附件，可重复删除 |

### 聊天与活动

| 按键 | 行为 |
|---|---|
| `Ctrl+F` | 搜索聊天记录；`Enter` / `Shift+Enter` 前后跳转匹配，`Esc` 关闭 |
| `Alt+↑` / `Alt+↓` | 按显示行滚动 |
| `PageUp` / `PageDown` | 按页滚动 |
| `Ctrl+Home` / `Ctrl+End` | 跳到顶部 / 回到底部并恢复自动跟随 |
| `Ctrl+O` | 展开或收起最近 Tool Use 明细 |
| `Enter` / `Space` | 活动区聚焦后展开或收起工具活动 |

### 状态栏

| 按键 | 行为 |
|---|---|
| `Tab` / `Shift+Tab` | 选择分组 |
| `↑` / `↓` | 选择 Inbox 条目 |
| `Enter` | 打开详情 |
| `K` | 停止运行中的任务或后台子 Agent |
| `R` | 重试已结束的后台子 Agent |
| `M` | 标记已读 |
| `Space` | 折叠或展开分组 |
| `Esc` | 返回输入区 |

## 鼠标

TUI 启动时开启鼠标捕获，退出时关闭。可用鼠标完成：

- 点击 Prompt 定位编辑光标；
- 点击聊天区、活动区、状态栏切换焦点；
- 滚轮浏览聊天历史、活动明细、状态栏内容和 Agent 详情；
- 点击状态栏标题折叠分组，点击条目查看详情；
- 在候选层、审批弹层和 `ask_user` 弹层中直接点选；
- 在 Agent 详情中补充指令、停止、重试和查看 Worktree Diff。

Diff Review 模态会独占全部鼠标事件：滚轮只浏览当前 Diff、文件列表或 Commit Preview，不会穿透并滚动底层聊天区。

### 通过 SSH 使用鼠标

**可以用。** 鼠标支持不依赖本地进程访问鼠标设备，而是终端协议的一部分：

1. WillDeep 向终端输出鼠标上报请求，crossterm 0.29 发送的是 `CSI ?1000h`（普通跟踪）、`?1003h`（任意移动跟踪）、`?1015h` 和 `?1006h`（SGR 扩展坐标）。
2. 真正响应这些序列的是**用户本地的终端模拟器**（iTerm2、kitty、Windows Terminal、Terminal.app、Alacritty 等）。用户点击或滚动时，终端模拟器把事件编码成转义序列写入 stdin。
3. SSH 只是透明的字节管道，两个方向的转义序列都原样传输。

所以远程 TUI 和本地 TUI 收到的是同样的字节流，行为一致，无需任何额外配置。SGR 1006 扩展模式也保证了超过 223 列的终端里坐标不会溢出。

### tmux / screen 下的鼠标

隔了终端复用器时，鼠标事件由复用器先接管：

- **tmux**：需要开启 `set -g mouse on`（写入 `~/.tmux.conf`，或临时执行 `tmux set -g mouse on`）。开启后 tmux 会把事件转发给当前 pane 内的程序。未开启时鼠标只会被 tmux 自己吞掉，TUI 收不到任何事件。
- **GNU screen**：鼠标支持较为残缺，建议改用键盘操作，或换用 tmux。

```bash
tmux set -g mouse on
```

### 文本选择与复制

开启鼠标捕获后，终端原生的拖选复制会被程序接管。两种办法：

- 按 `Ctrl+S` 进入文本选择模式。WillDeep 会临时关闭鼠标捕获，把终端还给用户：拖选聊天文字后用 `Cmd+C` / `Ctrl+Shift+C` 复制，按 `Esc` 返回交互模式并重新开启鼠标。状态行常驻 `Ctrl+S 选择` 入口。
- 或者按住修饰键绕过程序：多数 Linux 终端是 `Shift`，macOS 上通常是 `Option`（iTerm2）或 `Fn`。

这一点本地和 SSH 完全相同，不是远程特有的问题。

### 完全不支持鼠标的终端

极老的终端、串口控制台或某些精简环境不响应鼠标上报序列。这种情况下鼠标事件根本不会产生，但**所有操作都有键盘等价路径**——焦点切换、滚动、Inbox 选择、审批、Diff 浏览、Agent 控制都可以纯键盘完成，参见上文快捷键表。

一个诚实的提醒：`?1003h` 表示"上报所有鼠标移动"，即使没有按下按钮。在高延迟或低带宽的 SSH 链路上，鼠标划过终端窗口会产生持续的上行小包。介意的话按 `Ctrl+S` 关掉鼠标捕获即可。

### 剪贴板的真实边界

`Ctrl+Shift+V` / `Cmd+V` 读取的是**运行 WillDeep 那台机器**的系统剪贴板。SSH 会话中，远端进程读不到你本地电脑的剪贴板图片——这是终端的边界，WillDeep 会明确报错，不会静默伪装成功。

文本粘贴不受影响：终端启用了 Bracketed Paste，短单行粘贴直接插入光标处，多行或超过 200 字符的内容显示为 `Pasted text` 附件行，走的是正常的键盘输入通道。

## 斜杠命令

在 Prompt 中输入 `/` 会弹出候选层。可用命令：

| 命令 | 用途 |
|---|---|
| `/help` | 查看本地命令帮助 |
| `/goal <目标>` | 为后续消息持续注入目标约束；`/goal off` 关闭。目标按 Core Session 持久保存，重启及切换会话/工作区后恢复 |
| `/compress` | 立即调用当前 Provider 总结较旧历史，保留最近六条消息并保存会话。历史不足八条时不消耗模型请求 |
| `/mobile` | 管理手机中继，详见 [手机中继](MOBILE.md) |
| `/webapp` | `start`（缺省）/ `stop` / `status` / `127.0.0.1:PORT`，启停或查看本地 Web App |
| `/daemon` | `status`（缺省）/ `start` / `stop` / `upgrade`，管理真正执行命令的 Runtime。`upgrade` 会排空在途工作再交接，耗时较长但不阻塞界面 |
| `/runtime <任务>` | 提交可分离的 Runtime 任务 |
| `/local <任务>` | 仅本轮使用进程内 Harness |
| `/session` | 管理、搜索、切换、Fork 或导出会话 |
| `/workspace` | `list` 列出注册表，`switch <ID>` 原地切换工作区 |
| `/agent` | 查看或控制子 Agent，如 `/agent spawn scout\|reader\|deep <task>` |
| `/diff` | 打开 Diff Review Center |
| `/skills` | 查看当前目录发现的技能 |
| `/sidebar` | 显示或隐藏右侧状态栏（`on` / `off` 显式指定）。状态栏**默认隐藏**，`Ctrl+B` 等效 |
| `/clear` | 清空聊天显示 |

### Runtime 版本不一致

TUI 只是前端，命令实际由 Runtime Daemon 执行。一个几天前启动的 Daemon 会继续按**它自己那版**的审批策略跑，此时 `willdeep --version` 显示的是客户端版本，说明不了实际执行方。

侧栏「运行状态」会在版本不一致时置顶一条黄色警告，对话里也会写一行说明：

```text
⚠ 运行时 0.21.0-rc62 ≠ 客户端 0.22.0-rc5
工具按旧版策略执行 · 请运行 willdeep daemon upgrade
```

在 TUI 里直接 `/daemon upgrade` 即可，无需另开终端。若有任务正等待人工审批，Runtime 不会退出——先把待审批处理掉，交接才能完成。

Prompt 中的 `$skill-name` 会显式读取并附加对应 `SKILL.md`。详见 [Skills 与 MCP](SKILLS_AND_MCP.md)。

## 默认执行路径

TUI 的普通 Prompt 默认幂等收养当前 Core Session，把输入作为该长期 Session 的新 Turn 提交给持久 Runtime。`/runtime <任务>` 是同一路径的明确别名；`/local <任务>` 仅让单轮使用旧的进程内 Harness。

重新进入同一 TUI 会话后继续复用原 Root Agent 和历史上下文。用户输入立即显示，消息文件只由后台 Harness 写入，Turn 结束时 TUI 从唯一 Core Session 历史同步，避免前后台双写产生重复或丢消息。

> 跨 Workspace 切换后 `/local` 会保守禁用，因为进程内 Local Harness 的工具边界在启动时固定。从目标目录重新启动 TUI 后才可再次使用。

## 工具活动显示

Tool Use 默认显示为单条聚合活动摘要：

```text
Tools: 6 calls · list_directory×3 · read_file×2 · git_status×1
```

失败数量不会被隐藏。需要排查时按 `Ctrl+O` 查看最近明细。

状态栏按 Provider Profile 的 `context_window` 显示上下文占比，并显示最近一次输入/输出 Token 与耗时。

## Markdown 渲染

TUI 按终端能力渐进渲染常用 Markdown：标题、粗体、行内代码、引用、列表、代码块和链接。原始会话内容仍以 Markdown 文本保存，不会因渲染而丢失。

## 相关文档

- [Runtime Daemon 与工作区](RUNTIME_DAEMON.md) — Inbox 里的任务从哪来
- [审批与自动化](APPROVALS.md) — 审批弹层的三种决定
- [子 Agent 与后台任务](SUBAGENTS.md) — Agent 详情与 Worktree 合并
- [手机中继](MOBILE.md) — `/mobile` 二维码配对
- [故障排查](TROUBLESHOOTING.md)
