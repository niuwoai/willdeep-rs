# 短剧插件的自定义圆桌

Rust 宿主从 0.90.0-rc1 实现 `willdeep/roundtable/run`。短剧工坊 0.44.0-rc1 可使用该能力完成主框架或单集的创作审改闭环。现有内置商业专家圆桌仍保留原来的固定专家入口。

## 宿主契约

- 插件清单必须具有 `ai.chat` 权限；初始化时宿主宣告反向请求方法。
- 请求含 title/topic/experts；每位专家含 id/name/expertise/persona。支持 2～8 席、1～3 轮，专家 ID 不重复。
- 可指定 chairPersona、verdictInstruction、provider、model。模型调用走现有宿主路由与用量账本，凭据不交给插件。
- 专家顺序发言并看到前序发言；单次上下文带最近 5000 字发言记录。原始 topic 最多 18000 字，且 JSON 转义后不超过该预算；超长直接拒绝，不静默截掉审核材料。
- 主持人每轮总结并判断收敛，生成终稿后按 verdictInstruction 生成结构化 verdict。少于两位或不足半数专家成功发言时不给审核结论。
- 响应含完整 transcript、document、verdict、reportID/sessionID 等；报告保存在 `WILLDEEP_HOME/plugin-data/<插件 ID>/roundtables/`。
- MCP 圆桌最长 1800 秒，普通反向请求仍为 600 秒。当前一次性返回结果，不提供 macOS 圆桌页面的实时展示。

## 创作闭环

插件工具 `drama.write_with_panel` 接收 dramaID、scope（drama/episode）、episodeID（单集必需），可选 brief、maxRevisions（0～3，默认 2）、regenerate、requestID。

空正文先生成初稿；已有正文先审稿。没有必改项且未被拦截则 ready，否则按必改项修订后再审。达到上限或重复同稿则 needs_human，保留最后稿与意见；不会自动绕过上限继续调用。文字生成使用助手模型，审核使用审核模型；旧宿主可回退到插件自身的专家席。

每次采用经过文本白名单校验并保留历史。修订号、完整文字快照和账本版本在存储锁内比对；已有未采用草稿时拒绝启动。角色、分镜、资产和账本由各自工具管理，此循环只处理已建项目的主框架及单集文字。

取消会停止后续步骤并拒绝保存晚到的写稿结果；已经发出的宿主模型请求可能仍需结束，已有成功保存的版本保留。

## 验证入口

```sh
cargo test -p willdeep plugin_roundtable --bin willdeep
cargo test -p willdeep plugin_host_requests --bin willdeep
cargo test -p willdeep-core mcp --lib
cargo test -p willdeep --test workspace_versions
```

插件仓库对应测试为 `scripts/script_creation_test.rb`、`scripts/review_panel_test.rb`、`scripts/background_jobs_test.rb`，前端使用 `yarn test --run` 与 `yarn build`。测试使用隔离数据与模拟模型；未将模拟审核结果当作真实剧作质量结论。
