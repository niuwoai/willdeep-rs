# 生命周期挂钩（Hooks）

> 状态：**可用**。当前覆盖 `pre_tool` / `post_tool` 两个触发点。

审计要的是"谁在什么时候做了什么"，合规要的是"没批准就不许干"。这两件事都不能
交给一条可以丢的通知。

## 和通知 webhook 的区别

| | `[notifications]` webhook | `[[hooks]]` |
|---|---|---|
| 时机 | 事后 | 事中 |
| 位置 | 关键路径**外**，detached | 关键路径**上** |
| 端点挂了 | 最多浪费一次超时，轮次照跑 | 阻塞式 hook 会拦下动作 |
| 用途 | 提醒人来看一眼 | 审计留痕、门禁拦截 |

webhook 那条设计成"永远不会让一个轮次失败"是对的——没人希望因为通知服务器
宕机就干不了活。但也正因如此，**它会丢事件**：把审计需求接到 webhook 上，
丢掉的恰好是最需要留痕的那次。

## 契约

每条 hook 是一条 shell 命令：

1. 事件 JSON 从 **stdin** 喂进去（一行 UTF-8，上限 16 KB）。
2. **退出码 0 = 放行。** 阻塞式 hook 的**非零退出 = 拦截**，stderr 前 2 KB
   作为拒绝理由回给模型和用户。
3. 超时（默认 10 秒）按 `on_error` 处置。

非阻塞 hook 的退出码不影响流程——它就是来留痕的，自己挂了不该连累这次调用。

### 事件载荷

```json
{
  "event": "pre_tool",
  "session_id": "0b4e…",
  "workspace": "/Users/me/project",
  "tool": "run_command",
  "detail": "cargo test -p willdeep-core",
  "outcome": "ok"
}
```

字段刻意扁平：hook 多半是三行 shell 加一个 `jq`，嵌套结构会让最常见的用法难写。

`detail` **已经按审批日志同一套规则脱敏**（`redact_credentials`）。凭据一旦交出
进程就收不回来了，这件事不指望每个 hook 作者自己记得。

## 三个触发点

| 事件 | 时机 | 能拦吗 |
|---|---|---|
| `pre_tool` | 工具即将执行 | **能** |
| `post_tool` | 工具执行完毕 | 不能——事情已经发生了 |
| `approval_resolved` | 一次审批有了结果 | 不能 |

在 `post_tool` 上写 `blocking = true` 会被配置检查直接拒绝启动，而不是默默失效。
一个拦不住任何东西却看起来像门禁的配置，比没有配置更危险。

## 失败时默认拦，不默认放

阻塞式 hook 超时或起不来时，默认 `on_error = "deny"`。

理由是这类 hook 的用途是合规门禁，而**一个坏掉就自动放行的门禁等于没有门禁**
——它恰好会在出事的时候失效。代价是 hook 写坏会把 agent 卡住，所以拒绝理由里
必须点名是哪条 hook、为什么：

```
<hook-denied hook="change-ticket">
缺少变更单编号
</hook-denied>
```

明确不想要这个语义的，写 `on_error = "ignore"`。

## 配置

```toml
[[hooks]]
name = "audit"
event = "pre_tool"
command = "jq -c . >> /var/log/willdeep-audit.jsonl"

[[hooks]]
name = "change-ticket"
event = "pre_tool"
command = "/usr/local/bin/check-change-ticket"
blocking = true
timeout_seconds = 10
on_error = "deny"
```

同一事件上的多条 hook 按配置顺序执行，**第一条拦截即短路**：动作已经不会发生
了，后面的 hook 再跑也只是在为一件不存在的事收集审计记录。

配置写错（事件名拼错、`post_tool` 上开 `blocking`、命令为空、`on_error` 取值
非法）会**直接让启动失败**，不静默跳过。一条被悄悄忽略的门禁 hook 会让用户
以为门禁在生效，而它一次都不会触发。

## hook 自己不进沙箱

[OS 级写入围栏](SANDBOX.md)罩的是模型请求执行的命令。hook 是**操作者自己配的
代码**，信任级别与操作者的 shell 相同：审计 hook 往 `/var/log` 写、门禁 hook 去
问公司的策略服务，都是它的本职。把它关进工作区围栏只会让这两件事都干不成。

反过来说：**能写 `[[hooks]]` 的人，等于能在这台机器上执行任意命令。** 配置文件
的权限就是这条能力的边界。

## 现在还没有的

- **只有三个触发点。** 没有 `session_start` / `turn_end` / `pre_write`。
- **`approval_resolved` 尚未接线。** 事件类型已定义，审批路径还没往外发。
- **hook 改不了参数。** 它只能放行或拦截，不能重写模型请求的命令——能改的话
  就得回答"改完之后审批过的还算不算数"，那是另一个设计。

## 相关文档

- [审批与自动化](APPROVALS.md) — 进程内的三道闸门
- [OS 级写入围栏](SANDBOX.md) — 内核裁决的第四道
- [配置指南](CONFIGURATION.md) — 完整 TOML 结构
