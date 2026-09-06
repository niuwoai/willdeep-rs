# Agent 可靠性任务评测

模式：live_cli_configuration；已执行：5
未实现场景：无
完成率：100.0%；误报完成率：0.0%
已核实中断：1；恢复率：100.0%

| 场景 | 状态 | 外部验收 | 误报完成 | 秒 | 输入 Token | 输出 Token | 人工介入 |
| --- | --- | --- | --- | ---: | ---: | ---: | ---: |
| dirty_files | evaluated | true | false | 25.4 | 69581 | 1830 | 0 |
| repeated_failure | evaluated | true | false | 21.345 | 69777 | 2239 | 0 |
| subtask_integration | evaluated | true | false | 59.082 | 69434 | 5014 | 0 |
| compression_constraints | evaluated | true | false | 21.41 | 未取得 | 未取得 | 0 |
| interruption_recovery | evaluated | true | false | 20.798 | 未取得 | 未取得 | 0 |

“未取得”表示未执行或缺少完整证据，不等于 0；预检不计为真实任务成功。
