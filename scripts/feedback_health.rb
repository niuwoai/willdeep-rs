#!/usr/bin/env ruby
# frozen_string_literal: true

require 'optparse'
require 'fileutils'
require_relative 'lib/feedback_health'

if $PROGRAM_NAME == __FILE__
  options = { home: ENV.fetch('WILLDEEP_HOME', File.join(Dir.home, '.willdeep')) }
  OptionParser.new do |parser|
    parser.on('--home DIR', '只读本机 WillDeep 数据目录') { |value| options[:home] = File.expand_path(value) }
    parser.on('--out PREFIX', '输出 PREFIX.json 与 PREFIX.md，权限 0600') { |value| options[:out] = File.expand_path(value) }
    parser.on('--json', '标准输出 JSON') { options[:json] = true }
  end.parse!
  rows, problems = FeedbackHealth.read(File.join(options[:home], 'feedback'))
  report = FeedbackHealth.summarize(rows, problems)
  json = "#{JSON.pretty_generate(report)}\n"
  markdown = FeedbackHealth.markdown(report)
  if options[:out]
    FileUtils.mkdir_p(File.dirname(options[:out]), mode: 0o700)
    { '.json' => json, '.md' => markdown }.each do |extension, text|
      File.open("#{options[:out]}#{extension}", 'w', 0o600) { |file| file.write(text) }
    end
  end
  puts(options[:json] ? json : markdown)
end
