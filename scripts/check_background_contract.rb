#!/usr/bin/env ruby
# frozen_string_literal: true

# 后台任务合同 v1 的跨仓库漂移检查。
#
# canonical 在 Xedit 仓库（docs/BACKGROUND_TASK_CONTRACT.md 与
# docs/contracts/background-task-notification.v1.txt）。本仓库存金样副本，并把附录 A
# 的提示词原文嵌进 prompt.rs。两份各自有测试，但测试只看得见自己仓库——
# WORKER_ROUTING_CONTRACT.json 就是这样两边悄悄漂开的。
#
# 用法：ruby scripts/check_background_contract.rb [--xedit PATH]
# 找不到 Xedit 仓库（例如 CI 上）时跳过并返回 0；找到了就必须一致。

require 'optparse'

FIXTURE = 'docs/contracts/background-task-notification.v1.txt'
CONTRACT_DOC = 'docs/BACKGROUND_TASK_CONTRACT.md'
PROMPT_SOURCE = 'crates/willdeep-core/src/prompt.rs'

root = File.expand_path('..', __dir__)
xedit = ENV.fetch('XEDIT_REPO', File.expand_path('~/Sites/Xedit'))
OptionParser.new { |parser| parser.on('--xedit PATH') { |path| xedit = File.expand_path(path) } }.parse!

canonical_fixture = File.join(xedit, FIXTURE)
canonical_doc = File.join(xedit, CONTRACT_DOC)
unless File.file?(canonical_fixture) && File.file?(canonical_doc)
  puts "skipped: canonical contract not found under #{xedit}"
  exit 0
end

failures = []

# 金样的注释头允许两边各写各的，正文必须逐字相同。
body = ->(path) { File.read(path, encoding: 'UTF-8').lines.drop_while { |line| line.start_with?('#') }.join }
local_fixture = File.join(root, FIXTURE)
failures << "#{FIXTURE} differs from #{canonical_fixture}" unless body.call(local_fixture) == body.call(canonical_fixture)

appendix = File.read(canonical_doc, encoding: 'UTF-8')[/## 附录 A[^\n]*\n+```\n(.+?)\n```/m, 1]
if appendix.nil?
  failures << "#{canonical_doc} has no appendix A prompt block"
elsif !File.read(File.join(root, PROMPT_SOURCE), encoding: 'UTF-8').include?(appendix.strip)
  failures << "#{PROMPT_SOURCE} does not embed appendix A of #{canonical_doc} verbatim"
end

if failures.empty?
  puts "ok: background task contract matches #{xedit}"
else
  failures.each { |failure| warn "drift: #{failure}" }
  exit 1
end
