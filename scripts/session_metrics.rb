#!/usr/bin/env ruby
# frozen_string_literal: true

# 从会话记录算「模型行为」指标：人话率、工具调用与失败、思维链占比、平均轮次。
#
# 只读本机 ~/.willdeep/sessions/*.json（或指定目录 / 文件），只输出计数，不输出任何
# 消息正文。提示词或路由改动前后各跑一次，拿数字说话，而不是肉眼看。
#
# 用法：
#   ruby scripts/session_metrics.rb                      # 默认 ~/.willdeep/sessions，最近 20 个会话
#   ruby scripts/session_metrics.rb --limit 50           # 最近 50 个
#   ruby scripts/session_metrics.rb --dir /path/to/sessions
#   ruby scripts/session_metrics.rb --json out.json --markdown out.md
#   ruby scripts/session_metrics.rb --since 2026-09-19   # 只看这天之后改过的会话文件
#
# 指标定义：
#   narration_ratio  有正文的 assistant 消息 / 全部 assistant 消息（「人话率」）
#   tool_calls       assistant 消息里的工具调用总数
#   tool_failures    工具结果消息里以错误开头的条数（启发式：内容以 "Error" / "错误" 开头）
#   reasoning_ratio  带思维链的 assistant 消息占比
#   turns            assistant 消息数（≈ 模型调用次数）
#   silent_tool_turns 只带工具调用、正文为空的 assistant 消息数

require "json"
require "optparse"
require "time"

Encoding.default_external = Encoding::UTF_8
Encoding.default_internal = Encoding::UTF_8

options = {
  dir: File.expand_path("~/.willdeep/sessions"),
  limit: 20,
  since: nil,
  json: nil,
  markdown: nil,
}

OptionParser.new do |parser|
  parser.banner = "Usage: session_metrics.rb [options] [session.json ...]"
  parser.on("--dir DIR", "会话目录（默认 ~/.willdeep/sessions）") { |v| options[:dir] = File.expand_path(v) }
  parser.on("--limit N", Integer, "最近 N 个会话（默认 20）") { |v| options[:limit] = v }
  parser.on("--since DATE", "只统计该日期之后修改过的会话文件") { |v| options[:since] = Time.parse(v) }
  parser.on("--json PATH", "把结果写成 JSON") { |v| options[:json] = v }
  parser.on("--markdown PATH", "把结果写成 Markdown") { |v| options[:markdown] = v }
end.parse!

ERROR_PREFIXES = ["error", "错误", "failed", "失败"].freeze

def tool_failure?(message)
  return false unless message["role"] == "tool"

  head = message["content"].to_s.lstrip.downcase
  ERROR_PREFIXES.any? { |prefix| head.start_with?(prefix) }
end

def metrics_for(path)
  session = JSON.parse(File.read(path, encoding: "UTF-8"))
  messages = session["messages"] || []
  assistant = messages.select { |m| m["role"] == "assistant" }
  with_text = assistant.count { |m| !m["content"].to_s.strip.empty? }
  with_tools = assistant.select { |m| (m["tool_calls"] || []).any? }
  silent_tool_turns = with_tools.count { |m| m["content"].to_s.strip.empty? }
  tool_calls = with_tools.sum { |m| m["tool_calls"].size }
  {
    session: File.basename(path, ".json")[0, 8],
    model: session["model"],
    updated_at: File.mtime(path).iso8601,
    messages: messages.size,
    turns: assistant.size,
    narration_ratio: assistant.empty? ? nil : (with_text.to_f / assistant.size).round(3),
    silent_tool_turns: silent_tool_turns,
    tool_calls: tool_calls,
    tool_failures: messages.count { |m| tool_failure?(m) },
    reasoning_ratio: assistant.empty? ? nil : (assistant.count { |m| !m["reasoning"].to_s.strip.empty? }.to_f / assistant.size).round(3),
  }
rescue JSON::ParserError => error
  warn "skip #{path}: #{error.message}"
  nil
end

files = if ARGV.empty?
  Dir[File.join(options[:dir], "*.json")]
    .select { |path| options[:since].nil? || File.mtime(path) >= options[:since] }
    .sort_by { |path| -File.mtime(path).to_i }
    .first(options[:limit])
else
  ARGV
end

rows = files.filter_map { |path| metrics_for(path) }.reject { |row| row[:turns].zero? }
abort "no sessions with assistant messages found" if rows.empty?

def weighted_ratio(rows, key, weight)
  total = rows.sum { |r| r[weight] }
  return nil if total.zero?

  (rows.sum { |r| (r[key] || 0) * r[weight] } / total).round(3)
end

summary = {
  generated_at: Time.now.iso8601,
  sessions: rows.size,
  turns: rows.sum { |r| r[:turns] },
  tool_calls: rows.sum { |r| r[:tool_calls] },
  tool_failures: rows.sum { |r| r[:tool_failures] },
  narration_ratio: weighted_ratio(rows, :narration_ratio, :turns),
  reasoning_ratio: weighted_ratio(rows, :reasoning_ratio, :turns),
  silent_tool_turns: rows.sum { |r| r[:silent_tool_turns] },
  by_model: rows.group_by { |r| r[:model] || "unknown" }.transform_values do |group|
    {
      sessions: group.size,
      turns: group.sum { |r| r[:turns] },
      narration_ratio: weighted_ratio(group, :narration_ratio, :turns),
      reasoning_ratio: weighted_ratio(group, :reasoning_ratio, :turns),
      tool_calls: group.sum { |r| r[:tool_calls] },
      tool_failures: group.sum { |r| r[:tool_failures] },
    }
  end,
}

report = { summary: summary, sessions: rows }

markdown = +"# 会话模型行为指标\n\n"
markdown << "生成于 #{summary[:generated_at]} · #{summary[:sessions]} 个会话 · #{summary[:turns]} 次模型调用\n\n"
markdown << "| 模型 | 会话 | 调用 | 人话率 | 思维链占比 | 工具调用 | 工具失败 |\n|---|---|---|---|---|---|---|\n"
summary[:by_model].sort_by { |_, v| -v[:turns] }.each do |model, v|
  markdown << "| #{model} | #{v[:sessions]} | #{v[:turns]} | #{v[:narration_ratio]} | #{v[:reasoning_ratio]} | #{v[:tool_calls]} | #{v[:tool_failures]} |\n"
end
markdown << "| **合计** | #{summary[:sessions]} | #{summary[:turns]} | #{summary[:narration_ratio]} | #{summary[:reasoning_ratio]} | #{summary[:tool_calls]} | #{summary[:tool_failures]} |\n\n"
markdown << "## 逐会话\n\n| 会话 | 模型 | 调用 | 人话率 | 静默工具轮 | 工具调用 | 工具失败 | 更新时间 |\n|---|---|---|---|---|---|---|---|\n"
rows.each do |r|
  markdown << "| #{r[:session]} | #{r[:model] || 'unknown'} | #{r[:turns]} | #{r[:narration_ratio]} | #{r[:silent_tool_turns]} | #{r[:tool_calls]} | #{r[:tool_failures]} | #{r[:updated_at]} |\n"
end

File.write(options[:json], JSON.pretty_generate(report)) if options[:json]
File.write(options[:markdown], markdown) if options[:markdown]
puts markdown
