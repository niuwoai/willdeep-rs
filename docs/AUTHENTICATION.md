# 认证与凭据

WillDeep 涉及四类互不相通的凭据：Provider API Key、some.im 浏览器登录、Runtime Daemon 控制 Token、手机中继 Token。本文说明它们各自的来源、存放位置和安全边界。

## 一、Provider API Key

### 解析优先级

Key 按以下顺序解析，取第一个非空值：

```text
1. --api-key（与 WILLDEEP_API_KEY 是同一层，命令行优先，缺省回落环境变量）
2. Provider Profile 的 api_key（内联明文）
3. Provider Profile 的 api_key_env 指向的环境变量
4. Provider 专属环境变量：
     some-im             → SOMEIM_API_KEY
     anthropic           → ANTHROPIC_API_KEY
     openai-compatible   → OPENAI_API_KEY
```

全部为空时启动失败，并明确提示这四条路径。

> **子 Agent 的例外**：子 Agent 从 Profile 直接构造 Provider 配置，只走上面的第 2、3、4 层，**不继承** `--api-key` / `WILLDEEP_API_KEY`。给子 Agent 绑定独立 Profile 时，必须在该 Profile 里写 `api_key` 或 `api_key_env`。

API Base 的回退是独立的一条链：`--api-base` / `WILLDEEP_API_BASE` → Profile 的 `api_base` → Provider 内置默认值（some-im 为 `https://some.im/v1`，anthropic 为 `https://api.anthropic.com`）。

### 推荐做法

优先使用 `api_key_env`，让 Key 留在环境变量或密钥管理器里：

```toml
[providers.anthropic]
provider = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"
```

配置中的 `api_key` 与 `api_key_env` **不能同时定义**，两者都不允许为空字符串。

命令行传 `--api-key` 会进入 Shell 历史，只适合一次性调试。

### 配置文件权限强制

Unix 下，只要任意 Provider Profile 里出现**内联明文 `api_key`**，配置文件权限必须对 group 和 other 完全关闭（即 `mode & 0o077 == 0`，`0600` 或 `0700` 通过，`0644`、`0640` 一律拒绝）。不满足时 WillDeep **拒绝启动**：

```bash
chmod 600 ~/.willdeep/config.toml
```

全部使用 `api_key_env` 时不触发该检查。Windows 上此检查为空操作。

同一校验在 `willdeep config check` 和 `willdeep config show` 中也会执行。`config show` 会把 `providers.*.api_key` 替换为 `[REDACTED]`，`api_key_env` 的名字保留（环境变量名不是秘密）。

`willdeep config init` 创建文件时直接用 `create_new` + `0600` 打开，不存在权限窗口。

### 脱敏

Provider 返回的错误体在写入日志或界面前会脱敏：替换 `Bearer `、`sk-`、`api_key":"` 三类标记以及 Key 原文为 `[REDACTED]`，并截断到 8 KB。

Runtime 协议层有防泄漏断言：序列化后的公开 DTO 不允许包含 `api_key`、`authorization` 或 `x-willdeep-token`。

## 二、some.im 浏览器登录

`willdeep --onboarding` 提供两条路：`1) some.im 浏览器登录（推荐）`，`2) 手动填写 API Base / API Key`。首次运行且默认配置路径不存在时会自动进入。

登录需要 stdin 是 TTY，非交互环境直接失败。

### 流程

1. 本地生成一次性设备码（形如 `WD-A1B2-C3D4`）和一个 64 位十六进制配对 token；
2. 终端打印 `https://some.im/customer/login?...` 登录 URL 与设备码，用户自行在浏览器打开并授权；
3. CLI **轮询** `GET https://some.im/api/v1/public/client-login/browser-status`，间隔 2 秒，最多 180 次（约 6 分钟）。认证使用配对 token 的 Bearer 加上客户端密钥头；
4. 服务端返回 `connected` / `success` / `completed` / `authenticated` 视为成功，`expired` / `cancelled` / `timeout` 立即失败；
5. 成功后把返回的 API Key 与默认模型写入配置文件的 `[providers.default]`。

这是"打开 URL + 轮询状态"，**不是 OAuth 授权码交换**：没有 refresh token，没有本地回调 HTTP 服务器，CLI 全程不监听端口。

### `WILLDEEP_CLIENT_LOGIN_SECRET`

浏览器登录需要一个**客户端密钥**，通过 `WILLDEEP_CLIENT_LOGIN_SECRET` 环境变量注入，作为请求头发给 some.im。它标识的是"WillDeep 这个客户端"，不是用户身份。

- 发布构建时由构建流程注入；
- **不得提交到 Git**；
- 缺失时登录直接失败，并提示可改用手动 API Key。

### 落盘

登录结果写入 `--config` 指定的路径，或默认的 `$WILLDEEP_HOME/config.toml`（未设 `WILLDEEP_HOME` 时为 `~/.willdeep/config.toml`），Unix 权限设为 `0600`。

