# Provider 协议契约

## Chat Completions

- Endpoint：`{api_base}/chat/completions`；
- 鉴权：Bearer；
- 工具定义：`{ type: function, function: { name, description, parameters } }`；
- 工具调用：assistant `tool_calls`；
- 工具结果：`role: tool` + `tool_call_id`。

## Responses

- Endpoint：`{api_base}/responses`；
- 鉴权：Bearer；
- 系统提示：顶层 `instructions`；
- 普通消息：typed `message` input；
- 工具调用：`function_call`；
- 工具结果：`function_call_output`；
- 当前使用 `store: false`、`stream: false`。

## Anthropic Messages

- Endpoint：去掉 API Base 尾部 `/v1` 后追加 `/v1/messages`；
- 原生鉴权：`x-api-key`；
- 必需头：`anthropic-version: 2023-06-01`；
- 系统提示：顶层 `system`；
- 工具定义：`{ name, description, input_schema }`；
- 工具调用：assistant `tool_use` content block；
- 工具结果：下一条 user message 中的 `tool_result` content block；
- 根据旧 Claude 3 型号限制 `max_tokens`，与 Swift App 行为一致。

## some.im

some.im 是 Provider 身份，不是第四种线协议：

- 默认 Base：`https://some.im/v1`；
- 中国线路可使用 `https://api.niuwoai.com/v1`；
- 鉴权使用 Bearer，包括选择 Anthropic Messages Dialect 时；
- 添加 `x-willdeep-session-id`；
- 添加 `x-willdeep-workspace-id`；
- 两个 ID 均为随机 UUID，不包含用户名、目录或项目路径。

