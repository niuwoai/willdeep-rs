# 双端共享模型目录（model-catalog.v1）

> 状态：2026-08-26 canonical 草案。适用于 WillDeep macOS App（Xedit）与
> willdeep-rs。机器可读契约见
> [`schemas/model-catalog.v1.schema.json`](schemas/model-catalog.v1.schema.json)，
> 示例见 [`examples/model-catalog.v1.json`](examples/model-catalog.v1.json)。

## 一句话结论

两端共享同一份**非敏感模型目录**，目录只保存 Provider、模型事实、路由候选和
`credential_ref`；真实凭据留在系统凭据存储中，由 Provider 调用边界解析，永不进入
JSON、TOML、会话、遥测、日志或模型上下文。

共享目录解决的是「两端是否在谈论同一个 Provider、同一个模型、同一个窗口和同一组
路由候选」；凭据解析解决的是「实际调用时从哪里安全取 Key」。这两个问题相关，但不能
用一份带明文 Key 的大配置文件粗暴合并。

## 现状与必须保留的经验

### Xedit 已有能力

- `AgentProviderLibrary` 保存多个 Provider 实例，每个实例有独立 Base URL、Flash/Pro
  模型和 UUID；非敏感骨架在 UserDefaults，API Key 在 Data Protection Keychain。
- some.im 的 `/api/v1/public/model-pricing` 已提供 `context_tokens_max`、能力类型、输入/
  输出模态、标签和价格字段；这些 Provider 上报事实应优先于客户端按模型名猜测。
- `/models` 只适合发现普通模型 ID，**不包含所有虚拟模型链**。`someim-32b-*`、
  compressor、judge、安全模型等必须允许由受管目录显式声明，不能因为列表里没有就删掉。
- Xedit 会把 Provider 上报的上下文窗口合并进进程目录，并让它覆盖内置名称匹配表。

### willdeep-rs 已有能力

- `$WILLDEEP_HOME/config.toml` 已由两端共享部分字段；`[agent]` 路由开关由 CLI 管理、
  App 只读。
- S/M/L、Task Packet、Verifier、Deep 票据和按工种模型绑定已经落地。
- `/routing` 可以改 Root、Worker、Deep 的 Provider、模型和上下文，但当前每个 profile
  仍主要绑定一个静态模型。
- Provider 凭据当前依次来自命令行/环境变量、`api_key`、`api_key_env` 和 Provider
  专属环境变量；CLI 还没有跨平台系统凭据抽象。

### 当前漂移

- 两端对同一模型窗口仍可能给出不同值。例如 rs 的 M 档按 256K 设计，而 Xedit 当前
  对裸 `glm-5` 的内置回退是 200K。目录必须让 Provider 实测值或用户覆盖成为单一事实，
  不能继续让两张硬编码表各自有理。
- Xedit 的 Provider 实例以 UUID 区分，rs 的 Provider profile 以 TOML table 名区分；
  仅用 `providerID = some-im` 无法区分公司内网、个人账号和直连上游。
- Xedit Keychain 与 rs 环境变量/明文 TOML 是两套凭据来源。直接把 Xedit 的 UserDefaults
  导出给 CLI 只会得到没有 Key 的空壳；直接把 CLI 的 TOML 导给 App 又会把明文密钥扩散。

## 文件与所有权

默认路径：

```text
$WILLDEEP_HOME/model-catalog.v1.json
```

未设置 `WILLDEEP_HOME` 时为：

```text
~/.willdeep/model-catalog.v1.json
```

规则：

1. 文件权限固定为 `0600`。目录虽然不含 Key，但可能含私有 Base URL、内部模型名和价格。
2. 写入使用同目录临时文件、`fsync`、原子 `rename`；禁止原地截断写。
3. `revision` 是不透明 CAS 标识。编辑器读取 revision，保存时不一致就拒绝并要求重载。
4. 双端写入前取得 `$WILLDEEP_HOME/model-catalog.v1.lock`；锁只保护一次读取—合并—替换。
5. `schema_version` 不认识时只读失败并回落旧配置，**不得拿旧解码器回写新文件**。
6. v1 的字段集冻结；未来新增结构字段使用 v2。Provider/模型的厂商扩展只能放进
   `extensions`，避免旧客户端把新字段当拼写错误；扩展字段仍不得携带 Key、Token、
   Password 或 Secret，解析器会递归拒绝具有这类名称的字段。