**没有独立的凭据存储文件**——some.im 的 API Key 就以明文形式存放在 `config.toml` 的 `providers.default.api_key` 中。因此上一节的 `0600` 权限强制对 some.im 登录用户是常态生效的。

## 三、Runtime Daemon 控制 Token

Daemon 的本地控制面完全依赖一个进程级 Token。

| 项目 | 说明 |
|---|---|
| Header | `x-willdeep-token` |
| 生成 | 每次 Daemon 启动生成一个随机 UUID（32 位十六进制） |
| 存放 | `$WILLDEEP_HOME/runtime/daemon.json` |
| 权限 | Unix `0600`，原子写入（临时文件 + rename） |
| 覆盖范围 | **所有** HTTP 端点，包括 `/v1/health` 和 `/v1/capabilities` |

Daemon 退出时只在 Token 匹配的情况下删除 `daemon.json`，同时清理锁文件与 Unix socket。

**不要复制或提交 `runtime/` 目录。**

### 传输选择

Daemon 同时监听随机 `127.0.0.1` 端口和一个本地传输通道。客户端按以下顺序选择：

1. Unix domain socket（绑定后立即设为 `0600`，且拒绝替换非 socket 文件）；
2. Windows Named Pipe（`reject_remote_clients` 开启）；
3. 受 Token 保护的回环 TCP（兼容旧状态文件）。

三种通道走同一套路由和同一个 Token。

### `/v1/internal` 私有端点

进程内 Harness 与 Daemon 之间的通信走 `/v1/internal/*`，除 Token 外还要求 `x-willdeep-internal-transport: 1`。

标记缺失或值不对时返回 **404 而非 401**——刻意不暴露这些端点的存在性。这个标记**不是第二个秘密**，真正的认证仍然是 Token。这些端点不属于公开协议，第三方客户端不应依赖。

### Token 不外泄给工具

任务执行时，Runtime 连接信息在 Daemon 进程内直接构造并交给进程内 Harness，**不经环境变量，也不落磁盘**。Runtime 控制 Token 不会传给 Shell 子进程或 MCP 子进程。

### 单实例锁

`runtime/daemon.lock` 保存另一个独立的随机 Token，用于单实例互斥，与 HTTP 认证 Token 无关。Daemon 通过短周期心跳续租；异常退出后一次 `daemon start` 会等待旧租约过期并安全接管。

## 四、手机中继 Token

`/mobile` 使用独立于 Swift App 的 room 与 token。

| 项目 | 说明 |
|---|---|
| 凭据文件 | `$WILLDEEP_HOME/mobile-relay.toml` |
| 权限 | Unix `0600`；先写临时文件并设权限再 rename，无权限窗口 |
| 校验 | 已存在时先检查 `mode & 0o077 == 0`，不合规则拒绝启动中继并提示 `chmod 600` |
| room | `willdeep-cli-<uuid>` |
| token | 64 位十六进制随机值 |
| 连接 | `wss://j.niuwoai.com/ws/broadcast/<room>`，`Authorization: Bearer <token>` |

**配对二维码中明文携带 relay token**，扫码即等于交出该中继房间的访问权。只对自己的手机扫码，不要把二维码截图外传。CLI 端不监听任何本地端口，只主动外连中继。

详见 [手机中继](MOBILE.md)。

## 五、Web 模式没有应用层认证

`--web` 是**单用户模式，不实现任何应用层鉴权**。启动时会打印警告。

跨机器访问必须由 Nginx、VPN 或 SSH Tunnel 提供认证与 HTTPS，**不应把端口直接暴露到公网**。Web 层的工作区归属校验是权限边界，不是身份认证。详见 [Web 端指南](WEB_GUIDE.md)。

## 六、凭据相关环境变量一览

| 环境变量 | 用途 |
|---|---|
| `WILLDEEP_API_KEY` | 通用 API Key，与 `--api-key` 同层 |
| `SOMEIM_API_KEY` | some.im Key 回退 |
| `ANTHROPIC_API_KEY` | Anthropic Key 回退 |
| `OPENAI_API_KEY` | OpenAI-compatible Key 回退 |
| `WILLDEEP_CLIENT_LOGIN_SECRET` | some.im 浏览器登录的客户端密钥，构建时注入 |
| `WILLDEEP_HOME` | 配置与运行时目录，默认 `~/.willdeep` |
| `WILLDEEP_CONFIG` | 显式配置文件路径 |

完整环境变量清单见 [配置指南](CONFIGURATION.md)。

## 相关文档

- [配置指南](CONFIGURATION.md)
- [some.im 集成](SOMEIM_INTEGRATION.md)
- [Runtime Daemon 与工作区](RUNTIME_DAEMON.md)
- [审批与自动化](APPROVALS.md)
