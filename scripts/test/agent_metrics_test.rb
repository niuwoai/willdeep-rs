#!/usr/bin/env ruby
# frozen_string_literal: true

# 线上派工指标快照与趋势的测试。
#
#   ruby scripts/test/agent_metrics_test.rb
#
# 不联网、不调 willdeep、不碰真实历史：用现造的 JSON 钉住归档形状（只留认识的
# 字段、null 原样保留）、目标判定（分母为 0 不算没达标、恰好等于目标算达标）、
# 渲染纪律（null 是 `-` 与 `·`、不是谷底）、注入幂等，以及两个脚本的离线命令行。

require 'fileutils'
require 'json'
require 'minitest/autorun'
require 'open3'
require 'tmpdir'

require_relative '../lib/agent_metrics_report'
require_relative '../agent_metrics_trend'

module MetricsFixture
  SCRIPTS = File.expand_path('..', __dir__)

  def self.metrics(overrides = {})
    {
      'since' => nil, 'children' => 6, 'workers' => 3, 'standard' => 2, 'deep' => 1,
      'verified_runs' => 4, 'passed' => 3, 'unverified_runs' => 2, 'attempts' => 5,
      'claims_checked' => 4, 'claims_unverifiable' => 1,
      'deep_share' => 16.7, 'skill_coverage' => 50.0, 'worker_verified_success' => 75.0,
      'escalation_rate' => 25.0, 'citation_accuracy' => 75.0, 'attempts_per_verified_run' => 1.25
    }.merge(overrides)
  end

  def self.on_target
    metrics('deep' => 0, 'deep_share' => 0.0, 'workers' => 4, 'skill_coverage' => 66.7,
            'passed' => 4, 'worker_verified_success' => 100.0, 'escalation_rate' => 0.0)
  end

  def self.empty
    counts = AgentMetricsReport::COUNT_KEYS.to_h { |key| [key, 0] }
    rates = AgentMetricsReport::RATE_KEYS.to_h { |key| [key, nil] }
    { 'since' => nil }.merge(counts).merge(rates)
  end

  def self.snapshot(recent, ran_at:, commit: 'abc1234')
    AgentMetricsReport.summarize(recent: recent, total: metrics('children' => 60), window: '7d',
                                 commit: commit, dirty: false, version: '0.78.0-rc26', ran_at: ran_at)
  end

  # 子进程的输出按当前 locale 打标签，CI 里往往是 US-ASCII；脚本写的是 UTF-8，照实标回来。
  def self.run_script(name, *args)
    out, err, status = Open3.capture3('ruby', File.join(SCRIPTS, name), *args)
    [out.force_encoding('UTF-8'), err.force_encoding('UTF-8'), status]
  end
end

class AgentMetricsReportTest < Minitest::Test
  def test_slice_keeps_known_fields_and_null_rates
    sliced = AgentMetricsReport.slice(MetricsFixture.metrics('surprise' => 1, 'deep_share' => nil, 'children' => '6'))
    refute sliced.key?('surprise'), '没 review 过的字段不该进公开历史'
    refute sliced.key?('since')
    assert_nil sliced['deep_share']
    assert_equal 6, sliced['children']
    assert_in_delta 75.0, sliced['worker_verified_success']
    assert_raises(KeyError) { AgentMetricsReport.slice(MetricsFixture.metrics.reject { |key, _| key == 'children' }) }
    assert_raises(ArgumentError) { AgentMetricsReport.slice([]) }
  end

  def test_summarize_carries_provenance_and_archive_appends
    Dir.mktmpdir do |dir|
      first = MetricsFixture.snapshot(MetricsFixture.metrics, ran_at: '2026-09-14T04:00:00Z')
      second = MetricsFixture.snapshot(MetricsFixture.on_target, ran_at: '2026-09-21T04:00:00Z', commit: 'def5678')
      assert_equal %w[ran_at commit dirty version window recent total], first.keys
      assert_equal '7d', first['window']
      assert_equal 60, first['total']['children']

      path = AgentMetricsReport.archive(first, dir)
      AgentMetricsReport.archive(second, dir)
      rows = File.readlines(path).map { |line| JSON.parse(line) }
      assert_equal %w[abc1234 def5678], rows.map { |row| row['commit'] }
      assert_nil rows[0]['recent']['since']
    end
  end

  def test_misses_flag_only_breached_targets
    missed = AgentMetricsReport.misses(MetricsFixture.metrics).map { |miss| miss[:key] }
    assert_equal %w[deep_share worker_verified_success escalation_rate], missed
    assert_empty AgentMetricsReport.misses(MetricsFixture.on_target)
    assert_empty AgentMetricsReport.misses(MetricsFixture.empty), '分母为 0 是「没得比」，不是没达标'
    exactly = MetricsFixture.on_target.merge('worker_verified_success' => 85.0, 'deep_share' => 5.0)
    assert_empty AgentMetricsReport.misses(exactly), '恰好等于目标算达标'
    assert_equal '≥ 85%', AgentMetricsReport.target_text('worker_verified_success')
    assert_equal '≤ 5%', AgentMetricsReport.target_text('deep_share')
    assert_equal '—', AgentMetricsReport.target_text('citation_accuracy')
  end
end

