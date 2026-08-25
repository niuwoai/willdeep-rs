# 故障排查

## 先跑 doctor

```bash
willdeep doctor
willdeep doctor --json
willdeep doctor --bundle ./willdeep-diagnostic.zip
```

`doctor` 在**不联系任何 Provider** 的前提下检查：配置有效性、Provider 完整性、工作区、Git、内嵌 Web 资源、Runtime 版本与传输状态。

`--bundle` 导出的 ZIP **不含日志和本地路径**，可以安全地附在问题报告里。

## 配置与启动

### 拒绝启动：配置文件权限不安全

```text
... contains api_key but permissions are 644; run `chmod 600 ...` or use api_key_env
```

配置里有内联明文 `api_key` 时，Unix 要求文件对 group 和 other 完全不可访问：

```bash
chmod 600 ~/.willdeep/config.toml
```

或者改用 `api_key_env`，从根本上绕开这个检查。

### API key is required

四条解析路径全空。按优先级检查：`--api-key` / `WILLDEEP_API_KEY` → Profile 的 `api_key` → Profile 的 `api_key_env` 指向的变量 → Provider 专属变量（`SOMEIM_API_KEY` / `ANTHROPIC_API_KEY` / `OPENAI_API_KEY`）。

**子 Agent 报这个错**时要单独看：子 Agent 不继承 `--api-key` / `WILLDEEP_API_KEY`，必须在它绑定的 Profile 里写 `api_key` 或 `api_key_env`。

### 浏览器登录失败

- `WILLDEEP_CLIENT_LOGIN_SECRET` 未设置 → 发布构建需要注入该客户端密钥，或改用 `--onboarding` 的手动 API Key 分支；
- 登录需要 stdin 是 TTY，非交互环境（CI、管道）无法使用；
- 轮询上限约 6 分钟，超时后重新执行 `willdeep --onboarding`。

### 配置校验

```bash
willdeep config check
willdeep config show      # 内联 api_key 显示为 [REDACTED]
```

常见问题：`api_key` 与 `api_key_env` 同时定义（不允许），或其中之一为空字符串（不允许）。

## TUI

### SSH 连过来鼠标不能用

先确认是不是 tmux 吞掉了事件：

```bash
tmux set -g mouse on
```