## 数据模型

### Provider 实例

Provider 的 `id` 标识一套真实调用端点，不是厂商名：

```json
{
  "id": "some-im-main",
  "display_name": "some.im",
  "provider_kind": "some-im",
  "api_dialect": "chat-completions",
  "base_url": "https://some.im/v1",
  "network_scope": "public",
  "credential_ref": "credential:provider/some-im-main",
  "credential_env": "SOMEIM_API_KEY",
  "supports_model_discovery": true,
  "enabled": true,
  "extensions": {}
}
```

同一厂商的公司内网代理和公网账号必须是两个 Provider 实例，因为它们的凭据、价格、
可出网策略和可用模型都可能不同。

Base URL 必须是带 authority 的 HTTP(S) URI，且不得含 `user:password@host` 形式的
user-info；认证信息只能经 `credential_ref` 解析。

`network_scope` 是硬约束：

| 值 | 含义 |
|---|---|
| `local` | 本机回环、Unix socket 或同进程推理 |
| `lan` | 局域网/机房内网，不经过公网 |
| `private_cloud` | 企业专有云或受控专线 |
| `public` | 公网服务 |

### 模型事实

模型身份是 `(provider_id, model)`，不能只看模型名。some.im 与 DeepSeek 直连都可能
提供 `deepseek-v4-flash`，但二者的凭据、价格、限流和信任边界不是一回事。

核心字段：

- `id`：目录内稳定标识，路由 profile 引用它；
- `provider_id`：调用端点；
- `model`：发给 Provider 的真实 request model；
- `kind`：`physical`、`virtual` 或 `local_alias`；
- `context_window_tokens`：Provider 上报或用户确认的真实窗口；`null` 表示未知；
- `max_output_tokens`：未知可为 `null`；
- `capabilities`：工具调用、编码、视觉、结构化输出等；
- `input_modalities` / `output_modalities`；
- `pricing`：可选，价格使用十进制字符串避免 Swift/Rust 浮点漂移；
- `metadata_source`：`provider`、`user`、`managed`、`bundled`；
- `observed_at`：动态事实最后确认时间；
- `billing_model_id`：虚拟模型与计费逻辑模型不一致时使用，不能据此推断上游。

`context_window_tokens = null` 时的纪律：

1. 状态栏与普通短任务可使用 32K 保守窗口；
2. 该模型不得因为名字像 `1m` 就自动承担 context-critical 路由；
3. Provider 上报、用户覆盖或靶场确认后再提升；
4. 不把 `null` 当 `0`，也不让缺字段覆盖已知值。

### 路由策略与 profile 候选池

目录不再保存「工种唯一模型」，而是候选池：

```json
{
  "id": "test_fixer",
  "required_capabilities": ["tool_calling", "coding"],
  "candidate_model_ids": ["local-qwen-worker", "some-im-glm-standard"],
  "context_utilization_limit": 0.6,
  "max_same_tier_retries": 1,
  "escalates_to": "implementer"
}
```

选择顺序：

1. 先按网络域、凭据是否可用、能力、模态、上下文和健康状态做硬过滤；
2. 能消化/分片就先整形上下文，不因原始材料大直接上 L；
3. 在剩余候选中选择达到质量门槛的最低预期成功总成本；
4. 没有可靠观测时按 `candidate_model_ids` 顺序；
5. 超时/限流只允许同档换端点；Verifier 失败才允许带证据升能力档；
6. 公网候选被任务数据策略拒绝时，动态评分无权把它加回来。

`context_utilization_limit` 作用于完整请求估算：系统提示词、工具 Schema、Task Packet、
材料、预期工具回传和输出余量都算，不能只数用户消息。

