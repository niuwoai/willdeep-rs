#!/usr/bin/env ruby
require 'json'
require 'digest'
require 'fileutils'

module SwebenchFailureAnalysis
  def self.observe(evidence)
    messages = evidence.fetch('trace', {}).fetch('sessions', []).flat_map { |s| s.fetch('messages', []) }
    calls = messages.flat_map { |m| m.fetch('tool_calls', []) || [] }
    returned = messages.select { |m| m['role'] == 'tool' }.map { |m| m['tool_call_id'] }
    last = messages.reverse.find { |m| m['role'] == 'assistant' }
    pending = (last || {}).fetch('tool_calls', []).reject { |c| returned.include?(c['id']) }
    files = evidence.fetch('patch', '').scan(/^diff --git a\/(.*?) b\/(.*?)$/).map(&:last)
    diagnostic_only = !files.empty? && files.all? { |f| f.match?(%r{\A(?:tests?/|core\z)}) }
    {
      'trace_missing' => messages.empty?, 'patch_files' => files,
      'diagnostic_or_core_only' => diagnostic_only,
      'tool_calls' => calls.group_by { |c| c['name'] }.transform_values(&:length),
      'last_proposed_tools_without_saved_result' => pending.map { |c| c['name'] },
      'spawn_calls_with_saved_result' => calls.count { |c| c['name'] == 'spawn_agent' && returned.include?(c['id']) }
    }
  end

  def self.run(input, output)
    index = JSON.parse(File.read(File.join(input, 'index.json')))
    sources = {'index.json' => Digest::SHA256.file(File.join(input, 'index.json')).hexdigest}
    failures = index.fetch('runs').flat_map do |run|
      run.fetch('rows').reject { |row| row['resolved'] }.map do |row|
        relative = row.fetch('evidence').delete_prefix('/benchmarks/')
        path = File.join(input, relative)
        sources[relative] = Digest::SHA256.file(path).hexdigest
        evidence = JSON.parse(File.read(path))
        official = evidence.fetch('official_report', nil) || {}
        judge = official.fetch(row.fetch('id'), {})
        tests = judge.fetch('tests_status', {})
        observe(evidence).merge(
          'run' => run.fetch('id'), 'instance_id' => row.fetch('id'),
          'status' => row.fetch('status'), 'reported_tokens' => row.fetch('tokens'),
          'official_report_present' => official.key?(row.fetch('id')),
          'fail_to_pass_failures' => tests.fetch('FAIL_TO_PASS', {}).fetch('failure', []),
          'pass_to_pass_failure_count' => tests.fetch('PASS_TO_PASS', {}).fetch('failure', []).length,
          'evidence_url' => 'https://willdeep.com' + row.fetch('evidence')
        )
      end
    end
    summary = {
      'schema' => 'willdeep.swebench.failure-analysis.v1',
      'scope' => 'Published sanitized CLI 0.92.0-rc1 traces; observations are not inferred root causes.',
      'runs' => index.fetch('runs').map { |r| r.slice('id', 'model', 'resolved', 'planned', 'statuses') },
      'failed_attempts' => failures.length,
      'distinct_failed_tasks' => failures.map { |f| f['instance_id'] }.uniq.length,
      'empty_patches' => failures.count { |f| f['patch_files'].empty? },
      'trace_missing' => failures.count { |f| f['trace_missing'] },
      'failures' => failures, 'input_sha256' => sources
    }
    FileUtils.mkdir_p(output)
    File.write(File.join(output, 'report.json'), JSON.pretty_generate(summary) + "\n")
    lines = ["# SWE-bench 失败证据分析（第一轮）", '',
      "两轮各 30 题：DeepSeek 11/30，GLM-5 19/30。共有 #{summary['failed_attempts']} 次失败尝试、#{summary['distinct_failed_tasks']} 个不同失败题，其中 #{summary['empty_patches']} 次为空补丁。", '',
      '范围：WillDeep CLI 0.92.0-rc1 的公开脱敏轨迹；不是 Mac 版，也不是完整 Verified 成绩。历史报告保持只读。', '',
      '## 逐题观察', '',
      '缺失工具返回只表示没有保存结果，不能单凭此认定工具失败或未运行。读取次数不代表浪费；测试目录/core-only 是产物观察，不是自动根因判定。零 token 且轨迹缺失不能证明未调用模型。测试数为 — 表示没有官方逐题测试报告，不表示零失败或通过。', '',
      '| 模型轮次 | 题目 | 终态 | Token | 修改文件数 | 末条提议无结果 | 官方失败测试数 FTP / PTP |',
      '|---|---|---|---:|---:|---|---:|']
    failures.each do |f|
      counts = f['official_report_present'] ? "#{f['fail_to_pass_failures'].length} / #{f['pass_to_pass_failure_count']}" : '—'
      lines << "| #{f['run']} | [#{f['instance_id']}](#{f['evidence_url']}) | #{f['status']} | #{f['reported_tokens']} | #{f['patch_files'].length} | #{f['last_proposed_tools_without_saved_result'].join(', ')} | #{counts} |"
    end
    lines += ['', '## 可行动的证据与假设', '',
      '- 空补丁：部分预算耗尽题最后仍在提议委派/等待，尚无保存结果。应在边界可确定后及时委派，避免完整重复探索；这并不能证明所有空补丁都是委派造成。',
      '- django-13449：GLM 轨迹包含 25 次 grep、24 次 read，无修改；DeepSeek 同题产物仅诊断测试。应把探索推进到可验证的实现修改，临时复现文件不能替代修复。',
      '- sympy-11618：DeepSeek 局部测试通过，但以拒绝不同维度点的方式修复距离计算，与问题要求的计算结果不符。应先写出原始例子的预期行为，再验证修复；不能把自行设计的测试全绿当作完成。',
      '- django-16145：DeepSeek 子 Agent 报告在独立工作树及 /testbed 重复修改以适配绝对路径 verifier。应让命令基于执行工作树运行，合并后检查父工作树实际差异；不要扩大写权限来绕过。',
      '- django-11555：DeepSeek 轨迹缺失，只有 core 产物与官方未解决判定。原因未知，单独列为采集缺口。', '',
      '## 第一轮候选与验证边界', '',
      '候选仅修改通用主 Agent 与 implementer 提示词：原始行为验收、有限探索后执行、及时委派、工作树内验证、合并后检查实现差异。不写入题号、gold patch 或特定项目答案，不改变模型、预算、权限与裁判。', '',
      '历史评测为 0.92.0-rc1；候选基于 develop 的 0.94.0-rc1，标记为 0.94.0-rc2。比较历史分数不能隔离提示词效果。下一步须同版本、同模型、同参数分别运行 baseline/candidate，保留成功题回归，新增 30 题作为尚未分析的 holdout。未重跑前，不宣称解决率提升。']
    File.write(File.join(output, 'report.md'), lines.join("\n") + "\n")
    puts JSON.generate(summary.reject { |k, _| ['failures', 'input_sha256'].include?(k) })
  end
end

SwebenchFailureAnalysis.run(ARGV.fetch(0), ARGV.fetch(1)) if $PROGRAM_NAME == __FILE__
