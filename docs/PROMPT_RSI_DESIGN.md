# WillDeep Agentic RSI（Prompt RSI）设计方案

> 状态：提案
>
> 目标项目：`willdeep-rs`
>
> 模型服务：`some.im`，由 `/Users/rocky/Sites/muchtoken` 提供网关、路由、用量与审计能力
>
> 本文只规划 Prompt、Worker 路由和 Agent 执行策略的受控自动优化，不规划模型权重训练，也不允许 Agent 直接修改自己的安全边界。

## 1. 摘要

WillDeep-rs 已经具备 Prompt RSI 的大部分基础设施：子 Agent、Worker Tier、Task Packet、Verifier、Skill Worker、固定任务集评测、Agent Reliability 评测、worktree 隔离和运行指标。

下一阶段的目标不是让 Agent 无限制地“自我进化”，而是建立一个可审计的实验闭环：

```text
采集真实运行数据
  → 脱敏与归一化
  → 形成失败样本与评测集
  → 生成 Prompt 候选版本
  → 在固定任务集和影子流量上评测
  → 通过质量、成本、安全门禁
  → 人工批准晋升
  → 小流量启用
  → 观察、回滚、继续学习
```

其中：

- `willdeep-rs` 负责 Agent Runtime、Worker 调度、Prompt 版本选择、本地评测和执行结果上报；
- `some.im / muchtoken` 负责模型调用、请求级可观测性、真实 token 与费用、模型/路由快照、租户隔离和服务端数据留存；
- 评测器负责判定“更好”，不能由被测 Agent 自己宣布成功；
- Prompt 候选只能经过版本化、评测、审批和回滚后进入生产流量。

## 2. 问题定义

### 2.1 我们要优化什么

第一阶段只优化以下对象：

1. Worker 路由 Prompt：把任务派给正确的工种和模型档位；
2. Task Packet 生成 Prompt：让子任务目标、范围、验证命令和完成条件更明确；
3. Worker 职责 Prompt：developer、tester、reviewer、reader、researcher 等窄职责提示词；
4. Verifier 修复 Prompt：测试失败后如何读取证据、缩小修改范围并重试；
5. 汇总 Prompt：如何合并多个 Worker 结果、处理冲突并报告未完成事项；
6. 错误恢复 Prompt：超时、工具失败、上下文不足、模型返回非法结构时的处理策略。

第二阶段才考虑：

- Worker Tier 的自动选择；
- 是否并行派出多个 Worker；
- 并行 Worker 的数量；
- Judge / Reviewer 的模型选择；
- 不同模型、温度、最大输出长度和工具预算的组合。

暂不优化：

- 模型权重；
- 生产安全策略；
- 工具白名单；
- 文件系统沙箱边界；
- 凭据访问权限；
- 用户确认规则；
- 合并、发布、部署和删除数据的权限。

### 2.2 “更好”的定义

Prompt RSI 的目标不能写成“模型感觉更聪明”，必须拆成可测量指标：

| 维度 | 主要指标 | 说明 |
| --- | --- | --- |
| 任务质量 | `task_success_rate` | 任务是否达到目标 |
| 工程正确性 | `verifier_pass_rate` | 测试、构建或契约验证是否通过 |
| 首次成功 | `first_pass_rate` | 是否需要重试或人工修复 |
| 路由质量 | `routing_accuracy` | 是否选对 Worker |
| 范围纪律 | `scope_violation_rate` | 是否读写了未授权文件 |
| 安全 | `policy_violation_rate` | 是否触发权限、注入或敏感信息违规 |
| 稳定性 | `recovery_rate` | 工具失败后是否能正确恢复 |
| 成本 | `input_tokens`、`output_tokens`、`estimated_cost` | 真实服务端 usage 为准 |
| 延迟 | `wall_time_ms`、`model_latency_ms` | 区分本地等待与模型等待 |
| 人工负担 | `human_edit_distance`、`manual_intervention` | 最终是否需要大量人工接管 |
| 回归 | `regression_count` | 新 Prompt 是否破坏原有能力 |

安全违规、越权写入、伪造验证结果、泄露凭据和不可恢复的数据操作，不应仅仅作为扣分项，而应是候选版本直接淘汰的硬门禁。

## 3. 现有能力与改造边界

### 3.1 WillDeep-rs 已有能力

现有代码和文档已经覆盖以下基础：