SSH 本身对鼠标是完全透明的，远程和本地行为一致。完整原理与排查见 [TUI 使用指南 · 鼠标](TUI_GUIDE.md#鼠标)。

### 无法用鼠标选中并复制文字

鼠标捕获接管了终端的原生拖选。按 `Ctrl+S` 进入文本选择模式（临时关闭鼠标捕获），拖选后 `Cmd+C` / `Ctrl+Shift+C` 复制，`Esc` 返回。

也可以按住修饰键绕过：Linux 终端多为 `Shift`，macOS 上通常是 `Option` 或 `Fn`。

### SSH 下粘贴图片失败

`Ctrl+V` / `Ctrl+Shift+V` / `Cmd+V` 读的是**运行 WillDeep 那台机器**的系统剪贴板。若终端截获 `Cmd+V` 或 `Ctrl+Shift+V`，请用会直接送给 TUI 的 `Ctrl+V`。远端进程读不到你本地电脑的剪贴板，这是终端的边界。WillDeep 会明确报错而不是静默失败。

文本粘贴不受影响。

### Diff 退出后聊天区有残影

已经处理：Diff 内容在渲染前展开 Tab 并转义 ESC、响铃等终端控制字符，退出时强制完整重绘。如果仍然出现，请带上终端型号和 `willdeep doctor --bundle` 报告。

### 终端完全不支持鼠标

所有操作都有键盘等价路径，见 [TUI 使用指南](TUI_GUIDE.md) 的快捷键表。

## Runtime Daemon

### 升级后旧任务被杀了

`daemon stop` 的语义就是**主动取消活跃任务后停止**。想保住任务请用：

```bash
willdeep daemon upgrade
```

旧进程进入 draining，拒绝新工作但保留活跃任务，归零后才交接。

从 rc31 或更早版本首次迁移时，`upgrade` 会保持旧任务不动并要求手动等待后 stop/start——这是安全 Drain 协议的引入边界。

### Daemon 起不来 / 提示租约冲突

异常退出后一次 `daemon start` 会等待旧租约过期并安全接管，耐心等一会儿。

```bash
willdeep daemon status
willdeep daemon logs --lines 200
```

### Rename / Fork / Archive / Delete 报错

这四个操作只允许在会话没有活跃或排队 Turn 时执行，避免覆盖 Harness 正在写入的历史。先停掉：

```bash
willdeep session stop <SESSION_ID>
```

### `session stop` 说会话空闲

`session stop` 只停止目标 Session **当前明确绑定**的 active/queued Turn。空闲 Session 直接报错，不会去猜或误停其他 Session 的任务——这是有意为之。

### 跨 Workspace 切换后 `/local` 用不了

进程内 Local Harness 的工具边界在启动时固定，切换后保守禁用。从目标目录重新启动 TUI 即可恢复。

## Web

### 打不开页面 / 界面是空白

前端产物在编译期嵌入二进制，构建顺序不能颠倒：

```bash
cd web && yarn install --frozen-lockfile && yarn build && cd ..
cargo build --release
```

`willdeep doctor` 的 `web_assets` 检查会直接告诉你嵌入是否成功。

### 图片附件上传失败

Web 请求体总上限 **1 MiB**，而附件是 base64 塞进 JSON body 的。实际能传的图片明显小于单张图片的格式校验上限。大图请改用 TUI，或先压缩。

### 开发模式接口 404

Vite 的代理目标端口**硬编码为 `127.0.0.1:9847`**。如果后端用 `--listen` 改了端口，需要同步修改 `web/vite.config.ts`。

### 工作区下拉里没有我要的目录

工作区准入是**启动白名单 ∩ Runtime 注册表**。两个条件都要满足：

```bash
# 1. 启动时授权
willdeep --web --workspace /path/to/main --web-workspace /path/to/other

# 2. 确认 Runtime 里已注册
willdeep daemon workspaces
willdeep daemon register-workspace /path/to/other --name "其他项目"
```

### 刷新后聊天记录没了

正常情况下会先重载持久历史再按事件游标重新附着。如果没有，检查浏览器是否禁用了 localStorage（游标存在那里）。

注意：Web 端的 `/goal` 只存在于浏览器内存中，刷新即失效，这是设计如此，与 TUI 的持久 `/goal` 不同。

## 自动化与退出码

`willdeep run` 的退出码：

| 码 | 含义 | 常见原因 |
|---|---|---|
| `0` | 成功 | |
| `1` | 配置或内部错误 | 配置无效、权限不合规 |
| `2` | 调用/输入错误 | 参数写错、附件超限 |
| `3` | Provider 错误 | Key 无效、模型不存在、上游报错 |
| `4` | 审批被拒或 Workspace 策略 | 非交互下需要审批的 Shell/MCP |
| `5` | Harness / Tool 执行失败 | 工具执行出错 |

### CI 里 Shell 命令总是被拒

非交互输入下，`smart` / `workspace-write` 只允许工作区内的创建和编辑（`smart` 另允许 `cargo test` 及其只读过滤管道）。**其他 Shell、MCP 和外部操作因为无法交互审批而拒绝**，退出码 `4`。

Harness 会把拒绝结果返回给模型，不会静默放行。要放宽请在已隔离的容器中评估后使用 `--full-auto`，见 [审批与自动化](APPROVALS.md)。

## 提交问题

```bash
willdeep doctor --bundle ./willdeep-diagnostic.zip
```

附上这个包，以及：WillDeep 版本（`willdeep --version`）、操作系统、终端模拟器（TUI 问题）或浏览器（Web 问题）、是否经过 SSH / tmux。

**不要**直接粘贴 `~/.willdeep/config.toml` 或 `~/.willdeep/runtime/` 里的内容，那里面有明文凭据。
