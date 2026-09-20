# OS 级围栏：写入与网络

> 状态：**默认开**（v0.78.0-rc25 起，机器上有后端就开）。写入围栏罩住主 Agent 的 Shell 命令、
> 后台任务、监视器、子 Agent 的命令与 verifier；网络围栏按档位断通，断网时有逃生口。

## 为什么需要它

在此之前，"agent 只能改工作区里的东西"这句话由三样东西保证：审批闸门、
[静态命令分类器](APPROVALS.md)、以及子 Agent 的写集校验。

三样都在**进程内**。它们判的是「模型请求做什么」，不是「进程实际能做什么」。
一条被判成安全的命令——比如一个测试脚本——自己 fork 出去写
`~/.ssh/authorized_keys`，上面三道闸门一道都不会响，因为没人向它们请求过。

围栏补的就是这个差：写入范围和网络交给内核裁决。

## 它是什么，不是什么

**是**写入围栏加网络围栏：进程能读、能跑，只能往指定的几个根里写；网络按档位通或断。

**不是**完整的牢笼。读取不受限——源码读得到，`~/.aws/credentials` 也读得到。
要完整隔离得上容器或虚拟机，别指望这一层。

这样切是故意的。从"什么都不许"起步的策略更安全，但 `cargo`、`npm`、`git`
会因为读不到 dyld 缓存、`/dev`、证书库而以千奇百怪的方式挂掉，结果是所有人
第一天就把沙箱关了——**一个被关掉的沙箱防不住任何东西**。

## 档位，对齐已有的工作区策略

不新造一个轴。围栏档位是[工作区策略](RUNTIME_DAEMON.md)的 OS 侧投影，
用户已经选过一次的东西不该再选第二次。

| 工作区策略 | 写入 | 网络 | 需要联网的命令 |
|---|---|---|---|
| `read_only` | 无 | 断 | 不跑 |
| `strict` / `smart` | 工作区 + 临时目录 + 工具链缓存 + 显式放行的根 | **通** | 静态规则 / AI 判官照常把关 |
| `workspace_write` | 同上 | **断** | 模型用 `network: true` 重试，你放行一次或始终允许 |
| `full_access` | 不加围栏 | 通 | 不问 |

- `strict` / `smart` 缺省通网，因为这两档联网命令本来就要过判官或问人，再断网只会让
  `cargo build` 拉依赖时莫名失败。
- `workspace_write` 不请判官，它的承诺是「命令留在工作区里」，往外发东西不在承诺里，所以缺省断网。
- `agent.sandbox_network = "deny"` 让所有套围栏的命令都断网（`strict` / `smart` 也断），
  `"allow"` 让 `workspace_write` 也通。`read_only` 永远断。
- 围栏按**当前**档位选取，会话中途切档后的下一条命令即生效。

## 断网时怎么办：逃生口

围栏断网的命令失败时，工具结果里会多一段：

```
<sandbox-denied>
这条命令看起来需要联网，而当前围栏断网（不是命令本身写错了）。
当前档位只允许写入：…；网络：断。
如果它确实必须联网，用 network: true 重新调用 run_command，用户会被询问是否放行这条命令；
能不联网完成（本地缓存、离线模式）就优先那样做。
</sandbox-denied>
```

模型带 `network: true` 重试时，**由人放行**：判官不判这个，档位也不管这个。放行可以「始终允许」，
记的是带 `network:` 前缀的规范化完整命令，与不联网跑的同一条命令分开记；审计里记一笔
`user` 来源、原因「network fence」。`full-access` 没有围栏，逃生口自然不问。

两个后端都只有「通」和「断」：Seatbelt 能按 IP 端口放行，bubblewrap 只能整个网络命名空间拿掉，
为了两边语义一致不做中间档。**断网连回环也断**——bwrap 新命名空间里 `lo` 本来就不通，
Seatbelt 跟着一样。本机服务（数据库、mock server）要连的话走逃生口。

## 默认放行的写入根

工作区与系统临时目录**总是**可写。此外默认放行这些工具链缓存（不存在的自动略过）：

```text
~/.cargo/registry  ~/.cargo/git  ~/.npm  ~/.yarn  ~/.cache
~/Library/Caches   ~/Library/pnpm  ~/.local/share/pnpm  ~/go/pkg/mod
```