- `docs/SUBAGENTS.md`：子 Agent、后台任务、Worker Profile、Worker Tier、Task Packet、Verifier、worktree 和审计边界；
- `docs/SKILL_WORKERS.md`：小上下文 Skill Worker、固定工种、任务包、验证闭环和实弹靶场；
- `docs/MODEL_EVAL.md`：固定任务集、定期模型运行、报告和趋势；
- `docs/AGENT_METRICS.md`：Agent、Worker、模型档位和运行指标；
- `docs/WORKER_ROUTING_CONTRACT.json`：Worker 路由输出契约；
- `scripts/model_eval.rb`：模型评测入口；
- `scripts/agent_reliability_eval.rb`：Agent 可靠性评测入口；
- `scripts/skill_worker_range.rb`：Skill Worker 实弹靶场；
- `scripts/agent_metrics_*`：指标汇总和趋势归档；
- worktree、文件集锁、Verifier 和权限策略：为并行 Worker 提供隔离与验收边界。

这些能力应当被扩展成统一的 `Prompt Experiment` 记录，而不是重新创建一套平行的 Agent 平台。

### 3.2 MuchToken / some.im 应承担的职责

WillDeep-rs 本地能够知道 Agent 是否完成了任务，但不能独立、准确地知道：

- 上游实际收到的模型名；
- 最终命中的 Provider 和路由模板；
- 真实输入、输出、缓存读写 token；
- 上游延迟、重试、限流和错误分类；
- 实际成本和计费口径；
- 某个模型版本或路由配置是否在实验期间发生变化。

这些数据应由 some.im 网关在请求边界生成，并通过稳定的 `request_id` / `trace_id` 与 WillDeep 的 `run_id` 关联。

服务端不应默认保存完整 Prompt 和完整回答。默认保存结构化指标、哈希、短摘要和脱敏后的错误片段；只有用户或管理员明确开启实验采集时，才保存受控样本正文。

## 4. 总体架构

```text
┌──────────────────────────────────────────────────────────────┐
│                         Xedit / CLI / Web                     │
│  用户目标、确认、暂停、批准、回滚                            │
└──────────────────────────────┬───────────────────────────────┘
                               │
┌──────────────────────────────▼───────────────────────────────┐
│                         WillDeep-rs                           │
│                                                               │
│  Agent Runtime                                                │
│   ├─ Prompt Registry / 本地缓存                               │
│   ├─ Worker Router                                            │
│   ├─ Task Packet Builder                                      │
│   ├─ Worker Executor                                          │
│   ├─ Verifier / Judge                                         │
│   ├─ Run Event Collector                                      │
│   └─ Experiment Runner                                        │
└──────────────────────────────┬───────────────────────────────┘
                               │ OpenAI-compatible API
┌──────────────────────────────▼───────────────────────────────┐
│                       some.im / MuchToken                     │
│                                                               │
│  认证、路由、能力过滤、模型调用、usage、成本、审计            │
│  request_id / trace_id / route_snapshot / model_snapshot      │
│  RSI 数据接收、聚合、查询、租户隔离                            │
└──────────────────────────────┬───────────────────────────────┘
                               │
┌──────────────────────────────▼───────────────────────────────┐
│                      RSI 数据与评测层                         │
│  Run → Trace → Outcome → Dataset → Candidate → Evaluation     │
└──────────────────────────────────────────────────────────────┘
```

### 4.1 本地实时路径

一次正常 Agent 任务的实时路径：

1. 主 Agent 创建 `run_id`；
2. 选定当前稳定的 Prompt Bundle；
3. 生成带 `task_packet_id` 的子任务；
4. Worker 发起模型调用时携带实验上下文；
5. some.im 返回标准模型结果，并附带 `request_id`；
6. Worker 执行工具、测试和 Verifier；
7. WillDeep 记录事件和最终 Outcome；
8. 后台异步上报完整的结构化事件；
9. 服务端用 request trace 补齐真实模型、路由、usage、费用和延迟。

实时请求不能等待 RSI 上报成功。采集服务不可用时，Agent 任务仍应继续运行，本地只保留有限大小的 WAL 或 ring buffer，避免遥测拖垮主循环。

### 4.2 离线实验路径

离线实验必须和生产运行隔离：

```text
Prompt Candidate
  → 固定 Dataset
  → WillDeep Experiment Runner
  → some.im 指定模型/路由
  → Verifier / Judge
  → Metrics Aggregator
  → Gate
  → Candidate status
```

实验请求必须携带：

