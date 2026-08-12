# 手机中继

`/mobile` 让你用 WillDeep Mobile 从手机向当前 CLI 会话发消息——排队时、通勤时补一句指令，回到电脑前结果已经跑完了。

## 使用

在 TUI 中输入：

```text
/mobile          # 连接中继并显示配对二维码
/mobile show     # 再次显示二维码
/mobile hide     # 只隐藏二维码，保持连接（等价于按 Esc）
/mobile off      # 断开中继
```

用 WillDeep Mobile 扫码后即可从手机发消息，消息作为新的 Prompt 进入当前会话。

## 连接

| 项目 | 值 |
|---|---|
| 中继地址 | `wss://j.niuwoai.com/ws/broadcast/<room>` |
| 协议版本 | `mobile-gateway.v1` |
| 房间 | `wd-<32 位十六进制>` |
| 认证 | `Authorization: Bearer <token>` |
| 断线重连 | 2 秒间隔自动重试 |

**CLI 不监听任何本地端口**，只主动外连中继。

## 凭据

CLI 使用**独立于 Swift App** 的 room 与 token，保存在 `$WILLDEEP_HOME/mobile-relay.toml`（默认 `~/.willdeep/mobile-relay.toml`）：

```toml
relay_base_url = "https://j.niuwoai.com"
room = "wd-<32 位十六进制>"
token = "<32 位十六进制随机值，即 128 位熵>"
```

Unix 权限为 `0600`。首次生成时先写临时文件并设好权限再 rename，不存在权限窗口。已存在的文件在使用前会校验权限，不合规则拒绝启动中继并提示 `chmod 600`。

旧版凭据（`willdeep-cli-<uuid>` room + 64 位 token）会在下次加载时自动重新生成为紧凑格式并覆盖原文件——手机重新扫一次码即可，无需其它操作。

## 配对二维码

二维码里装的是 `mobile-gateway.v1` 的**紧凑配对 URL**，不是完整配对 JSON：

```text
https://j.niuwoai.com/pair?r=<room>&t=<token>&d=<桌面名>
```

| 参数 | 含义 | 缺省行为 |
|---|---|---|
| `r` | relay room | 必填 |
| `t` | relay token | 必填 |
| `d` | 桌面名（≤16 字符） | 手机端显示为 `WillDeep Mac` |
| `u` | relay base url | 缺省即 `https://j.niuwoai.com`，自建中继才下发 |
| `v` | 协议版本 | 缺省即 `mobile-gateway.v1` |

手机端把这几个参数补全成完整配对 JSON：`base_url` 由 `u` 推出，`pairing_token` 等于 `t`，`expires_at` 取远期常量。所以 `base_url`/`pairing_token`/`expires_at` 不进二维码——它们要么是中继字段的副本，要么是常量，每重复一份都要多烧几十个模块。

尺寸：二维码在终端里每个模块占一个字符格，只由载荷字节数和纠错等级决定。当前载荷 114 字节、纠错 L 级（屏幕显示不存在印刷污损，7% 冗余足够），二维码 41×41 模块，加静区即 **49 列 × 25 行**（Dense1x2 渲染，一个字符格装两行模块）。`mobile.rs` 的 `pairing_qr_fits_the_terminal_popup` 把尺寸钉死为 `MAX_QR_WIDTH` × `MAX_QR_HEIGHT`，任何加长配对内容的改动都会先撞到它。

该文件不会写入仓库。

## 安全提醒

**配对二维码中明文携带 relay token。** 扫码即等于交出该中继房间的访问权限：

- 只对自己的手机扫码；
- 不要把二维码截图分享或上传；
- 二维码里的过期时间实际上是一个远期常量，等同于**永不过期**。需要作废时用 `/mobile off` 断开，并删除 `mobile-relay.toml` 让下次生成新的 room 与 token。

## 相关文档

- [TUI 使用指南](TUI_GUIDE.md)
- [认证与凭据](AUTHENTICATION.md)
