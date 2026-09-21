# 在 CI 里跑 `willdeep run`

> 决策记录：`docs/decisions/2026-09-20-experience-baseline-and-model-eval.md` 第 4 项。
> 目标：一个作业、一个提示词、一份可归档的结果与审计报告；退出码决定成败，没有人守在旁边。

## 适合与不适合

适合：定时体检（「检查测试是否覆盖了 README 承诺的行为，只报告」）、PR 只读审查、按需派活并把
结果当产物归档。不适合：需要人审批的改动——CI 里没人回答审批，需要审批的命令一律被拒，
作业以退出码 4 结束。

## 安装

```bash
# Linux（amd64 / arm64）、macOS（universal）、Windows（x64）都有发行包，按 tag 取：
curl -fsSL -o willdeep.tar.gz \
  https://github.com/niuwoai/willdeep-rs/releases/download/v0.78.0-rc23/willdeep-linux-amd64.tar.gz
tar -xzf willdeep.tar.gz && install -m 0755 willdeep /usr/local/bin/willdeep
willdeep --version

# 或者从源码装（要 Rust 1.94+、Node 22 与 yarn）。内嵌 Web 的 web/dist 不入库、
# 由 yarn build 生成，所以 `cargo install --git` 在干净机器上会编译失败，要先 clone：
git clone --depth 1 --branch v0.78.0-rc23 https://github.com/niuwoai/willdeep-rs
(cd willdeep-rs/web && yarn install --frozen-lockfile && yarn build)
cargo install --locked --path willdeep-rs/crates/willdeep-cli
```

把版本号钉死在 CI 配置里；不要在流水线里追 `latest`。

## 凭据与状态隔离

- 凭据走环境变量，不写配置文件：环境里有 `SOMEIM_API_KEY`（缺省模型 `glm-5`）或
  `ANTHROPIC_API_KEY`（缺省 `claude-sonnet-4-5`）就直接开工；`OPENAI_API_KEY` 要配
  `WILLDEEP_API_BASE` 和 `WILLDEEP_MODEL`。见 `docs/AUTHENTICATION.md`。
- 把 `WILLDEEP_HOME` 指到作业目录里一个空目录（样例用 `.willdeep-ci`）。会话记录、审批日志、
  Diff 归属都落在那里，作业结束整个目录可以当产物归档，也保证 `audit export --session latest`
  指的就是这一次。
- 密钥只放 CI 的 secret / masked variable；产物里不要带 `config.toml`。

## 运行

```bash
willdeep --workspace . --full-auto --max-turns 40 run --local --output json --input prompt.txt
```

| 选项 | 为什么 |
|---|---|
| `run --local` | 用进程内 Harness。缺省会拉起常驻 Runtime，一次性 runner 上它活到作业结束也没人管；只有 self-hosted、想跨作业复用会话的机器才留 daemon，并记得 `willdeep daemon stop` |
| `--full-auto` | 等于 `smart` 审批档：工作区内创建 / 编辑文件不问；shell 命令走静态规则与 AI judge；仍需要问人的一律拒绝（无人可问） |
| `--max-turns N` | 模型调用上限，触顶交出部分结果（退出码 5）。配置里的 `[agent] token_budget` 是另一道闸 |
| `--output json` | stdout 只有一个 JSON 对象：`type`（`completed` / `partial`）、`stop_reason`、`turns`、`text`、`session_id` |
| `--input FILE` | 提示词从文件来，别把它拼进命令行：CI 的 inputs 拼进 shell 就是注入口 |

只读任务在提示词里写明「只报告，不改代码」；改动类任务把验收命令写进提示词，或在配置里
用 `[agent] verification_commands` 要求它跑完再交。

## 退出码怎么判

| 码 | 含义 | 作业怎么办 |
|---|---|---|
| `0` | 完成 | 通过 |
| `5` | 部分结果：触顶、工具失败、验收未过 | 缺省算失败；只读体检类作业可以按 `result.json` 的 `text` 自行判断 |
| `4` | 需要审批 / 策略拒绝 | 失败。提示词让它去做了 CI 里不该做的事 |
| `3` | Provider 错误 | 失败，通常是凭据、配额或网络 |
| `2` | 输入错误 | 失败，检查提示词文件与参数 |
| `1` | 配置或内部错误 | 失败 |

## 产物

样例脚本把三样东西放进 `willdeep-artifacts/`：`result.json`（`run` 的输出）、`audit.json` 与
`audit.md`（`willdeep audit export --session latest`：审批放行、人工裁决、hook 拦截、验证证据、
改动归属，见 `docs/AUDIT_EXPORT.md`）。`$WILLDEEP_HOME` 整个目录也可以归档，但它含会话正文，
公开仓库慎重。

## 成本护栏

`--max-turns`、`[agent] token_budget`、作业级 `timeout-minutes`，三个都设；用 `concurrency`
避免同一 PR 连推几次就并行跑几份。定时作业别设得比人看结果的频率还高。

## 样例

- [`examples/ci/run-task.sh`](../examples/ci/run-task.sh)：通用脚本，任何 CI 都能调
- [`examples/ci/github-actions.yml`](../examples/ci/github-actions.yml)：GitHub Actions，`workflow_dispatch` 输入提示词
- [`examples/ci/gitlab-ci.yml`](../examples/ci/gitlab-ci.yml)：GitLab CI 同一件事

样例的语法在本仓库 CI 里校验（`bash -n` 与 YAML 解析）；真跑模型不进公共 CI。

## 常见问题

- **退出码 4**：某条命令需要人审批。要么把它从任务里拿掉，要么在配置里用持久 Always Allow
  精确放行那条命令（见 `docs/APPROVALS.md`），不要为了 CI 开 `full-access`。
- **退出码 3 且 `result.json` 为空**：凭据没进环境，或 Provider 不通。`willdeep doctor` 会指出是哪一层。
- **想用 SDK 而不是 CLI**：`willdeep-runtime-client` crate 直接对接常驻 Runtime，
  见 `crates/willdeep-runtime-client/README.md`；一次性作业用 CLI 更省事。