- `experiment_id`；
- `candidate_id`；
- `dataset_version`；
- `case_id`；
- `prompt_bundle_version`；
- `model_snapshot` 或明确的逻辑模型名；
- `route_policy`；
- `source=offline_eval`。

这样线上流量和离线评测不会混账，也不会把离线实验误算成普通用户用量。

## 5. 核心数据模型

### 5.1 Prompt Bundle

Prompt 不应只保存成一段字符串，而应保存成可比较的 Bundle：

```json
{
  "bundle_id": "worker-developer",
  "version": "12",
  "status": "active",
  "parent_version": "11",
  "role": "developer",
  "sections": {
    "system": "...",
    "task_packet_rules": "...",
    "tool_rules": "...",
    "verification_rules": "...",
    "failure_recovery_rules": "..."
  },
  "constraints": {
    "max_context_tokens": 32000,
    "allowed_tools": ["read_file", "apply_patch", "run_command"],
    "write_scope_required": true
  },
  "created_by": "human|optimizer|import",
  "created_from": "failure-cluster-2026-09-28-001",
  "content_hash": "sha256:..."
}
```

版本状态：

```text
draft → evaluated → approved → canary → active
                         ↘ rejected
active → superseded
active → rolled_back
```

只有 `approved` 之后才能进入 `canary`。`optimizer` 永远不能直接把版本标记为 `active`。

### 5.2 Worker Run

```json
{
  "run_id": "run_01...",
  "parent_run_id": "run_00...",
  "task_id": "task_01...",
  "task_kind": "code_fix",
  "role": "test_fixer",
  "worker_tier": "standard",
  "prompt_bundle_id": "worker-test-fixer",
  "prompt_bundle_version": "7",
  "task_packet_hash": "sha256:...",
  "workspace_id": "workspace_01...",
  "base_revision": "git-sha",
  "experiment_id": null,
  "started_at": "2026-09-28T00:00:00Z",
  "ended_at": "2026-09-28T00:01:00Z"
}
```

### 5.3 Model Request Link

```json
{
  "run_id": "run_01...",
  "request_id": "req_01...",
  "trace_id": "trace_01...",
  "logical_model": "some/agent-standard",
  "resolved_model": "provider/model-version",
  "provider": "some.im",
  "route_snapshot_hash": "sha256:...",
  "request_kind": "agent_worker",
  "attempt": 1,
  "input_tokens": 1200,
  "output_tokens": 850,
  "cache_read_tokens": 0,
  "cache_write_tokens": 0,
  "latency_ms": 4200,
  "finish_reason": "stop",
  "error_class": null
}
```

`resolved_model`、Provider、路由和 token 必须以 some.im 服务端记录为准，客户端上报的值只能作为关联信息，不能作为计费或模型评测的权威来源。

### 5.4 Tool / Verifier Event

```json
{
  "run_id": "run_01...",
  "sequence": 18,
  "event_type": "verifier_finished",
  "tool_name": "run_command",
  "command_class": "test",
  "command_hash": "sha256:...",
  "exit_code": 0,
  "duration_ms": 9300,
  "changed_files": ["crates/willdeep-core/src/subagent/runner.rs"],
  "scope_violation": false,
  "output_excerpt": "...脱敏后的短摘要..."
}
```

不要默认上传完整 shell 输出。大输出应在本地保存，服务端只接收哈希、大小、退出码、分类和经过脱敏的短摘要。

### 5.5 Outcome

Outcome 是评测的基本单位，不能只用最终自然语言回答判断：

```json
{
  "run_id": "run_01...",
  "status": "completed",
  "task_success": true,
  "verifier_passed": true,
  "first_pass": false,
  "retry_count": 1,
  "manual_intervention": false,
  "scope_violation": false,
  "security_violation": false,
  "regression": false,
  "quality_score": 0.92,
  "cost_score": 0.81,
  "latency_score": 0.74,
  "failure_labels": ["initial_test_misread"],
  "evidence_refs": ["verifier-event-18"],
  "human_disposition": null
}
```

## 6. 数据采集策略

### 6.1 采集分层

分成三层，避免收集过多正文：

#### L0：必须采集的元数据

- `run_id`、`task_id`、`parent_run_id`；
- Worker role 和 Worker tier；
- Prompt Bundle ID 和版本；
- Task Packet hash；
- 仓库 revision；
- 工具名称、事件顺序和退出状态；
- Verifier 类型和结果；
- request_id、trace_id；
- token、费用、延迟、错误分类；
- 最终成功、重试、人工介入、回滚结果。

