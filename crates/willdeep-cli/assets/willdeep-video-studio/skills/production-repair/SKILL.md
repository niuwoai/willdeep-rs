---
name: production-repair
description: Continue a short drama in willdeep-rs and repair reproducible Video Studio plugin defects, verify candidates and install updates with a durable checkpoint. Use when the user authorizes production with automatic plugin repair and installation.
---

# 短剧制作与插件修复

以用户指定的短剧 ID 和 Rust 宿主为目标，先读相邻的 `video-studio` 技能。
所有制作工具走 Rust MCP 网关；不启动第二份独立 stdio 插件。不把 Mac 网关当成 Rust 的备用入口。

配套控制器为本技能目录下 `scripts/studio_production.rb`。它只读 `~/.willdeep/mcp-gateway.json` 的 Rust 网关，凭据不出现在命令参数或报告中。命令以 `ruby <控制器绝对路径> <动作>` 调用。

## 制作

1. `inspect` 核对真实插件版本、进程归属和安装根；`checkpoint --drama <ID>` 保存实际 `drama.get_progress`。
2. 按进度中的 `nextSteps` 做一项有明确产物的工作。先读 `drama.get_stage_context`，保护未采用草稿。写主框架/剧本优先 `drama.write_with_panel`；后台任务使用稳定 requestID，用 jobs.wait/status 跟踪。
3. 内容意见、画面质量问题先走创作修订/素材重做。401、余额、限流、上游故障先核实外部状态；不得修改业务成功条件来绕过它们。
4. 完成后保存 checkpoint，展示正文、图像或视频等实际产物。工具返回成功不等于成片质量验收。

## 插件工程修复

本流程只在用户已授权自动修复和安装时使用。授权限于该插件、指定源仓库和 Rust 宿主。不得自动扩展权限、修改凭据、发布公网或更改其它插件。

首次配置由用户授权的主控制器执行 `authorize --source <源仓库> --drama <ID>`，保存固定测试与控制器摘要。已存在策略不得重建来绕过失败。只读检查策略的 source、dramaID、host 是否对应本轮目标。

制作遇到工程问题时：

1. 将脱敏的 incident、原工具/目标 ID、稳定请求 ID、实际与预期结果保存到私有报告。sent/unknown 的外部请求先对账，不重新提交。
2. 在源仓库复现，保留旧版失败证据；新增有意义的回归用例。保护其它会话改动。每个缺陷最多两个候选，仍失败则停止自动修复并报告原因。
3. 修业务代码，版本与 changelog 同步。修复者不得修改 `scripts/` 中已有验证器、此控制器或策略来获取通过；确需改验证器时交给用户另行审查。
4. `verify --source <仓库> --report <绝对报告目录>` 运行固定 Ruby 回归、前端测试和构建，生成 JSON/Markdown 及运行文件摘要。新增回归另行运行并记录。已有检查失败或跳过不得安装。
5. `install --source <仓库> --report <同一目录> --binary <willdeep绝对路径>` 再核对摘要与权限，检查后台任务和上游视频台账，通过正式 Rust 安装器并存安装、批准启用。发现 active/unknown 时等待安全点，禁止强杀插件。
6. 安装返回 `installed_pending_runtime_readback`，尚不算激活。Rust Web 的包集合在进程启动时加载；确认 Web 无正在执行的页面 AI 请求后，按原参数优雅重启选定 Web 进程，不重启 Runtime daemon，不改变 Mac 的启用状态。无法确认安全点则保留新包等待，不能声称完成。
7. `readback` 核对新进程的版本、安装根和实际运行文件摘要；再次 checkpoint。原失败任务真实恢复成功后才把 incident 记 resolved；失败保留两份包及证据，不覆盖业务数据快照。

用户停止时同时停止制作、修复和更新续推，后台已提交任务仍保留并按其真实状态对账。费用与运行参数沿用本轮明确授权，不无限生成。此技能协调 Agent 的代码工具和确定性验证器，不训练模型权重，也不自行将模型意见记为 human accepted/verified。

## 持续改进账本（0.46.0-rc1）

本目录的 `scripts/improvement_ledger.rb` 包装原控制器，不修改原策略。首次由本次用户授权的安装流程执行 `bootstrap`，冻结新增账本与回归测试。以后不得重建策略、改固定复现包、改受保护验证器或清空次数来获取通过。

1. 每次进入制作或定期唤醒，先 `scan`，再 `status`。它读取固定剧的真实 jobs.status，保存私有证据。`completeScan=false` 表示只覆盖返回的窗口，不得宣称全历史已检查。采集时 runtime 是观察时版本，原任务提交版本未知时保持 unknown。
2. dependency 先处理模型、认证或上游状态；creative/needs_human 保留必改意见，不重开写稿循环绕过两轮上限。unknown 保持待调查，不能凭错误关键词记为已确认工程缺陷。
3. 对真实待调查故障先写独立可审查复现包（replay.rb 和固定数据），使用 `reproduce --incident <ID> --packet <目录>`。在尚未修改的实际运行包对应源码上，冻结用例必须产生真实失败断言。它失败后才能注册工程候选。契约见 [replay-contract.md](references/replay-contract.md)。
4. **改代码前** `candidate --incident <ID>`，程序持久化候选次数，每个故障最多两个。然后只修改业务代码，同步 rc/版本和 changelog，保护其它会话改动。
5. `verify --incident <ID> --report <目录>` 运行新旧固定回归及同一冻结用例。必须原版失败、候选通过、版本和内容改变、验证期间源码与验收材料不漂移；失败保存 candidate_failed。不得从自报模型意见取得 resolved。
6. `install --incident <ID> --binary <绝对CLI路径>` 走原正式安装器并核对安全点。出现 install_started/ install_unknown 时先人工对账，不重试安装，不把未知外部副作用当成未发生。成功停在 installed_pending_runtime_readback。
7. 按前文在安全点由 Agent 协调 Rust Web 重连，`readback --incident <ID>` 绑定实际运行候选；随后在授权制作范围内恢复原任务。不要重复提交 sent/unknown 的付费请求。`resolve --incident <ID>` 必须同一冻结用例 live 模式通过、目标剧与故障 ID 一致、实际产物文件及 SHA256 一致，且运行版本未漂移。

每次唤醒最多推进一项工程候选；没有工程证据时保持安静。新问题应积累为固定回归样本，记录真实成功、失败、unknown 和已有用量；没有 token/耗时数据时保留 unknown。开发者主动调用其它 CLI 仍是宿主原有权限，这个账本不是全局沙箱。它只为该流程提供确定性门禁，不训练模型权重。