### 策略预设

v1 允许多个策略，至少保留：

- `sovereign`：仅 `local` / `lan` / `private_cloud`；
- `balanced`：允许公网，但显著惩罚公网暴露和失败升级；
- `economy`：成本权重更高，但质量门槛不降低；
- `fast`：延迟权重更高；
- `quality`：失败风险权重更高，仍受网络和数据硬约束。

这些名字只是权重预设，不是另一套模型档位。S/M/L 与 profile 语义保持不变。

## 事实来源与优先级

逐字段优先级固定为：

```text
用户显式覆盖
  > Provider/受管网关上报
  > 双端内置已验证目录
  > 模型名启发式猜测
```

注意：`/models` 返回「这个 ID 可以列出来」，不等于模型支持工具、拥有某窗口或价格已知；
虚拟模型也可能可调用但不在列表中。目录合并时：

- 没上报的字段不覆盖旧事实；
- 上报 `0` 的窗口按未知处理；
- Provider 删除普通模型可标 `enabled = false`，但不能自动删除 `kind = virtual` 的受管项；
- 上游物理模型变化不改虚拟模型身份，客户端只依赖请求模型和能力契约。

## 凭据契约（credential-ref.v1）

### 目录里允许出现什么

```json
{
  "credential_ref": "credential:provider/some-im-main",
  "credential_env": "SOMEIM_API_KEY"
}
```

- `credential_ref` 是稳定逻辑引用，不是 Keychain service/account 的实现细节；
- `credential_env` 是无系统凭据后端时的显式回退，环境变量名不是秘密；
- 本地无需认证的 Provider 可把两者都设为 `null`；
- schema 没有 `api_key`、`token`、`secret` 字段，避免「大家自觉别填」这种纸门禁。

### 解析优先级

实际 Provider 调用边界按序解析：

```text
本次调用显式凭据
  > 系统凭据存储中的 credential_ref
  > credential_env 指向的环境变量
  > 旧版 config.toml 明文/Provider 专属环境变量（兼容期）
```

解析结果只进入 Provider Client 内存，不进入 Agent 消息、Tool 结果、遥测和错误正文。

### macOS 的真实共享

目标存储：

```text
Keychain access group: ZH2S7D6PL6.com.willdeep.app
service:               com.willdeep.providers
account:               credential_ref 原文
```

Xedit 已使用该 Data Protection Keychain access group。发布版 `willdeep` CLI 若要直接
读取同一条凭据，必须由同一 Team 签名并携带相同 Keychain entitlement；源码自编译、
临时 ad-hoc 签名或第三方分发的 CLI **不能假装能够访问**，应明确回落环境变量。

禁止让 CLI 通过 `security find-generic-password` 或一个任意进程都能调用、把 Key 打到
stdout 的 helper 绕过 entitlement。那会把「共享」做成「同用户下任何进程可导出」。

### Linux 与 Windows

- Linux 桌面优先 Secret Service；无会话总线的服务器使用环境变量或外部 secret manager；
- Windows 使用 Credential Manager；
- 两端共享的是 `credential_ref` 与解析接口，不要求所有平台使用同一种物理后端；
- 后端不可用必须显式报 `credential_unavailable`，不能静默换到另一个公网 Provider。

### 长期形态：共享调用面而非传 Key

中期路线仍是 willdeep-rs Runtime 成为唯一 Harness。Xedit 通过权限为 `0600` 的 Unix
socket 提交 Provider 请求，Runtime 持有凭据并返回模型事件；App 不需要取得 Key，CLI
也不需要从 App 导出 Key。该形态比「两个进程轮流解密同一 secret」更容易审计。

在 Runtime 接管前，签名版 CLI 与 Xedit 可以直接共享 Keychain；接管后
`credential_ref` 保持不变，只替换 resolver 为 Runtime broker。

## 迁移

### Xedit