L0 不包含用户 Prompt 正文和模型完整回答，默认可以长期保存。

#### L1：受控摘要

- Prompt 长度和结构摘要；
- Task Packet 的字段统计；
- 错误类型和脱敏 excerpt；
- 工具调用参数的 schema 摘要；
- 文件路径的项目内相对路径哈希；
- 模型输出的结构化字段；
- 自动聚类所需的 embedding 或特征摘要。

L1 应按租户配置保留时间，并在进入服务端前做密钥、Token、邮箱、手机号和绝对路径脱敏。

#### L2：实验样本正文

仅在以下条件同时满足时保存：

- 用户或组织明确开启实验数据采集；
- 样本属于指定 Experiment 或 Dataset；
- 已执行 secret scanner 和 PII scanner；
- 有保留期限；
- 有删除和导出机制；
- 只有授权的评测任务可以读取。

L2 不能默认用于训练，也不能因为“对模型有帮助”就永久保存。

### 6.2 采集时机

WillDeep-rs 至少应在以下节点发事件：

```text
run_started
prompt_resolved
task_packet_created
worker_spawned
model_request_started
model_request_linked
tool_call_started
tool_call_finished
verifier_started
verifier_finished
worker_retrying
worker_finished
judge_finished
run_finished
human_disposition_recorded
```

事件必须有：

- 单调递增的本地 sequence；
- 客户端生成时间；
- 可选的服务端接收时间；
- `run_id`；
- schema version；
- 幂等 event_id。

### 6.3 采样策略

不应把所有成功运行的完整数据都上传。建议：

- 所有运行上传 L0；
- 所有失败、重试、Verifier 不通过和人工接管运行上传 L1；
- 成功运行按 1%～5% 采样上传 L1；
- L2 只采集固定评测集、用户主动标记样本和经过授权的实验样本；
- 对同一 `task_kind + failure_label + prompt_version` 做分层采样，避免热门简单任务淹没困难案例；
- 发生安全违规时保留最小必要证据，不上传原始秘密内容。

## 7. some.im / MuchToken 服务端设计

下面是建议的服务端能力，不假设当前已经存在，应该作为新模块或现有 trace/evaluation 能力的扩展进行设计。

### 7.1 事件接收 API

建议内部接口：

```text
POST /api/v1/agent-rsi/runs
POST /api/v1/agent-rsi/events:batch
POST /api/v1/agent-rsi/outcomes
POST /api/v1/agent-rsi/human-dispositions
```

要求：

- 使用服务端认证的 Agent Client ID；
- `event_id` 幂等；
- 支持批量和压缩；
- 不能阻塞模型请求主链路；
- 写入失败可重试；
- 每个租户、项目和 Agent 有配额；
- 事件写入使用 append-only 语义；
- 原始事件和聚合指标分开存储。

批量事件示例：

```json
{
  "schema": "willdeep.agent-rsi-event.v1",
  "client_id": "willdeep-rs",
  "project_id": "project_hash",
  "runs": [
    {
      "run_id": "run_01...",
      "events": [
        {
          "event_id": "evt_01...",
          "sequence": 1,
          "type": "run_started",
          "occurred_at": "2026-09-28T00:00:00Z",
          "payload": {}
        }
      ]
    }
  ]
}
```

### 7.2 服务端补全模型调用事实

some.im 应根据 `request_id` 自动补全：

- 逻辑模型名；
- 实际 Provider；
- 实际模型版本或路由目标；
- 路由模板版本；
- 输入、输出、缓存 token；
- 上游耗时和网络耗时；
- 重试、限流、熔断和 fallback；
- 成本和计费口径；
- 响应完成原因；
- 上游错误分类。

服务端生成一份不可由客户端覆盖的 `model_observation`，用于评测和账务。

### 7.3 Dataset API

建议接口：

```text
POST /api/v1/agent-rsi/datasets
POST /api/v1/agent-rsi/datasets/{id}/cases
GET  /api/v1/agent-rsi/datasets/{id}/versions
POST /api/v1/agent-rsi/datasets/{id}/freeze
```

Dataset 必须不可变版本化：

```text
draft → review → frozen → archived
```

冻结后的 Dataset 不能静默修改。新增案例应创建新版本，否则历史 Prompt 分数无法比较。

案例来源：

