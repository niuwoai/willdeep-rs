# willdeep-runtime-protocol

WillDeep Runtime 控制面的稳定协议：操作名、请求 / 响应 DTO、统一响应信封、事件，以及
主 Agent 事件内核的入向信号契约。服务端（`willdeep daemon`）与 Rust 客户端
（[`willdeep-runtime-client`](https://crates.io/crates/willdeep-runtime-client)）共同依赖
这一个 crate：字符串、错误码、对象枚举只在这里定义一次，谁也不许各抄一份。

- 协议版本：`PROTOCOL_VERSION`，客户端连接后先 `GET /v1/capabilities` 协商，
  服务端返回 `protocol_version`、`min_client_protocol_version` 和它真正提供的操作名列表。
- 操作名：`SUPPORTED_OPERATIONS`，形如 `session.create` / `turn.submit` / `event.list`。
- 信封：`ApiResponse<T>` 是 `Ok { data, meta }` 或 `Error { error, meta }`，`ApiError` 带
  稳定的 `ErrorCode`、可读消息与 `retryable`。
- 跨语言夹具：`fixtures/` 里是 Swift / TypeScript 客户端也要逐字通过的样例。

完整语义见仓库文档
[`docs/RUNTIME_CONTROL_API.md`](https://github.com/niuwoai/willdeep-rs/blob/main/docs/RUNTIME_CONTROL_API.md)。
crate 版本跟随 `willdeep` 发行版本；线上契约以协商到的 `protocol_version` 为准。
