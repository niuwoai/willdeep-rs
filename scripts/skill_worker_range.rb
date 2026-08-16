#!/usr/bin/env ruby
# frozen_string_literal: true

# 小上下文工种的实弹靶场驱动器。
#
# 干三件事：从 ~/.willdeep/config.toml 取出凭据、跑 willdeep-core 里的
# live-fire 测试、把结果渲染成 JSON + Markdown 两份报告。
#
# 它花真钱：每个样本都会真的调用 Provider。默认十个样本。
#
#   ruby scripts/skill_worker_range.rb
#   ruby scripts/skill_worker_range.rb --model glm-5 --cases test_off_by_one,build_missing_mut
#
# 凭据只在进程内传给 cargo，不打印、不写进报告。

require 'json'
require 'fileutils'
require 'optparse'

options = {
  model: ENV['WILLDEEP_RANGE_MODEL'] || 'glm-5',
  cases: ENV['WILLDEEP_RANGE_CASES'],
  config: File.join(ENV['WILLDEEP_HOME'] || File.join(Dir.home, '.willdeep'), 'config.toml'),
  out: File.join(Dir.pwd, 'target', 'skill-worker-range')
}

OptionParser.new do |parser|
  parser.banner = 'Usage: ruby scripts/skill_worker_range.rb [options]'
  parser.on('--model MODEL', '派工用的模型，默认 glm-5') { |value| options[:model] = value }
  parser.on('--cases LIST', '只跑这些样本，逗号分隔') { |value| options[:cases] = value }
  parser.on('--config PATH', 'willdeep 配置文件路径') { |value| options[:config] = value }
  parser.on('--out DIR', '报告输出目录') { |value| options[:out] = value }
  parser.on('--report-only', '不派工，只把已有的 range.json 重新渲染成 Markdown') { options[:report_only] = true }
end.parse!

# 只认默认 provider 那一段的 api_base / api_key。配置里可能有多个 provider，
# 猜错一个就是拿错凭据打错端点，还不如报错。
def provider_credentials(path)
  raise "配置文件不存在：#{path}" unless File.exist?(path)

  # 配置里有中文注释，默认外部编码可能是 US-ASCII，读进来就炸。
  text = File.read(path, encoding: 'UTF-8')
  default = text[/^\s*default_provider\s*=\s*"([^"]+)"/, 1]
  raise "#{path} 里没有 default_provider" unless default

  section = text[/^\s*\[providers\.#{Regexp.escape(default)}\]\s*$(.*?)(?=^\s*\[|\z)/m, 1]
  raise "#{path} 里没有 [providers.#{default}] 段" unless section

  base = section[/^\s*api_base\s*=\s*"([^"]+)"/, 1]
  key = section[/^\s*api_key\s*=\s*"([^"]+)"/, 1]
  key ||= ENV[section[/^\s*api_key_env\s*=\s*"([^"]+)"/, 1].to_s]
  raise "provider #{default} 缺 api_base" unless base
  raise "provider #{default} 缺 api_key（或 api_key_env 指向的环境变量为空）" if key.to_s.strip.empty?

  [default, base, key]
end

# cargo test 的工作目录是 crate 目录，不是仓库根：相对路径会把报告丢到
# crates/willdeep-core/target 下面，Ruby 再也找不回来。
options[:out] = File.expand_path(options[:out])
FileUtils.mkdir_p(options[:out])
json_path = File.join(options[:out], 'range.json')
md_path = File.join(options[:out], 'range.md')

unless options[:report_only]
  provider_name, api_base, api_key = provider_credentials(options[:config])
  puts "provider: #{provider_name} (#{api_base})"
  puts "model:    #{options[:model]}"
  puts "cases:    #{options[:cases] || 'all'}"
  puts '开跑。每个样本都会真的调用 Provider，慢是正常的。'

  env = {
    'WILLDEEP_RANGE_API_BASE' => api_base,
    'WILLDEEP_RANGE_API_KEY' => api_key,
    'WILLDEEP_RANGE_MODEL' => options[:model],
    'WILLDEEP_RANGE_OUT' => json_path
  }
  env['WILLDEEP_RANGE_CASES'] = options[:cases] if options[:cases]

  command = %w[cargo test -p willdeep-core --lib livefire::skill_worker_range --
               --ignored --nocapture --test-threads=1]
  ok = system(env, *command)

  unless File.exist?(json_path)
    warn "靶场没有产出报告（cargo 退出状态：#{ok.inspect}）。上面的输出就是原因。"
    exit 1
  end
