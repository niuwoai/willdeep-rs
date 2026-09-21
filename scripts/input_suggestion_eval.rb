#!/usr/bin/env ruby
# frozen_string_literal: true

# 下一句预测的实弹评测驱动器。
#
# 从 ~/.willdeep/config.toml 取出默认 provider 的凭据，跑 willdeep-core 里
# `#[ignore]` 的实弹测试（逐样本真调一次模型），把结果写成 JSON + Markdown，
# 归档进 `bench/input-suggestion/runs/`，并向 `history.jsonl` 追加一行摘要。
#
# 它花真钱：每个样本一次小请求。
#
#   ruby scripts/input_suggestion_eval.rb --model deepseek-v4-flash
#   ruby scripts/input_suggestion_eval.rb --rescore bench/input-suggestion/runs/<run>.json
#
# `--rescore` 用于人工填完 `judged` 之后重算那一轮的摘要；它不改旧的 history 行，
# 而是追加一行带 `rescored_from` 的新摘要——history 只追加，不改写。
#
# 凭据只在进程内传给 cargo，不打印、不写进报告、不进归档。

require 'English'
require 'json'
require 'fileutils'
require 'optparse'

require_relative 'lib/suggestion_report'
require_relative 'lib/willdeep_credentials'

REPO_ROOT = File.expand_path('..', __dir__)

options = {
  model: ENV['WILLDEEP_RANGE_MODEL'] || 'glm-5',
  config: File.join(ENV['WILLDEEP_HOME'] || File.join(Dir.home, '.willdeep'), 'config.toml'),
  out: File.join(REPO_ROOT, 'target', 'input-suggestion'),
  history: File.join(REPO_ROOT, 'bench', 'input-suggestion')
}

OptionParser.new do |parser|
  parser.banner = 'Usage: ruby scripts/input_suggestion_eval.rb [options]'
  parser.on('--model MODEL', '预测用的模型，默认 glm-5') { |value| options[:model] = value }
  parser.on('--config PATH', 'willdeep 配置文件路径') { |value| options[:config] = value }
  parser.on('--out DIR', '报告输出目录') { |value| options[:out] = value }
  parser.on('--no-history', '只跑不归档（调试用）') { options[:history] = nil }
  parser.on('--rescore PATH', '重算一份已归档的 run（人工填完 judged 后用）') { |value| options[:rescore] = value }
end.parse!

def git_output(*args)
  text = IO.popen(['git', '-C', REPO_ROOT, *args], err: File::NULL, &:read).to_s.strip
  $CHILD_STATUS.success? && !text.empty? ? text : nil
rescue SystemCallError
  nil
end

def provenance
  {
    commit: git_output('rev-parse', '--short', 'HEAD'),
    # 只看已跟踪文件，与靶场同一口径。
    dirty: !system('git', '-C', REPO_ROOT, 'diff', '--quiet', 'HEAD', out: File::NULL, err: File::NULL),
    version: File.read(File.join(REPO_ROOT, 'Cargo.toml'), encoding: 'UTF-8')[/^version\s*=\s*"([^"]+)"/, 1]
  }
end

def append_history(history_dir, summary)
  FileUtils.mkdir_p(history_dir)
  File.open(File.join(history_dir, 'history.jsonl'), 'a') { |file| file.puts(JSON.generate(summary)) }
end

if options[:rescore]
  run = JSON.parse(File.read(options[:rescore], encoding: 'UTF-8'))
  old = run['summary']
  summary = SuggestionReport.summarize(
    run, commit: old['commit'], dirty: old['dirty'], version: old['version'], ran_at: old['ran_at']
  )
  summary['rescored_from'] = File.basename(options[:rescore])
  summary['rescored_at'] = Time.now.utc.strftime('%Y-%m-%dT%H:%M:%SZ')
  run['summary'] = summary
  File.write(options[:rescore], "#{JSON.pretty_generate(run)}\n")
  append_history(File.dirname(File.dirname(options[:rescore])), summary) if options[:history]
  puts SuggestionReport.markdown(summary, run['cases'])
  exit 0
end

FileUtils.mkdir_p(options[:out])
json_path = File.join(options[:out], 'report.json')
FileUtils.rm_f(json_path)

provider_name, api_base, api_key = WilldeepCredentials.default_provider(options[:config])
puts "provider: #{provider_name} (#{api_base})"
puts "model:    #{options[:model]}"
warn '警告：工作区有未提交的改动，这一轮成绩挂在一个回放不了的状态上。' if provenance[:dirty]

env = {
  'WILLDEEP_RANGE_API_BASE' => api_base,
  'WILLDEEP_RANGE_API_KEY' => api_key,
  'WILLDEEP_RANGE_MODEL' => options[:model],
  'WILLDEEP_SUGGEST_OUT' => json_path
}
command = %w[cargo test -p willdeep-core --lib input_suggestion_livefire::input_suggestion_live_fire --
             --ignored --nocapture --test-threads=1]
ok = system(env, *command)
unless File.exist?(json_path)
  warn "实弹测试没有产出报告（cargo 退出状态：#{ok.inspect}）。上面的输出就是原因。"
  exit 1
end

report = JSON.parse(File.read(json_path, encoding: 'UTF-8'))
report['cases'].each { |row| row['judged'] = nil if row['expect'] == 'suggest' }
meta = provenance
summary = SuggestionReport.summarize(report, commit: meta[:commit], dirty: meta[:dirty], version: meta[:version])
markdown = SuggestionReport.markdown(summary, report['cases'])
File.write(File.join(options[:out], 'report.md'), markdown)
puts markdown

if options[:history]
  stamp = summary['ran_at'].tr(':', '').tr('-', '')
  run_path = File.join(options[:history], 'runs', "#{stamp}-#{options[:model].gsub(/[^\w.-]/, '_')}.json")
  FileUtils.mkdir_p(File.dirname(run_path))
  File.write(run_path, "#{JSON.pretty_generate({ 'summary' => summary, 'model' => report['model'], 'cases' => report['cases'] })}\n")
  append_history(options[:history], summary)
  puts "归档：#{run_path}"
end
