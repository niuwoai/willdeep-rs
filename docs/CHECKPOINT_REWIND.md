# 检查点回退：回到第 N 步

> 体验基线第 11 项（`docs/EXPERIENCE_BASELINE.md`）。目的：一步做错了，回到做错之前——对话和文件一起。
> Diff 审查 + 安全撤销只能把文件退回 git HEAD；这里退的是「那一步当时的样子」。

## 一步是什么

一步 = 一个 Runtime 轮次（你发一条提示词，到它跑完）。「回到第 N 步」是**回到第 N 步刚结束时**：
对话保留到第 N 步的最后一条消息，第 N+1 步及之后全部丢掉；文件恢复到**第 N+1 步开始前**拍的检查点。
「回到开头」是第 0 步，只有从没压缩过的会话做得到——压缩之后消息 0 已经不是开头了。

能回的步骤：Runtime 跑完（`completed`）的轮次，且它的消息边界与当前压缩代数对得上。最后一步不算，你已经在那儿。
本地跑（`/local`）的轮次没有 Runtime 记录，回不了。

## 文件检查点存在哪

每个轮次开始前，Runtime 给工作树拍一张内容快照，存进一个**私有的影子 git 仓库**：

```
$WILLDEEP_HOME/runtime/checkpoints/<工作区路径哈希>/     # 裸仓库
  objects/info/alternates → <工作区>/.git/objects        # 借用工作区自己的对象库
  refs/willdeep/<会话 id>/<轮次 id>                        # 每轮一份，独立 commit，没有父提交
  refs/willdeep/<会话 id>/before-rewind-<随机>             # 每次回退前的整棵树
```

- **不碰用户仓库**：HEAD、索引、分支、`git log --all`、`git status` 一概不变；新对象只写进影子仓库，没改过的文件一个字节不重复存。
- **进快照的**：已跟踪文件全部，未跟踪但没被 `.gitignore` 的文件按上限收：超过 2000 个就只拍已跟踪的，单个超过 4 MiB 的不拍。
- **不是 git 仓库的工作区没有检查点**：只能回对话，面板和对话框会写明「仅对话」。
- 每个工作区最多留 200 份，超了删最老的引用；对象不主动清，需要时 `git --git-dir=<影子仓库> gc --prune=now`。
- 会话删除时它的引用一起删。`WILLDEEP_WORKSPACE_CHECKPOINTS=0` 关掉拍照。
- 拍照在模型动手之前、串行发生；大仓库第一次拍要把工作树哈希一遍，之后只算改过的文件。拍不成不拦这一轮。

影子仓库借用工作区对象库有一个已知边界：工作区那边 `git gc --prune` 只认自己的引用，理论上会清掉只有检查点还在引用的**悬空**对象。这种对象只来自工作区本来就悬空的内容（比如被丢掉的 stash），正常提交过的内容不受影响；真碰上了，恢复会对那一个文件报错而不是写错东西。

## 回退时发生什么

顺序是**先文件、后对话**：文件恢复失败时对话一个字没动，人拿着错误信息能重来。

1. 校验：会话不在跑、没有排队轮次、边界轮次已完成且早不过压缩检查点。
2. 文件（勾了才做）：先把当前工作树拍成 `before-rewind` 快照；把检查点树和当前树做 diff，
   被改过的写回检查点版本，检查点里没有的（之后新建的）删掉，检查点里有、现在没有的重建。
   **每个被覆盖或删除的当前文件先原样进 `runtime/recovery/rewind-<会话>-<随机>/`**——与安全撤销同一个回收区，审计导出看得见。
   子模块（gitlink）不恢复，列在 `skipped` 里。
3. 对话：截断到第 N 步的 `message_end`，被丢掉的轮次从 `turns.json` 摘掉，执行检查点清空（它记的是已不存在的轮次里没拿到结果的调用）。
4. 事件流写一行 `session.rewound`，带丢了几步、恢复/移除了几个文件、回收区路径。

回退本身也可回退：按 `before_checkpoint` 再恢复一次就回来了（`willdeep daemon rewind-session` 目前只按轮次找检查点；`before-rewind` 引用留在影子仓库，可用 git 直接取）。

## 怎么用

**TUI**：`/rewind` 打开面板，每行一步（提示词首行 + `文件✓` / `仅对话`），默认停在最近一步。
Enter 进确认行：再按 Enter 对话与文件一起回，`v` 只回对话，Esc 返回列表。回退后整份会话重读重画，聊天区写一行摘要。轮次运行中拒绝。

**Web**：会话工具栏的 ↶ 打开对话框，单选一步，勾选「同时恢复工作区文件」（这一步没检查点时禁用并说明），底下写会丢几步。

**CLI**：

```bash
willdeep daemon turns <session-id>                                          # 看轮次与 workspace_checkpoint
willdeep daemon rewind-session <session-id> --through-turn <turn-id>        # 只回对话
willdeep daemon rewind-session <session-id> --through-turn <turn-id> --restore-workspace
willdeep daemon rewind-session <session-id> --restore-workspace             # 回到开头
```

**控制面**：`session.rewind { id, through_turn_id?, restore_workspace }` → `RewindSessionResult { session, message_count, dropped_turn_ids, workspace? }`，
`workspace` 里有 `checkpoint`、`before_checkpoint`、`restored[]`、`removed[]`、`skipped[]`、`recovery_path?`。
`turn.list` / `turn.get` 的 `RuntimeTurn` 带 `message_start` / `message_end` / `message_generation` / `workspace_checkpoint`。
Web 另有 `GET /api/sessions/{id}/rewind-points` 与 `POST /api/sessions/{id}/rewind`。

## 边界

- 回的是**这个工作区**的文件；子 Agent 专属 worktree 里的改动不在检查点里（它们要么已经合并进主工作区，要么还在自己的 worktree 里）。
- 只认内容，不认 git 状态：回退不改索引、不改 HEAD，`git status` 看到的是「相对 HEAD 的差异」，就像你手改回去一样。
- 检查点拍的是轮次**开始前**；一轮之内工具逐次改动的粒度仍靠 `/diff` 的归属记录。
- 对话回退不删审批记录、Diff 归属、审计事件——它们是「发生过」的证据，不因回退消失。