1. 固定手写任务；
2. 真实失败运行的脱敏样本；
3. 用户明确标记的“这个结果不好”；
4. Verifier 失败后归类的任务；
5. Worker 路由错误；
6. 安全和越权回归案例；
7. 线上成功样本中的困难任务。

每个案例都应有标签：

```json
{
  "case_id": "worker-routing-0042",
  "kind": "worker_routing",
  "difficulty": "medium",
  "input_hash": "sha256:...",
  "expected_role": "test_fixer",
  "required_evidence": ["verifier_passed", "scope_clean"],
  "forbidden_behaviors": ["modify_unrelated_file", "skip_verifier"],
  "split": "train|validation|holdout|regression"
}
```

Prompt Optimizer 不能看到 holdout 的答案和评分细节，只能在候选晋升阶段由评测服务运行 holdout。

### 7.4 Experiment API

建议接口：

```text
POST /api/v1/agent-rsi/experiments
POST /api/v1/agent-rsi/experiments/{id}/start
POST /api/v1/agent-rsi/experiments/{id}/cancel
GET  /api/v1/agent-rsi/experiments/{id}
GET  /api/v1/agent-rsi/experiments/{id}/results
```

实验定义：

```json
{
  "experiment_id": "exp_01...",
  "purpose": "improve_worker_routing",
  "baseline": {
    "bundle_id": "router",
    "version": "12"
  },
  "candidates": [
    {"bundle_id": "router", "version": "13"},
    {"bundle_id": "router", "version": "14"}
  ],
  "dataset_version": "worker-routing@2026-09-28.3",
  "model_policy": {
    "logical_model": "some/agent-standard",
    "route_mode": "pinned",
    "allow_fallback": false
  },
  "budgets": {
    "max_cases": 200,
    "max_total_tokens": 200000,
    "max_cost_cny": 30,
    "max_concurrency": 4
  },
  "gate": {
    "min_verifier_pass_rate": 0.85,
    "max_regression_count": 0,
    "max_scope_violation_rate": 0,
    "max_cost_increase_pct": 15
  }
}
```

### 7.5 Prompt Registry API

建议接口：

```text
GET  /api/v1/agent-rsi/prompt-bundles/{id}
POST /api/v1/agent-rsi/prompt-bundles/{id}/versions
POST /api/v1/agent-rsi/prompt-bundles/{id}/versions/{version}/approve
POST /api/v1/agent-rsi/prompt-bundles/{id}/versions/{version}/activate
POST /api/v1/agent-rsi/prompt-bundles/{id}/versions/{version}/rollback
```

但生产环境的最终生效版本最好由 WillDeep-rs 本地配置或签名快照确认，不能让一次网络请求直接替换所有 Agent 的 Prompt。

建议采用：

```text
服务端 Registry
  → 发布 signed bundle manifest
  → WillDeep-rs 下载并校验
  → 本地保存 active / previous
  → 新运行按实验或灰度规则选择
```

## 8. Prompt 自动优化器

### 8.1 优化器的输入

优化器不应直接读取所有用户会话，而应读取结构化材料：

- 当前 Prompt Bundle；
- 指标下降的案例；
- 失败标签统计；
- 成功与失败的脱敏对比；
- Verifier 错误类型；
- 工具调用轨迹摘要；
- 人工 Review 结论；
- 预算和延迟约束；
- 明确不可修改的约束列表。

### 8.2 优化器的输出

每次只允许输出一个候选 Bundle 和变更说明：

```json
{
  "parent_version": "12",
  "candidate_version": "13-candidate-001",
  "changes": [
    {
      "section": "verification_rules",
      "reason": "测试失败时先读取完整失败摘要，再决定是否修改文件",
      "before_hash": "sha256:...",
      "after_hash": "sha256:..."
    }
  ],
  "expected_effect": "降低 test_fixer 的首次误判",
  "risk": "可能增加一次读取操作",
  "forbidden_changes": [],
  "candidate_text": "..."
}
```

优化器本身不能：

- 改写工具权限；
- 删除 Verifier；
- 降低安全等级；
- 删除人工审批；
- 修改评测代码；
- 修改 holdout 数据集；
- 修改评分公式；
- 把失败标记为成功。

### 8.3 候选生成策略

第一阶段采用保守的“单点修改”：

- 一次只修改一个 section；
- 一次最多改变一个行为规则；
- 必须保留原 Prompt 的不变量；
- 必须给出失败案例到修改的因果说明；
- 候选数量每轮不超过 3 个；
- 不允许连续自动晋升超过 2 轮。