end

abort "没有可渲染的报告：#{json_path}" unless File.exist?(json_path)

# 报告里有中文（目标、约束、错误原文），按 UTF-8 读，别看外部编码脸色。
# 报告里有中文（目标、约束、错误原文），按 UTF-8 读，别看外部编码脸色。
report = JSON.parse(File.read(json_path, encoding: 'UTF-8'))
cases = report['cases']
verified_cases = cases.reject { |row| row['report_only'] }
report_cases = cases.select { |row| row['report_only'] }
verified = verified_cases.count { |row| row['verified_success'] }
cheated = verified_cases.count { |row| row['verifier_passed'] && !row['tests_intact'] }
mis_seeded = cases.reject { |row| row['seeded_red'] }.map { |row| row['case'] }
attempts = cases.map { |row| row['attempts'] }.sum
tokens = cases.map { |row| row['input_tokens'].to_i + row['output_tokens'].to_i }.sum
seconds = cases.map { |row| row['seconds'].to_f }.sum
claims = report_cases.map { |row| row['claims_checked'].to_i }.sum
bad_claims = report_cases.map { |row| row['claims_unverifiable'].to_i }.sum
answered = report_cases.count { |row| row['expectation_met'] }

def ratio(part, total)
  # 分母为 0 打 `-`：什么都没跑和什么都没通过是两件事。
  total.zero? ? '-' : format('%d/%d (%.0f%%)', part, total, part * 100.0 / total)
end

lines = []
lines << '# 小上下文工种实弹靶场'
lines << ''
lines << "- 模型：`#{report['model']}`"
lines << "- 样本：#{cases.size}（可验证 #{verified_cases.size} · 只读 #{report_cases.size}）"
lines << "- **Worker Verified Success**：#{ratio(verified, verified_cases.size)}（verifier 通过**且**测试块未被改动）"
lines << "- verifier 通过但改了测试（作弊）：#{cheated}"
lines << "- **只读工种引用准确率**：#{ratio(claims - bad_claims, claims)}（报告点名的路径/行号/commit 里，真实存在的）"
lines << "- **只读工种答对率**：#{ratio(answered, report_cases.size)}（引用真实 ≠ 答对，这一项单独算）"
lines << "- 平均尝试次数：#{cases.empty? ? '-' : format('%.2f', attempts.to_f / cases.size)}"
lines << "- 总 token：#{tokens}；总耗时：#{format('%.1f', seconds)}s"
lines << "- 派工前 verifier 未变红的样本（无效样本）：#{mis_seeded.empty? ? '无' : mis_seeded.join(', ')}"
lines << ''
lines << '| 样本 | 工种 | 判定 | 尝试 | 测试未被改 | 引用核对 | 答对 | tokens | 秒 | 错误 |'
lines << '|---|---|---|---:|---|---|---|---:|---:|---|'
cases.each do |row|
  verdict = case row['verifier_passed']
            when true then '✅ passed'
            when false then '❌ failed'
            else '— 只读'
            end
  checked = row['claims_checked'].to_i
  citations = checked.zero? ? '-' : "#{checked - row['claims_unverifiable'].to_i}/#{checked}"
  answer = row['report_only'] ? (row['expectation_met'] ? '是' : '**否**') : '-'
  error = row['error'].to_s.gsub(/\s+/, ' ')[0, 80]
  lines << format('| `%s` | %s | %s | %d | %s | %s | %s | %d | %.1f | %s |',
                  row['case'], row['profile'], verdict, row['attempts'],
                  row['report_only'] ? '-' : (row['tests_intact'] ? '是' : '**否**'),
                  citations, answer,
                  row['input_tokens'].to_i + row['output_tokens'].to_i,
                  row['seconds'].to_f, error.empty? ? '' : "`#{error}`")
end
lines << ''

File.write(md_path, lines.join("\n"))
puts lines.join("\n")
puts
puts "JSON: #{json_path}"
puts "Markdown: #{md_path}"
