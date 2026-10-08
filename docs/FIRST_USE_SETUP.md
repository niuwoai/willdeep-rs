# 首次使用：在 WebApp 配置 Provider

首次在交互终端运行 `willdeep`，且尚无可用的 Provider 配置或环境变量时，
CLI 自动在本机回环地址启动后台 WebApp，直接打印配置引导地址和 willdeep-config 配置页地址，
无需先选择菜单。用户按提示打开地址，终端等待保存配置。
也可用 `willdeep --onboarding` 再次打开引导；`willdeep --web` 在全新环境同样输出配置链接。
无桌面浏览器或浏览器启动失败时，手动访问终端打印的 URL；SSH 用户可转发该端口。
非交互式运行不会打开浏览器或弹交互提示。

1. 在插件中心查看 `willdeep-config` 的来源和权限，批准并启用。
2. 点击引导中的「打开配置插件」。新增所需的 Provider，填写 API Base、API Key
   （或 API Key 环境变量名）、模型与协议，并选择默认 Provider。
3. 在插件里预览、应用并校验配置。可配置多个 Provider，原有的备份与恢复功能保持可用。
4. 点击「配置好了，开始使用」。宿主检查已保存配置能否解析出默认 Provider 的地址、
   凭据和模型；此检查不调用模型，不验证账户余额或服务端认证是否成功。
5. 保存后 CLI 自动重新读取配置，继续进入终端 TUI，无需退出或重新启动。
   配置 WebApp 在本次 CLI 进程存活期间继续可用，退出 CLI 后关闭；等待期间可按 Ctrl+C 取消。
   显式运行 `willdeep --web` 时仍使用前台 Web 模式。

CLI 内嵌第一方插件 `0.2.0`（来源及固定提交见 `crates/willdeep-cli/assets/CONFIG_PLUGIN_SOURCE.md`）。
引导只在不存在任何已安装版本时安装它，不覆盖、升级或自动批准现有插件。
插件需要 `/usr/bin/ruby`，macOS 自带；Linux 需安装 Ruby，Windows 可选择终端配置。
缺少 Ruby 时引导会提示替代路径：`willdeep --onboarding`，选择 **3) 终端手动配置**。
原 some.im 浏览器登录保留在选项 2。

插件操作的是宿主的实际配置文件（包含 `--config` 和 `WILLDEEP_HOME`），备份写入
当前 WillDeep home 的 `plugin-data/willdeep-config/backups`。输入和输出均不在引导里回显密钥。
已通过环境变量配置的 CLI 用户直接启动，不重复进入引导；引导中的「稍后配置」只隐藏当前页面提示。
