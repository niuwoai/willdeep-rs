# Web 重试等待渲染回归

运行真实 `web/src/App.tsx`，由隔离夹具响应 Runtime API；不连接真实 daemon、工作区或模型，不加载凭据。仅监听 `127.0.0.1:19849`，未定义的 API 返回 404，不代理到生产服务。

## 运行

安装现有 Web 依赖后，在仓库根目录启动：

```sh
node scripts/web_runtime_retry_fixture.mjs
```

另一个终端运行浏览器测试。需要本机 Google Chrome，以及可被 Node 解析的 Playwright；也可用 `NODE_PATH` 指向现有 Playwright 包目录，无需改动生产 Web 依赖。

```sh
node scripts/web_runtime_retry_test.cjs
node scripts/web_chat_stop_test.cjs
node scripts/web_input_suggestion_test.cjs
```

测试完成后用 Ctrl+C 停止夹具。测试操作仅改变夹具内存状态。

## 检查范围

- Runtime 活动显示等待重试，停止生成按钮可用。
- 页面重载后从活动 API 重新获取等待状态，而非依赖旧 React 内存。
- 服务端阶段转为运行后，真实两秒轮询使等待标记消失。
- 等待中点击停止，真实 App 发送一次带正确 workspace 的 stop 请求。
- 停止后重载保持取消状态，停止入口消失、重试入口可见。
- 轮次收尾后空输入框出现灰字预测；`Tab` 只填入、不发 `/api/chat/stream`；打字与 `Esc` 放弃后不复现；刷新不复现；服务端回 `null` 时不显示；英日两语提示正确。
- 浏览器无未处理页面异常。

结果位于 `target/web-runtime-retry/report.json` 和 `report.md`；截图为 `waiting-after-refresh.png`、`stopped-after-refresh.png`。

根聊天测试另外覆盖：等待重试期间停止、提交轮次 ID 到达前提前停止。两者均通过真实按钮点击，断言正确轮次仅收到一次 stop 请求、SSE 连接关闭、等待提示消失、输入区恢复发送状态。结果和截图位于 `target/web-chat-stop/`。

此项证明前端渲染、刷新、轮询和控制请求。真实 Provider 429、daemon 重启与跨进程恢复分别由其他集成回归验收，不能用本夹具替代。