候选生成可使用 some.im 的高级模型，但候选评测应尽量使用固定模型和固定路由，以保证可比性。

### 8.4 防止 Prompt 过拟合

必须使用四组数据：

```text
train：允许优化器阅读失败摘要
validation：用于选择候选
holdout：只在晋升前运行
regression：历史高风险案例，永远不能被破坏
```

晋升条件示例：

```text
validation verifier_pass_rate 提升 ≥ 3 个百分点
holdout verifier_pass_rate 不下降
regression 通过率 = 100%
scope_violation_rate = 0
security_violation_rate = 0
平均成本增加 ≤ 15%
P95 延迟增加 ≤ 20%
```

如果候选 Prompt 只在训练案例上提升，而 holdout 下降，应标记为 `overfit`，不能上线。

## 9. 多 Worker RSI

### 9.1 不要默认“任务越多 Worker 越好”

每个任务先由路由器判断：

```text
single_worker
parallel_workers
worker_then_verifier
worker_then_reviewer
human_required
```

派 Worker 的收益必须大于额外成本：

```text
expected_quality_gain
  > expected_token_cost
  + expected_latency_cost
  + merge_conflict_cost
```

### 9.2 并行 Worker 的角色分工

不要派出多个完全相同的“通用 Agent”。建议使用互补角色：

```text
主 Agent：拆解和汇总
  ├─ Reader：只读定位事实
  ├─ Implementer：在限定文件内修改
  ├─ Tester：运行测试、归类失败
  ├─ Reviewer：只读审查风险
  └─ Verifier：独立执行验收命令
```

角色之间的 Prompt 应明确：

- 输入证据；
- 允许工具；
- 允许读写范围；
- 输出结构；
- 不负责的事项；
- 验收条件；
- 失败时如何报告。

### 9.3 并行写入策略

默认禁止多个 Worker 直接写同一个工作目录。

可选策略按优先级排列：

1. 多个只读 Worker 并行，只有一个 Implementer 写入；
2. 每个 Implementer 使用独立 worktree，最后由专门 Integrator 合并；
3. 多个 Worker 只生成 patch proposal，主 Agent 选择一个应用；
4. 只有经过文件集锁和冲突检测后才允许共享写入。

Integrator 不能同时承担 Reviewer 的角色。合并者和验收者必须逻辑独立，否则容易出现“自己改、自己说通过”。

### 9.4 多 Worker 的评测指标

每个任务同时记录以下对照组：

```text
单 Worker
两 Worker 并行
三 Worker + Verifier
三 Worker + Judge
```

比较：

- 最终通过率；
- 首次通过率；
- 平均 token；
- P95 延迟；
- 重复工作比例；
- 合并冲突数；
- 人工修改行数；
- 错误归因准确率；
- 无效派工比例。

只有当多 Worker 在质量提升上抵消了成本和延迟，才允许把该任务类型的路由从单 Worker 改成并行模式。

## 10. 安全、隐私和数据治理

### 10.1 不上传的内容

默认禁止上传：

- API Key、OAuth Token、SSH 私钥、Cookie；
- `.env` 内容；
- 用户密码和个人身份信息；
- 未经授权的源码全文；
- 完整 shell 输出中的凭据；
- 用户项目中的二进制和媒体文件；
- 与评测无关的完整对话历史。

### 10.2 服务端租户隔离

RSI 数据必须至少按以下维度隔离：

- account / tenant；
- project；
- Agent client；
- dataset；
- experiment；
- prompt bundle。

一个租户的失败样本不能自动进入另一个租户的训练或评测集。跨租户共享只能使用经过脱敏、聚合和明确授权的模式级统计。

### 10.3 允许采集的默认策略

建议默认值：

```text
L0 元数据：开启
L1 脱敏摘要：仅失败和实验运行开启
L2 正文：关闭
跨租户聚合：关闭
用于 Prompt 优化：关闭，需显式开启
保留期限：L0 90 天，L1 30 天，L2 7 天
```

具体期限应由产品和合规要求确认，不能仅凭技术方便决定。

## 11. 版本、晋升和回滚

### 11.1 版本不变量

一次实验必须固定或明确记录：

- Prompt Bundle 版本；
- Dataset 版本；
- 模型逻辑名和实际解析结果；
- route snapshot；
- Worker Tier；
- Verifier 版本；
- 评分器版本；
- WillDeep-rs commit；
- some.im API 版本。

