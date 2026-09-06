#!/usr/bin/env ruby
# Enforce the repository's hand-maintained source/document size boundary.
require 'fileutils'
require 'json'
require 'optparse'
require 'tmpdir'

MAX_LINES = 3_000
REVIEW_LINES = 2_000
SOURCE_EXTENSIONS = %w[.rs .ts .tsx .js .mjs .cjs .rb .md].freeze

output = File.join(Dir.tmpdir, 'willdeep-source-size')
OptionParser.new { |parser| parser.on('--output PATH') { |path| output = path } }.parse!
root = File.expand_path('..', __dir__)
paths = IO.popen(['git', '-C', root, 'ls-files', '-z', '--cached', '--others', '--exclude-standard'], &:read).split("\0").uniq
files = paths.filter_map do |relative|
  next unless SOURCE_EXTENSIONS.include?(File.extname(relative))
  next if relative.split('/').any? { |segment| %w[node_modules target dist].include?(segment) }

  path = File.join(root, relative)
  next unless File.file?(path)

  { path: relative, lines: File.foreach(path).count }
end
violations = files.select { |file| file[:lines] > MAX_LINES }
review = files.select { |file| file[:lines] > REVIEW_LINES }.sort_by { |file| -file[:lines] }
report = { passed: violations.empty?, max_lines: MAX_LINES, reviewed_files: files.size,
           violations: violations, responsibility_review: review }
FileUtils.mkdir_p(File.dirname(output))
File.write("#{output}.json", JSON.pretty_generate(report) + "\n")
markdown = ["# 源码行数检查", '', "结果：#{report[:passed] ? '通过' : '失败'}；检查 #{files.size} 个手写文件，上限 #{MAX_LINES} 行。", '',
            '| 文件 | 行数 | 状态 |', '| --- | ---: | --- |']
review.each { |file| markdown << "| #{file[:path]} | #{file[:lines]} | #{file[:lines] > MAX_LINES ? '必须拆分' : '需评估职责'} |" }
File.write("#{output}.md", markdown.join("\n") + "\n")
puts JSON.generate(report)
exit(violations.empty? ? 0 : 1)