class AgentMetricsTrendTest < Minitest::Test
  def test_empty_history_explains_how_to_snapshot
    block = AgentMetricsTrend.render([])
    assert_includes block, 'agent_metrics_publish.rb'
    assert_empty AgentMetricsTrend.alarms([])
  end

  def test_render_shows_targets_deltas_and_misses
    history = [
      MetricsFixture.snapshot(MetricsFixture.on_target, ran_at: '2026-09-14T04:00:00Z'),
      MetricsFixture.snapshot(MetricsFixture.metrics, ran_at: '2026-09-21T04:00:00Z', commit: 'def5678')
    ]
    block = AgentMetricsTrend.render(history)
    assert_includes block, '最近快照：**2026-09-21T04:00:00Z** · 窗口 7d · 代码 `def5678` · 版本 `0.78.0-rc26`'
    assert_includes block, '| **Worker Verified Success** | 75% | -25 | ≥ 85% | `█▁` |'
    assert_includes block, '| **Deep Share** | 17% | +17 | ≤ 5% | `▁█` |'
    assert_includes block, '| 只读工种引用准确率 | 75% | ±0 | — | `▄▄` |'
    assert_includes block, '累计：子 Agent 运行 60'
    assert_includes block, '⚠️ 未达标：Deep Share 17%（目标 ≤ 5%）；Worker Verified Success 75%（目标 ≥ 85%）；Escalation Rate 25%（目标 ≤ 15%）'
    assert_includes block, '历史 2 次快照'
    assert_includes block, '| 2026-09-14T04:00:00Z | `abc1234` | 7d | 6 | 0% | 67% | 100% | 0% | 75% | 1.25 |'
  end

  def test_null_rates_render_as_dashes_not_valleys
    history = [MetricsFixture.snapshot(MetricsFixture.empty, ran_at: '2026-09-21T04:00:00Z')]
    block = AgentMetricsTrend.render(history)
    assert_includes block, '| **Deep Share** | - | — | ≤ 5% | `·` |'
    assert_includes block, '平均尝试 -'
    refute_includes block, '未达标'
    refute_match(/(?<!\d)0%/, block, '空窗口不该渲染出任何 0%（目标列里的 50% / 15% 不算）')
  end

  def test_alarms_judge_the_latest_snapshot_against_absolute_targets
    good = MetricsFixture.snapshot(MetricsFixture.on_target, ran_at: '2026-09-14T04:00:00Z')
    bad = MetricsFixture.snapshot(MetricsFixture.metrics, ran_at: '2026-09-21T04:00:00Z')
    assert_empty AgentMetricsTrend.alarms([bad, good])
    assert_equal %w[deep_share worker_verified_success escalation_rate], AgentMetricsTrend.alarms([good, bad]).map { |miss| miss[:key] }
    assert_equal %w[deep_share worker_verified_success escalation_rate], AgentMetricsTrend.alarms([bad]).map { |miss| miss[:key] },
                 '只有一次快照也报：比的是目标，不是上一次'
  end

  def test_inject_is_idempotent_and_requires_markers
    original = "前文\n\n<!-- agent-metrics:begin -->\n旧内容\n<!-- agent-metrics:end -->\n\n后文\n"
    once = AgentMetricsTrend.inject(original, '新内容')
    assert_equal "前文\n\n<!-- agent-metrics:begin -->\n新内容\n<!-- agent-metrics:end -->\n\n后文\n", once
    assert_equal once, AgentMetricsTrend.inject(once, '新内容')
    assert_raises(RuntimeError) { AgentMetricsTrend.inject("没有标记\n", '新内容') }
    assert_raises(RuntimeError) { AgentMetricsTrend.inject("<!-- agent-metrics:end -->\n<!-- agent-metrics:begin -->\n", '新内容') }
  end

  def test_publish_and_trend_scripts_work_offline
    Dir.mktmpdir do |dir|
      recent = File.join(dir, 'recent.json')
      total = File.join(dir, 'total.json')
      File.write(recent, JSON.generate(MetricsFixture.metrics('since' => 1_789_000_000)))
      File.write(total, JSON.generate(MetricsFixture.metrics('children' => 60)))
      history = File.join(dir, 'history')

      out, err, status = MetricsFixture.run_script('agent_metrics_publish.rb', '--input', recent, '--input-total', total,
                                                   '--history', history, '--window', '14d')
      assert status.success?, "publish 失败：#{out}\n#{err}"
      rows = File.readlines(File.join(history, 'history.jsonl')).map { |line| JSON.parse(line) }
      assert_equal 1, rows.size
      assert_equal '14d', rows[0]['window']
      assert_equal 6, rows[0]['recent']['children']
      assert_equal 60, rows[0]['total']['children']
      refute_nil rows[0]['version']

      out, _err, status = MetricsFixture.run_script('agent_metrics_publish.rb', '--input', recent, '--history', history, '--dry-run')
      assert status.success?
      assert_includes out, '--dry-run'
      assert_equal 1, File.readlines(File.join(history, 'history.jsonl')).size, 'dry-run 不该归档'

      out, err, status = MetricsFixture.run_script('agent_metrics_trend.rb', '--history', history, '--alarm')
      assert_equal 1, status.exitstatus, "有指标没达标时应退出 1：#{out}"
      assert_includes err, '报警：Worker Verified Success 75.0%，目标 ≥ 85%'
      assert_includes out, '| 时间 | 代码 | 窗口 |'

      File.write(recent, JSON.generate(MetricsFixture.on_target))
      _out, _err, status = MetricsFixture.run_script('agent_metrics_publish.rb', '--input', recent, '--history', history)
      assert status.success?
      _out, err, status = MetricsFixture.run_script('agent_metrics_trend.rb', '--history', history, '--alarm')
      assert status.success?, "达标后不该报警：#{err}"
    end
  end
end
