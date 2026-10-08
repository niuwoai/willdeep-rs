# 首次 Web Provider 配置引导验收

版本：`0.93.0-rc1`，分支：`codex/web-provider-onboarding`。
本记录为发布前的开发验收；Git 集成、公开发行包与本机安装结果须另行核验。

## 自动检查

- 修正前的 `cargo test --workspace --offline --quiet`：1397 项通过、7 项忽略；子进程 helper 另通过 1 项。
  初次沙箱运行的 17 项本机监听测试被权限阻止；允许回环监听后完整重跑通过。
- `cargo clippy --workspace --all-targets --offline -- -D warnings`：通过。
- `cargo fmt --all --check`、`git diff --check`、`ruby scripts/check_source_size.rb`：通过。
- `yarn test`：102 项通过；`yarn build`：通过。
- 新增验证：首次安装插件不自动批准/启用；重复安装保留审批；实际 Ruby MCP 进程
  读取宿主指定的自定义 TOML；未保存/无效默认 Provider 不算就绪。
- 内嵌插件的 13 个文件与来源固定提交逐文件 SHA256 相同。

## 真实页面验收

临时 home：`/private/tmp/willdeep-first-use-qa`；临时 workspace：
`/private/tmp/willdeep-first-use-workspace`；监听地址：`127.0.0.1:19857`。
使用无效域名和占位密钥，没有联系模型服务，也没有修改用户现有配置。

1. 全新 home、指定不存在的 `selected.toml`，WebApp 无 Provider 也能启动，并打印引导链接。
2. 引导显示 willdeep-config 的审批、启用及 Provider 配置步骤；插件初始未批准。
3. 批准、启用后出现「打开配置插件」，点击进入真实沙箱 iframe 配置页面。
4. 新增 `setup-test` Provider，设置 OpenAI 兼容协议、`https://setup.invalid/v1`、
   占位密钥与 `setup-model`，选择默认 Provider。
5. 预览包含五项变更，密钥遮罩；确认写入成功，目标文件创建，页面显示已保存。
6. 点击「配置好了，开始使用」，检查通过，去除 setup 参数、关闭引导并进入对话页面。
7. 测试页面、Web 服务和临时 Runtime 已关闭。

截图：`/private/tmp/willdeep-provider-onboarding.png`。
就绪检查仅验证配置可解析、地址使用 HTTP(S)、具备凭据和模型，不验证远端认证或余额。
当前插件需要 `/usr/bin/ruby`；Windows / 无 Ruby 的 Linux 可用终端手动配置。

## 命令行首次启动流程修正验收

临时 home：`/private/tmp/willdeep-cli-setup-correction`，工作区为其 `workspace` 子目录。
在真实 PTY 终端直接启动 `willdeep --workspace ...`，未传 `--web` 或 `--onboarding`。

1. 无配置文件时自动安装内嵌的 willdeep-config 0.2.0，没有先弹菜单。
2. 打印本次实际地址 `http://127.0.0.1:64238/?setup=1#plugins`，以及
   `http://127.0.0.1:64238/?setup=1#plugin/willdeep-config%3Aconfig`。
3. `/api/setup` 返回 `ready: false` 和正确的临时配置路径，CLI 等待保存。
4. 写入仅含无效域名与占位凭据的隔离配置来验证 CLI 的保存检测（真实插件保存另见上节）。
   无须重启，终端输出「Provider 配置已就绪，继续启动命令行」，随后渲染真实 TUI。
5. Ctrl+C 正常退出，配置 WebApp 端口随 CLI 退出而关闭；临时 Runtime 已正常停止。
6. 前端重新执行 `yarn test --run`：102 项通过；`yarn build` 通过。
7. 修正后 `cargo test --workspace --offline --quiet`：1398 项通过、7 项忽略，
   子进程 helper 另通过 1 项；Clippy、格式、差异检查及源码行数门禁全部通过。

既有空配置同样触发引导；有效配置与环境变量配置直接启动。首次 CLI 引导只打印链接，
不自动打开浏览器或切换为前台 Web 模式；用户无需退出后再启动。
