# frozen_string_literal: true

# 线上派工指标快照的整理与归档。
#
# 数字不在这里算：`willdeep daemon agent-metrics --json` 只有一条算路，
# 这里只负责把它的输出裁成固定形状、贴上 commit / 版本 / 时间，追加进历史。
# 裁形状是刻意的——CLI 以后多一个字段，不该不经 review 就悄悄进了公开历史。

require 'json'
require 'fileutils'

module AgentMetricsReport
  COUNT_KEYS = %w[
    children workers standard deep verified_runs passed unverified_runs
    attempts claims_checked claims_unverifiable
  ].freeze
  RATE_KEYS = %w[
    deep_share skill_coverage worker_verified_success escalation_rate
    citation_accuracy attempts_per_verified_run
  ].freeze

  # 目标与 crates/willdeep-cli/src/agent_metrics.rs 里的常量同源：
  # 数字离开 CLI 时带着的那根标尺，到了文档里还是同一根。
  TARGETS = {
    'deep_share' => { label: 'Deep Share', direction: :max, value: 5.0 },
    'skill_coverage' => { label: 'Skill Coverage', direction: :min, value: 50.0 },
    'worker_verified_success' => { label: 'Worker Verified Success', direction: :min, value: 85.0 },
    'escalation_rate' => { label: 'Escalation Rate', direction: :max, value: 15.0 }
  }.freeze

  module_function

  # 只留认识的字段；比率转成 Float，分母为 0 的 null 原样保留成 nil。
  def slice(metrics)
    raise ArgumentError, "指标不是对象：#{metrics.class}" unless metrics.is_a?(Hash)

    counts = COUNT_KEYS.to_h { |key| [key, Integer(metrics.fetch(key))] }
    rates = RATE_KEYS.to_h { |key| [key, metrics[key].nil? ? nil : Float(metrics[key])] }
    counts.merge(rates)
  end

  # 一次快照：窗口内的数（趋势看它）＋累计的数（头一行提一句）。
  def summarize(recent:, total:, window:, commit: nil, dirty: false, version: nil, ran_at: nil)
    {
      'ran_at' => ran_at || Time.now.utc.strftime('%Y-%m-%dT%H:%M:%SZ'),
      # 没有 commit 的快照是无主的：派工提示词改了、路由改了，都归因不到这一行上。
      'commit' => commit,
      'dirty' => dirty,
      'version' => version,
      'window' => window,
      'recent' => slice(recent),
      'total' => slice(total)
    }
  end

  # 追加进 history.jsonl（append-only），返回路径。
  def archive(summary, dir)
    FileUtils.mkdir_p(dir)
    path = File.join(dir, 'history.jsonl')
    File.open(path, 'a') { |file| file.puts(JSON.generate(summary)) }
    path
  end

  # 一组指标里没达标的那些。分母为 0（nil）不算没达标——它是「没得比」。
  def misses(metrics)
    TARGETS.filter_map do |key, target|
      value = metrics[key]
      next if value.nil?

      failed = target[:direction] == :min ? value < target[:value] : value > target[:value]
      next unless failed

      { key: key, label: target[:label], value: value, direction: target[:direction], target: target[:value] }
    end
  end

  def target_text(key)
    target = TARGETS[key]
    return '—' unless target

    "#{target[:direction] == :min ? '≥' : '≤'} #{format('%g', target[:value])}%"
  end
end