不放行的话 `cargo fetch`、`npm install`、`pip install`、`go build` 第一天就撞墙，然后所有人把围栏关掉。
`agent.sandbox_toolchain_caches = false` 整组去掉；再要放行的根写进 `agent.sandbox_writable_roots`，
支持 `~/…`，相对路径拒收（相对谁说不清）。

## 两个后端，同一套语义

| 平台 | 后端 | 机制 |
|---|---|---|
| macOS | `sandbox-exec`（Seatbelt） | profile 里 `(deny file-write*)` 之后逐个 `subpath` 放行；断网 `(deny network*)` |
| Linux | `bwrap`（bubblewrap） | 整个根 `--ro-bind` 挂只读，可写根再 `--bind` 盖回去；断网 `--unshare-net` |

两边的可观察语义必须一致，而"一致"这件事由**同一批断言两边各跑一遍**保证，
不由这张表保证：工作区内可写、工作区外被拒且文件不被创建、只读档连工作区都
写不了、只读档仍然能读、`/dev/null` 仍然可写、断网的围栏连回环都到不了而通网的能。

`bwrap` 不是每台机器都装了：`apt install bubblewrap` / `dnf install bubblewrap`。

## 装了不等于能用

围栏的探测跑的是「能不能用」，不是「装没装」——先真跑一条最便宜的命令，
跑得通才算数。

这个区别是踩出来的：默认配置的 Docker 容器里 `bwrap` 明明在，跑起来却是
`Creating new namespace failed: Operation not permitted`，因为容器默认的
seccomp / capability profile 不给建命名空间。只查文件在不在的话，我们会声称
有围栏，然后**每一条命令**都以这句话失败。服务器跑在容器里是常态，这不是边角。

在容器里要用围栏，容器本身得有权限建命名空间（`--privileged`，或按需授予
`CAP_SYS_ADMIN` 并放宽 seccomp）。给不了就别开——**没有围栏，好过一个会把每条
命令都打回来的假围栏**。

## 开关与诊断

```toml
[agent]
# sandbox = true                 # 不写：有后端就开；true：没后端时命令拒绝启动；false：关
# sandbox_writable_roots = ["~/.pyenv"]
# sandbox_toolchain_caches = true
# sandbox_network = "deny"       # 或 "allow"；不写按档位
```

没后端的机器：`agent.sandbox` 不写时命令**不套围栏**照常跑，TUI 开屏和 `willdeep doctor`
会说明「这台机器没有围栏」；显式 `sandbox = true` 时命令拒绝启动（fail closed）。
`willdeep doctor` 的 `sandbox` 项报后端、开关来源、网络策略，以及配置里解析不出来的放行根。

## 被拦下来是什么样

写入越界时工具结果里会多一段：

```
<sandbox-denied>
这条命令看起来是被 OS 级写入围栏拦下的，不是命令本身写错了。
当前档位只允许写入：/path/to/workspace、/var/folders/.../T、…
把写入目标改到允许范围内，或请用户放宽工作区策略后重试。
</sandbox-denied>
```

把「命令自己错了」和「命令被围栏拦了」分开说，是为了让模型有机会自己改到
工作区里去，而不是把同一条越界命令再试三遍；也是为了让人不必对着一句
`Operation not permitted` 怀疑二十分钟自己的代码。两个平台的措辞不同
（macOS 说 `Operation not permitted`，Linux 说 `Read-only file system`；断网时是
`Could not resolve host`、`Network is unreachable` 之类），识别都覆盖了。

## 现在还没有的

- **没罩住的执行路径：** `[[hooks]]` 的 hook 命令、MCP stdio 子进程、宿主内部的 `git` / `rg`
  调用。前两者是用户自己配的程序，后者是宿主自己的动作，都不是模型发起的命令。
- **没有 Linux Landlock 后端。** 没装 bubblewrap 的机器就是没有围栏。
- **不限制读取。** 见上文"是什么，不是什么"。
- **网络没有中间档。** 不能只放某个域名；要么通要么断，断了走逃生口。

## 相关文档

- [审批与自动化](APPROVALS.md) — 进程内的三道闸门
- [Runtime Daemon 与工作区](RUNTIME_DAEMON.md) — 工作区策略从哪来
- [子 Agent 与后台任务](SUBAGENTS.md) — 写集校验与 Worktree 隔离
