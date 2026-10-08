# willdeep-config

WillDeep 配置管理插件，提供 TOML 配置文件的读取、编辑、校验与备份功能。

## 功能

- 配置文件快照读取（`config.snapshot`）
- 变更规划与预览（`config.plan`）
- 安全应用变更（`config.apply`）
- 配置校验（`config.validate`）
- 备份列表与恢复（`config.backups`、`config.restore`）
- 配置模板生成（`config.render`）
- 集合实例（提供商、MCP 服务、子代理）的新增与整节删除（`removeSection`）

页面同时运行在 WillDeep mac（WKWebView）与 willdeep-rs（沙箱 iframe）里，两边的差异与页面约定见
`docs/tool-contract.md` 的「页面显示约定」。

## 密钥处理

schema 中标记为 `secret: true` 的字段（如 provider 的 `api_key`）：
- 返回值中不包含明文，`value` 为空串，改用 `hasValue: true` 与 `preview: "…abcd"`（仅末 4 位）
- `raw` 预览文本中密钥值替换为 `"••••"`
- 日志与 stderr 不输出密钥值
- 仅 `config.reveal_secret` 工具可返回明文（需 `confirm: true`，仅供插件页面显示）

## 备份位置

默认备份目录：`~/Library/Application Support/WillDeep/plugin-data/willdeep-config/backups`

## 本地预览

```bash
ruby scripts/preview_server.rb 4789
# 打开 http://127.0.0.1:4789/?host=mac&scheme=light&locale=zh_CN
# host=rs 模拟 willdeep-rs（不注入主题变量），scheme=dark|light，locale=zh_CN|en
```

预览用临时目录里的样例配置，不会读写 `~/.willdeep/config.toml`。

## 构建与打包

```bash
# 打包为 zip（包含所有必需文件）
./scripts/package.sh
```

打包产物：`willdeep-config-<version>.zip`，可用于本地目录安装或分发。