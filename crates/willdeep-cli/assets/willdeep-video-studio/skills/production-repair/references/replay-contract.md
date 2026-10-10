# 固定复现包契约

复现包目录包含 replay.rb 和本用例全部固定数据。由可信初始实现/独立检查确定断言，修复候选不得修改这些文件。不执行付费生成，不改凭据，不切换宿主。测试夹具必须写明其性质，不能记为真实制作事故。

控制器执行 `ruby <冻结包>/replay.rb baseline|candidate|live`，提供 WILLDEEP_PLUGIN_SOURCE、WILLDEEP_HOME、WILLDEEP_DRAMA_ID、WILLDEEP_INCIDENT_ID。使用 ENV 指定的源仓库读业务实现，使用 __dir__ 读冻结数据；不得通过模式分支直接决定成功。

stdout 仅输出一个 JSON 对象；诊断走 stderr。必须含固定 assertionID 和 status。baseline 运行相同业务断言，实际失败时 status=failed 且 exit=1；candidate 和 live 断言通过才 status=passed 且 exit=0。解析失败、跳过、缺数据、异常或其它退出码均不得取得通过。

live 必须验证实际宿主工具结果与落地的原任务产物，并返回 incidentID、dramaID 和 artifact={path:绝对本地文件路径,sha256:实际文件摘要}。产物检查应覆盖目标内容、任务关联、保存状态等业务条件；生成一个任意空文件不能算语义验收。控制器会核对文件存在与摘要及实际运行包；固定断言的业务质量仍需可信审查。

验收报告区分两类：隔离流程门禁测试；真实制作事故的原失败→候选修复→真实恢复。前者通过不能冒充后者完成。原版复现不稳定、候选未通过、真实回放无法安全执行时保留未解决。
