# 定时跑实弹靶场

靶场不进 PR 的 CI——它要真凭据、要网络、每轮都花钱。但完全靠人记得跑，
结果就是 2026-08-16 之后七天没有第二个数据点。折中办法是定时：每周一轮，
跑完把变更留在工作区等人 review。

## macOS（launchd）

```bash
sed "s|__REPO__|$(pwd)|g" scripts/launchd/com.willdeep.range.plist \
  > ~/Library/LaunchAgents/com.willdeep.range.plist
launchctl load ~/Library/LaunchAgents/com.willdeep.range.plist
```

默认每周一 03:00 本地时间。确认已装上：

```bash
launchctl list | grep com.willdeep.range
```

先手动试一次，别等到周一才发现凭据没配好：

```bash
launchctl start com.willdeep.range
tail -f target/skill-worker-range/weekly.log
```

卸载：

```bash
launchctl unload ~/Library/LaunchAgents/com.willdeep.range.plist
rm ~/Library/LaunchAgents/com.willdeep.range.plist
```

## Linux（cron）

```cron
0 3 * * 1 cd /path/to/willdeep-rs && ./scripts/range_weekly.sh
```

cron 的环境变量比登录 shell 干净得多，`ruby` 和 `cargo` 很可能不在 `PATH` 里。
出问题先看 `target/skill-worker-range/weekly.log`。

## 它会动什么

跑完之后工作区里多出这些**未提交**的变更：

| 路径 | 变化 |
|---|---|
| `bench/skill-worker-range/history.jsonl` | 多一行摘要 |
| `bench/skill-worker-range/runs/*.json` | 多一份完整报告 |
| `README.md`、`docs/SKILL_WORKERS.md` | 趋势区块被重写 |

**它不自动提交，也不自动 `git pull`。** 一轮成绩是不是该进历史得有人看一眼，
尤其是当它变差的时候；测哪版代码也该由人决定，而不是由定时器决定。

靶场本身失败时（凭据过期、网络不通、样本没变红），趋势**不更新**——宁可显示
上一轮的旧数字，也不显示半轮的假数字。退出码非零，日志里有原因。
