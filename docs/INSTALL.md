# 安装与构建

## 用 Homebrew 安装（macOS，推荐）

WillDeep 通过第三方 Homebrew Tap `niuwoai/tap` 发布（非 Homebrew 官方收录），安装的是 macOS 通用二进制（Apple Silicon 与 Intel 均可用），不需要本地 Rust 或 Node 环境：

```bash
brew install niuwoai/tap/willdeep-cli
```

这条命令会自动添加 Tap；也可以先执行 `brew tap niuwoai/tap`，再用 `brew install willdeep-cli`。装好后命令名是 `willdeep`：

```bash
willdeep --version
```

升级与卸载：

```bash
brew upgrade willdeep-cli
brew uninstall willdeep-cli
```

如果本机跑着 Runtime Daemon，升级后执行 `willdeep daemon upgrade` 让它切换到新版本，详见 [Runtime Daemon](RUNTIME_DAEMON.md)。卸载只删除程序本身，配置与会话数据保留在原处。

Linux、Windows 或需要自行修改代码时，按下文从源码构建。

## 从源码构建

要求 Rust 1.94、Node.js 22 与 Yarn。Web 前端会嵌入最终二进制，所以必须先构建前端：

```bash
cd web
yarn install --frozen-lockfile
yarn build
cd ..
cargo build --release
```

产物位于 `target/release/willdeep`，Windows 下为 `target/release/willdeep.exe`。

Rust 构建显式跟踪 `web/dist`：前端产物变化会触发二进制重新嵌入，Debug 与 Release 都不会静默沿用旧的 Web UI。

## 平台支持

CI 覆盖三大系统构建与测试、Linux AMD64/ARM64 交叉测试、WSL ABI 烟测，并在打 tag 时自动发布。

## Shell 补全与 man page

补全脚本和 man page 从当前真实命令树动态生成，不会与实际命令脱节：

```bash
willdeep completions bash
willdeep completions zsh
willdeep completions fish
willdeep completions powershell
willdeep man
```

把补全输出保存到对应 Shell 的补全目录，或在当前 Shell 中直接加载。`willdeep man` 输出标准 roff，可安装为 `willdeep.1`。

## 首次运行

首次运行且没有配置时会自动进入交互式设置，也可随时执行：

```bash
willdeep --onboarding
```

配置细节见 [配置指南](CONFIGURATION.md)，登录与凭据见 [认证与凭据](AUTHENTICATION.md)。

## 验证安装

```bash
willdeep doctor
```

`doctor` 在不联系任何 Provider 的前提下检查配置、Provider 完整性、工作区、Git、内嵌 Web 资源与 Runtime 版本/传输状态。详见 [故障排查](TROUBLESHOOTING.md)。

## 开发者验证

```bash
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

三种 Provider 协议均有本地 Mock HTTP 契约测试，覆盖完整工具往返，不会调用真实 API，也不会消耗 Key。
