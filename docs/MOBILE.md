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
| 房间 | `willdeep-cli-<uuid>` |
| 认证 | `Authorization: Bearer <token>` |
| 断线重连 | 2 秒间隔自动重试 |

**CLI 不监听任何本地端口**，只主动外连中继。

## 凭据

CLI 使用**独立于 Swift App** 的 room 与 token，保存在 `$WILLDEEP_HOME/mobile-relay.toml`（默认 `~/.willdeep/mobile-relay.toml`）：

```toml
relay_base_url = "https://j.niuwoai.com"
room = "willdeep-cli-<uuid>"
token = "<64 位十六进制随机值>"
```

Unix 权限为 `0600`。首次生成时先写临时文件并设好权限再 rename，不存在权限窗口。已存在的文件在使用前会校验权限，不合规则拒绝启动中继并提示 `chmod 600`。

该文件不会写入仓库。

## 安全提醒

**配对二维码中明文携带 relay token。** 扫码即等于交出该中继房间的访问权限：

- 只对自己的手机扫码；
- 不要把二维码截图分享或上传；
- 二维码里的过期时间实际上是一个远期常量，等同于**永不过期**。需要作废时用 `/mobile off` 断开，并删除 `mobile-relay.toml` 让下次生成新的 room 与 token。

## 相关文档

- [TUI 使用指南](TUI_GUIDE.md)
- [认证与凭据](AUTHENTICATION.md)