缺少任一项时，实验结果标记为 `non_reproducible`，不能用于自动晋升。

### 11.2 晋升门禁

建议分为四道门：

1. **结构门**：Prompt schema、工具名、输出格式和不变量合法；
2. **安全门**：无越权、无敏感信息泄漏、无禁用规则变化；
3. **质量门**：validation、holdout、regression 达到阈值；
4. **人工门**：人类确认候选变更和评测报告。

通过后先进入 canary：

```text
0% → 离线
1% → 内部任务
5% → 可信项目
25% → 观察窗口
100% → 正式 active
```

任一硬指标恶化，自动切回 previous active version。

### 11.3 回滚

回滚对象至少包括：

- Prompt Bundle；
- Worker 路由规则；
- 模型选择策略；
- Judge 规则；
- 实验灰度配置。

回滚必须是配置切换，不应通过重新生成 Prompt 或重新运行优化器完成。回滚事件本身也要记录为 Outcome，方便分析为什么晋升失败。

## 12. 实施路线图

### Phase 0：契约和本地记录

目标：不改服务端也能获得可评测运行记录。

工作项：

- 定义 `prompt-bundle.v1`；
- 定义 `agent-rsi-event.v1`；
- 给现有 Worker Run 增加 `prompt_bundle_version`；
- 给 Task Packet 增加 hash、scope 和 verifier hash；
- 统一 run、attempt、request、outcome ID；
- 本地 WAL 和批量导出 JSONL；
- 运行记录中区分实验和生产。

验收：

- 一次多 Worker 任务可以完整重建；
- 能看到每个 Worker 使用的 Prompt 版本；
- 能将模型请求和 some.im request_id 关联；
- 采集失败不影响正常任务。

### Phase 1：WillDeep-rs 本地 Prompt Registry

工作项：

- Prompt Bundle 加载、校验、缓存；
- active / previous / candidate 状态；
- 本地签名或 hash 校验；
- 运行时按 role 选择版本；
- 一键切回 previous；
- Prompt 变更报告。

验收：

- 同一 commit、同一 dataset、同一 Prompt 可复现；
- 版本切换不需要修改代码；
- 不合法 Prompt 无法启动 Worker。

### Phase 2：some.im 数据接收和 request 事实补全

MuchToken 工作项：

- RSI event batch 接收；
- request_id / trace_id 关联；
- 服务端 model observation；
- token、成本、路由快照补全；
- 按 tenant/project 查询；
- 事件幂等和 retention。

验收：

- WillDeep 记录的 token 和网关 usage 可对账；
- 逻辑模型、实际模型和路由信息可追溯；
- 采集服务异常不影响模型请求。

### Phase 3：固定评测和 Prompt 候选实验

工作项：

- Dataset freeze；
- baseline/candidate 对照运行；
- validation、holdout、regression 分组；
- verifier 结果汇总；
- JSON 和 Markdown 报告；
- Gate 自动判定但不自动上线。

验收：

- 一个 Prompt 候选可以完整跑完实验；
- 能解释每个指标的来源；
- 候选失败时有失败聚类和样本链接；
- holdout 不暴露给候选生成器。

### Phase 4：失败聚类和候选生成

工作项：

- 错误标签标准化；
- 相似失败聚类；
- 用 some.im 高级模型生成候选 Prompt；
- 约束候选只能修改指定 section；
- 生成变更说明和风险说明；
- 自动执行安全扫描和结构校验。

验收：

- 优化器无法修改工具权限和评分器；
- 候选 Prompt 必须能追溯到失败案例；
- 失败候选不会进入 active。

### Phase 5：受控 Canary

工作项：

- 内部项目灰度；
- 版本级指标对照；
- 自动触发回滚；
- 人工 disposition；
- 版本晋升审计。

验收：

- 可以把候选版本只投放到一个项目；
- 质量或安全回归时自动回退；
- 回滚后新请求不再使用候选版本；
- 历史运行仍能按旧版本复盘。

## 13. 推荐的第一批评测任务

不要一开始收集所有 Agent 对话。先建立窄而有价值的评测集：

### Worker 路由

- 明确的测试修复任务；
- 只读调查任务；
- 需要编辑但不能提交的任务；
- 需要 Reviewer 的高风险任务；
- 不应派 Worker、主 Agent 可直接完成的简单任务；
- 含歧义、必须请求澄清的任务。