1. 以 `AgentProviderEntry.id` 的 UUID 作为共享 Provider ID，避免同厂商多个实例碰撞；
2. 把 UserDefaults 中的非敏感骨架写入共享目录；
3. 在 Keychain 内把 `com.willdeep.agent / library-entry-<UUID>` 复制到
   `com.willdeep.providers / credential:provider/<UUID>`；
4. 旧 Keychain row 至少保留两个正式版本，只读回退，不双写；
5. 共享目录存在且有效后，它是 canonical；UserDefaults 只做兼容镜像，不反向覆盖。

### willdeep-rs

1. `[providers.*]` 增加可选 `catalog_provider_id`，逐步停止重复保存 Base URL、模型和 Key；
2. `api_key_env` 迁移成 `credential_env` 只需复制变量名，不读取真实值；
3. 明文 `api_key` 只有在用户显式执行凭据迁移时才写入系统存储，并原子删除 TOML 原值；
4. 未迁移的旧配置继续可用，但绝不自动把明文 Key 复制进共享目录；
5. `[subagents.*]` 逐步从单一 `provider_profile/model` 改为共享 profile 候选池，显式覆盖仍优先。

### 冲突处理

- 有效共享目录 > Xedit UserDefaults / rs catalog 引用对应的旧 Provider 骨架；
- 会话显式模型覆盖 > 共享 profile；
- `--provider` / `--model` / `--api-key` 只影响本次 Harness，不改共享目录；
- 旧配置与共享目录同时修改同一 Provider 时不按时间戳猜输赢，CAS 冲突直接要求用户确认。

## 运行观测不写回静态目录

价格、能力和用户覆盖属于目录；延迟、成功率、重试次数和健康状态是观测。二者分开：

```text
$WILLDEEP_HOME/model-observations/xedit.jsonl
$WILLDEEP_HOME/model-observations/willdeep-rs.jsonl
```

每端只追加自己的 0600 文件，避免两个进程抢写同一 JSONL。路由器读取并合并有限字段：

- Provider/model/profile；
- 输入/输出 Token；
- 首次/最终 Verifier 结果；
- 尝试次数；
- 耗时与结构化错误码；
- 是否升档、是否使用公网。

绝不记录提示词、回复、路径、命令、原始错误和凭据。动态评分使用滑动窗口和最低样本数；
样本不足时回到目录候选顺序，不能拿一次好运气永久封神。

## 分阶段落地

### P0：契约（本文件）

- 冻结 v1 JSON Schema、示例和字段语义；
- 两端建立逐字 fixture 测试；
- 不改变现有 Provider 选择和凭据读取。

### P1：共享只读目录

- Xedit 从现有 Provider Library 导出非敏感目录；
- rs 读取目录，并在 `/routing` 展示候选与事实来源；
- Provider 上报窗口覆盖两端名称推断；
- 两端仍各用原凭据来源。

### P2：共享凭据引用

- Rust 增加 `CredentialResolver`；
- 签名版 macOS CLI 接入共享 Data Protection Keychain；
- Xedit Keychain 行迁移；Linux/Windows 接系统后端；
- 环境变量与旧 TOML 保留兼容回退。

### P3：候选池路由

- `profile -> model` 改成 `profile -> candidates`；
- 先硬过滤，再按观测评分；
- 测试/构建只允许一次同档重试，然后带 Verifier 证据升档；
- Deep 与公网继续使用 Runtime 硬门禁。

### P4：统一 Provider 调用面

- Xedit 把模型调用交给 willdeep-rs Runtime；
- 凭据只在 Runtime 解析；
- App 成为原生工作台，rs 成为唯一 Harness；
- 删除两套 Provider Client、压缩和路由逻辑的长期漂移。

## 不做

- 不把 API Key 写进共享 JSON；
- 不自动扫描所有环境变量并持久化；
- 不因 `/models` 未列出就删除虚拟模型；
- 不让模型自己声明能力、窗口或是否值得升档；
- 不把动态成功率直接改写成模型事实；
- 不在 Xedit 当前 Provider Library 与 rs TOML 之间做无冲突语义的双向盲同步；
- 不为了共享凭据引入一个任何本地进程都能导出明文的 helper。
