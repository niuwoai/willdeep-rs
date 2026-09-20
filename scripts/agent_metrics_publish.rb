#!/usr/bin/env ruby
# frozen_string_literal: true

# 给线上派工指标拍一张快照，归档进 bench/agent-metrics/history.jsonl。
#
#   ruby scripts/agent_metrics_publish.rb                   # 近 7 天 + 累计，各调一次 CLI
#   ruby scripts/agent_metrics_publish.rb --window 14d
#   ruby scripts/agent_metrics_publish.rb --dry-run         # 只打印，不归档
#   ruby scripts/agent_metrics_publish.rb --input recent.json --input-total total.json   # 离线：读现成的 JSON
#
# 它不联网、不花钱、不调 Provider：只跑 `willdeep daemon agent-metrics --json`，
# 那条命令读的是本机 Runtime 里的 agent 记录，只出计数和比率，不出任何正文、路径或 ID。
# Runtime 没起时 CLI 会顺手把它拉起来。
#
# 归档之后用 `ruby scripts/agent_metrics_trend.rb --inject` 把趋势写回文档。

require 'json'
require 'open3'
require 'optparse'

require_relative 'lib/agent_metrics_report'

REPO_ROOT = File.expand_path('..', __dir__)

module AgentMetricsPublish
  module_function

  def git_output(*args)
    output, status = Open3.capture2('git', '-C', REPO_ROOT, *args, err: File::NULL)
    status.success? ? output.strip : nil
  end

  # 只看已跟踪文件：未跟踪文件本来就不在任何 commit 里，
  # 算进来的话根目录随手放张图就会让每次快照都被标成「不可回放」。
  def dirty?
    !system('git', '-C', REPO_ROOT, 'diff', '--quiet', 'HEAD', out: File::NULL, err: File::NULL)
  end

  def version
    File.read(File.join(REPO_ROOT, 'Cargo.toml'), encoding: 'UTF-8')[/^version\s*=\s*"([^"]+)"/, 1]
  end

  # 跑一次 CLI 拿 JSON。stderr 原样透传：Runtime 起不来的原因该让人看见。
  def fetch(bin, since: nil)
    args = [bin, 'daemon', 'agent-metrics', '--json']
    args += ['--since', since] if since
    output, status = Open3.capture2(*args)
    raise "`#{args.join(' ')}` 退出码 #{status.exitstatus}" unless status.success?

    JSON.parse(output)
  end

  def read_input(path)
    JSON.parse(File.read(path, encoding: 'UTF-8'))
  end
end

if $PROGRAM_NAME == __FILE__
  options = {
    window: '7d',
    history: File.join(REPO_ROOT, 'bench', 'agent-metrics'),
    bin: ENV.fetch('WILLDEEP_BIN', 'willdeep'),
    input: nil,
    input_total: nil,
    dry_run: false
  }
  OptionParser.new do |parser|
    parser.banner = 'Usage: ruby scripts/agent_metrics_publish.rb [options]'
    parser.on('--window WINDOW', '趋势看的窗口，传给 --since，默认 7d') { |v| options[:window] = v }
    parser.on('--history DIR', '归档目录，默认 bench/agent-metrics') { |v| options[:history] = v }
    parser.on('--bin PATH', 'willdeep 可执行文件，默认 $WILLDEEP_BIN 或 PATH 里的 willdeep') { |v| options[:bin] = v }
    parser.on('--input FILE', '不调 CLI，窗口指标读这个 JSON') { |v| options[:input] = v }
    parser.on('--input-total FILE', '不调 CLI，累计指标读这个 JSON（缺省与 --input 同一份）') { |v| options[:input_total] = v }
    parser.on('--dry-run', '只打印快照，不写 history.jsonl') { options[:dry_run] = true }
  end.parse!

  recent, total =
    if options[:input]
      [AgentMetricsPublish.read_input(options[:input]),
       AgentMetricsPublish.read_input(options[:input_total] || options[:input])]
    else
      [AgentMetricsPublish.fetch(options[:bin], since: options[:window]),
       AgentMetricsPublish.fetch(options[:bin])]
    end

  summary = AgentMetricsReport.summarize(
    recent: recent,
    total: total,
    window: options[:window],
    commit: AgentMetricsPublish.git_output('rev-parse', '--short', 'HEAD'),
    dirty: AgentMetricsPublish.dirty?,
    version: AgentMetricsPublish.version
  )

  puts JSON.pretty_generate(summary)
  warn '注意：工作区不干净，这张快照挂在一个没提交的状态上。' if summary['dirty']

  if options[:dry_run]
    puts '（--dry-run：没有归档）'
  else
    path = AgentMetricsReport.archive(summary, options[:history])
    puts "已归档：#{path}"
  end
end
