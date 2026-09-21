# README 首屏演示。别直接 `vhs` 它：用 `ruby scripts/record_readme_demo.rb` 跑，
# 那个脚本负责搭临时演示仓库与 WILLDEEP_HOME、把凭据只经环境变量传进来、录完清理。
# 改了 TUI 的手感就重录一次。

# vhs 0.12 在本机合成视频那一步会静默失败（不报错、不出文件），所以只让它出 PNG 帧，
# 由 record_readme_demo.rb 用 ffmpeg 叠层、按总时长倍速、调色板压缩成 GIF。
Output "/tmp/willdeep-demo/frames/"

Set Shell "bash"
Set FontSize 14
Set Width 1000
Set Height 600
Set Padding 12
Set Theme "Catppuccin Mocha"
Set TypingSpeed 40ms

Hide
Type "export PS1='$ ' && cd /tmp/willdeep-demo/shop && clear"
Enter
Sleep 500ms
Show

Type "willdeep"
Sleep 300ms
Enter
Wait+Screen@30s /Shift\+Tab/
Sleep 1s

Type "检查当前仓库并修复测试，改完我先看看"
Sleep 600ms
Enter

# 先等轮次收尾（分隔线里的「本轮结束」；别用「轮到你」，输入框标题里常驻），再等灰字预测。模型判断任务已收口时会答 NONE、不出灰字——
# 那是对的行为，但这一条就录不成演示：超时失败，由脚本重录。
Wait+Screen@240s /本轮结束/
Wait+Screen@20s /Tab 采用/
Sleep 5s