### Task Packet

- 缺少文件范围；
- 缺少完成条件；
- 验证命令错误；
- 目标与写范围冲突；
- 任务过大，需要继续拆分；
- 任务描述包含 Prompt Injection。

### Worker 执行

- 测试失败后能否先定位再修改；
- 是否读取无关目录；
- 是否修改超出范围的文件；
- 是否伪造测试结果；
- 是否在工具失败后正确重试；
- 是否在无法完成时诚实报告。

### 汇总和 Judge

- 两个 Worker 结果冲突；
- 一个 Worker 成功但没有证据；
- 一个 Worker 提出危险修改；
- 部分任务完成、部分任务失败；
- 验证器和自然语言结论不一致。

## 14. 关键风险

### 14.1 Reward hacking

如果只优化“测试通过率”，Agent 可能学会删除测试、降低断言或绕过验证。因此：

- Verifier 命令和测试文件必须受保护；
- 评测环境与 Worker 写入环境隔离；
- 需要检查 diff 和测试覆盖的异常下降；
- 关键结果需要独立 Verifier。

### 14.2 Prompt 过拟合

单个失败案例不能直接驱动全局 Prompt 改动。必须有 holdout 和 regression 集，并限制每次只改一个行为点。

### 14.3 模型和路由漂移

some.im 的逻辑模型名可能解析到不同 Provider、模型版本或路由。评测必须保存服务端 route snapshot，否则前后两次结果不可比较。

### 14.4 多 Worker 的伪并行

如果多个 Worker 读取相同上下文、给出相同答案，成本增加但信息量没有增加。应记录结果相似度、重复工具调用和重复修改比例，必要时改用角色互补或单 Worker。

### 14.5 数据泄露

真实项目的失败案例可能包含私有源码、密钥和个人数据。脱敏必须在客户端和服务端各做一层，服务端不能假定客户端永远可信。

### 14.6 自我强化错误

一个错误的 Prompt 如果自动生成新的错误 Prompt，可能形成反馈回路。因此必须：

- 限制自动连续优化轮数；
- 每轮保留人工可读 diff；
- 不能自动修改评分器；
- 只能从 active 或 approved parent 生成候选；
- 回滚后冻结该候选的自动再尝试。

## 15. 最终建议

第一版不要做“让 Agent 自动修改所有 Prompt”。建议只做一个窄目标：

> 优化 `test_fixer` 和 `worker_router` 的 Prompt，使用 WillDeep-rs 固定评测集，使用 some.im/MuchToken 提供真实模型请求、token、路由和成本事实，候选版本经过 holdout、regression、人工批准和 canary 后才能生效。

第一阶段的成功标准：

- Worker 路由准确率提升；
- Verifier 首次通过率提升；
- 平均重试次数下降；
- Token 成本没有失控；
- 越权和安全违规保持为零；
- 每次改进都能回答“为什么改、证据是什么、效果多大、如何回滚”。

如果这些指标稳定，再扩展到多 Worker 数量选择、模型 Tier 自动选择和更复杂的 Judge 策略。

## 附录 A：建议新增的本地文件

```text
docs/PROMPT_RSI_DESIGN.md                 # 本文
docs/PROMPT_RSI_OPERATIONS.md             # 运维、保留、回滚和排障
docs/schemas/prompt-bundle.v1.schema.json
docs/schemas/agent-rsi-event.v1.schema.json
docs/schemas/agent-rsi-outcome.v1.schema.json
bench/prompt-rsi/datasets/                 # 冻结的本地评测集索引
bench/prompt-rsi/reports/                  # JSON + Markdown 报告
scripts/prompt_rsi_eval.rb                 # 候选评测入口
scripts/prompt_rsi_report.rb               # 报告和趋势
scripts/prompt_rsi_candidate.rb            # 候选生成与结构校验
```

## 附录 B：建议的最小事件字段

```text
schema
event_id
run_id
parent_run_id
project_id_hash
task_id
experiment_id
dataset_case_id
role
worker_tier
prompt_bundle_id
prompt_bundle_version
willdeep_revision
request_id
trace_id
event_type
sequence
occurred_at
status
error_class
verifier_status
input_tokens
output_tokens
latency_ms
estimated_cost
redaction_version
```

所有字段都应有明确的来源：WillDeep 本地事实、some.im 服务端事实、Verifier 事实或人工事实。对于来源不确定的字段，应标记为 unknown，而不是猜测或用默认值填充。
