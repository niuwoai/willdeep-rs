#!/usr/bin/env ruby
# frozen_string_literal: true

# 合成等待任务，只量调度器；不会把这个数字当成真实模型提速。
require 'optparse'
require_relative 'lib/model_eval/execution'

options = { out: File.expand_path('../target/eval-execution-benchmark', __dir__) }
OptionParser.new do |parser|
  parser.on('--out PREFIX', '输出 JSON / Markdown') { |value| options[:out] = File.expand_path(value) }
end.parse!
tasks = (0...24).to_a
timings = [1, 4].to_h do |jobs|
  start = Process.clock_gettime(Process::CLOCK_MONOTONIC)
  result = ModelEval::Execution.run(tasks, jobs: jobs) { |task| sleep(0.02); task }
  raise '任务结果缺失或顺序错误' unless result == tasks

  [jobs.to_s, (Process.clock_gettime(Process::CLOCK_MONOTONIC) - start).round(4)]
end
report = { scope: 'synthetic_scheduler_only', tasks: tasks.size, seconds: timings,
           speedup: (timings['1'] / timings['4']).round(2), provider_calls: 0 }
FileUtils.mkdir_p(File.dirname(options[:out]))
File.write("#{options[:out]}.json", "#{JSON.pretty_generate(report)}\n")
File.write("#{options[:out]}.md", "# 评测调度器合成基准\n\n24 个隔离等待任务；Provider 调用 0 次。\n\n串行 #{timings['1']} 秒；四路 #{timings['4']} 秒；#{report[:speedup]} 倍。\n\n只证明调度器能并发，不代表真实模型或 Agent 的加速比。\n")
puts JSON.generate(report)
