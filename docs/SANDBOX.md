# OS 级写入围栏

> 状态：**预览，默认关**。当前只罩住 Shell 工具这一条执行路径。

## 为什么需要它

在此之前，"agent 只能改工作区里的东西"这句话由三样东西保证：审批闸门、
[静态命令分类器](APPROVALS.md)、以及子 Agent 的写集校验。

三样都在**进程内**。它们判的是「模型请求做什么」，不是「进程实际能做什么」。
一条被判成安全的命令——比如一个测试脚本——自己 fork 出去写
`~/.ssh/authorized_keys`，上面三道闸门一道都不会响，因为没人向它们请求过。

围栏补的就是这个差：写入范围交给内核裁决。

## 它是什么，不是什么

**是**写入围栏：进程能读、能跑、能联网，但只能往指定的几个根里写。

**不是**完整的牢笼。读取不受限——源码读得到，`~/.aws/credentials` 也读得到；
网络只在只读档关闭。要完整隔离得上容器或虚拟机，别指望这一层。

这样切是故意的。从"什么都不许"起步的策略更安全，但 `cargo`、`npm`、`git`
会因为读不到 dyld 缓存、`/dev`、证书库而以千奇百怪的方式挂掉，结果是所有人
第一天就把沙箱关了——**一个被关掉的沙箱防不住任何东西**。

## 三档，对齐已有的工作区策略

不新造一个轴。围栏档位是[工作区策略](RUNTIME_DAEMON.md)三档的 OS 侧投影，
用户已经选过一次的东西不该再选第二次。

| 工作区策略 | 围栏档位 | 内核允许的写入 | 网络 |
|---|---|---|---|
| `read_only` | ReadOnly | 无 | 断 |
| `smart` / `workspace_write` | WorkspaceWrite | 工作区 + 临时目录 + 显式放行的根 | 通 |
| 配置里关掉 | Off | 不加围栏 | 通 |

## 两个后端，同一套语义

| 平台 | 后端 | 机制 |
|---|---|---|
| macOS | `sandbox-exec`（Seatbelt） | profile 里 `(deny file-write*)` 之后逐个 `subpath` 放行 |
| Linux | `bwrap`（bubblewrap） | 整个根 `--ro-bind` 挂只读，可写根再 `--bind` 盖回去 |

两边的可观察语义必须一致，而"一致"这件事由**同一批断言两边各跑一遍**保证，
不由这张表保证：工作区内可写、工作区外被拒且文件不被创建、只读档连工作区都
写不了、只读档仍然能读、`/dev/null` 仍然可写。

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

## 打开

```toml
[agent]
sandbox = true
# 工具链缓存在工作区外面，不放行的话 `cargo fetch` 会失败
sandbox_writable_roots = ["~/.cargo/registry", "~/.cargo/git"]
```

工作区与系统临时目录**总是**可写，不必列。不给临时目录的话，`cargo`、`rustc`、
`git` 全都写不了中间文件，围栏第一天就会被关掉。

## 被拦下来是什么样

命令失败时，如果输出像是撞了围栏，工具结果里会多一段：

```
<sandbox-denied>
这条命令看起来是被 OS 级写入围栏拦下的，不是命令本身写错了。
当前档位只允许写入：/path/to/workspace、/var/folders/.../T
把写入目标改到允许范围内，或请用户放宽工作区策略后重试。
</sandbox-denied>
```

把「命令自己错了」和「命令被围栏拦了」分开说，是为了让模型有机会自己改到
工作区里去，而不是把同一条越界命令再试三遍；也是为了让人不必对着一句
`Operation not permitted` 怀疑二十分钟自己的代码。两个平台的措辞不同
（macOS 说 `Operation not permitted`，Linux 说 `Read-only file system`），
识别都覆盖了。

## 现在还没有的

- **只罩 Shell 工具。** 后台任务（`run_background_shell`）与子 Agent 的
  verifier 走的是另外的执行路径，尚未接入。
- **没有 Linux Landlock 后端。** 没装 bubblewrap 的机器就是没有围栏。
- **不限制读取。** 见上文"是什么，不是什么"。
- **默认关。** 打开会改变已经在跑的命令的行为，这种破坏该由人在知情时选择，
  而不是升级一次二进制就突然撞上。

## 相关文档

- [审批与自动化](APPROVALS.md) — 进程内的三道闸门
- [Runtime Daemon 与工作区](RUNTIME_DAEMON.md) — 工作区策略从哪来
- [子 Agent 与后台任务](SUBAGENTS.md) — 写集校验与 Worktree 隔离
