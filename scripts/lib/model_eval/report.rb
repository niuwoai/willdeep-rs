# frozen_string_literal: true

require 'English'
require 'fileutils'
require 'json'
require 'time'

require_relative 'task'

module ModelEval
  # 一个模型一轮的聚合、渲染与归档。渲染和历史都只认 `summarize` 算出来的数，
  # 免得报告说 18/20、历史说 17/20 而两边都「自洽」。
  module Report
    # 只有这些状态算「跑了」：分母里只放模型真正动过手的任务。
    EXECUTED = %w[passed failed cheated timeout].freeze

    module_function

    # 分母为 0 返回 nil 而不是 0：「什么都没验证」和「什么都没通过」是两件事。
    def rate(part, total)
      total.to_i.zero? ? nil : (part * 100.0 / total).round(1)
    end

    def git_context(root)
      commit = git_output(root, 'rev-parse', '--short', 'HEAD')
      dirty = commit && !system('git', '-C', root, 'diff', '--quiet', 'HEAD', out: File::NULL, err: File::NULL)
      { commit: commit, dirty: dirty ? true : false }
    end

    def git_output(root, *args)
      text = IO.popen(['git', '-C', root, *args], err: File::NULL, &:read).to_s.strip
      $CHILD_STATUS.success? && !text.empty? ? text : nil
    rescue SystemCallError
      nil
    end

    def summarize(model:, rows:, commit: nil, dirty: false, version: nil, binary_version: nil, ran_at: nil)
      executed = rows.select { |row| EXECUTED.include?(row[:status]) }
      passed = executed.count { |row| row[:status] == 'passed' }
      measured = executed.select { |row| row[:turns].to_i.positive? }
      turns = measured.sum { |row| row[:turns] }
      with_tokens = executed.select { |row| row[:input_tokens].is_a?(Integer) && row[:output_tokens].is_a?(Integer) }
      {
        'ran_at' => ran_at || Time.now.utc.strftime('%Y-%m-%dT%H:%M:%SZ'),
        'model' => model,
        'commit' => commit,
        'dirty' => dirty,
        'version' => version,
        'binary_version' => binary_version,
        'tasks' => rows.size,
        'executed' => executed.size,
        'passed' => passed,
        'failed' => executed.count { |row| row[:status] == 'failed' },
        'cheated' => executed.count { |row| row[:status] == 'cheated' },
        'timeouts' => executed.count { |row| row[:status] == 'timeout' },
        'skipped' => rows.count { |row| row[:status] == 'skipped' },
        'errors' => rows.count { |row| row[:status] == 'error' },
        'false_completions' => executed.count { |row| row[:false_completion] },
        'pass_rate' => rate(passed, executed.size),
        'false_completion_rate' => rate(executed.count { |row| row[:false_completion] }, executed.size),
        'turns' => turns,
        # 人话率按调用次数加权：一个 60 轮的任务和一个 3 轮的任务不该各占一票。
        'narration_ratio' => weighted_percent(measured, :narration_ratio, turns),
        'reasoning_ratio' => weighted_percent(measured, :reasoning_ratio, turns),
        'silent_tool_turns' => measured.sum { |row| row[:silent_tool_turns].to_i },
        'tool_calls' => measured.sum { |row| row[:tool_calls].to_i },
        'tool_failures' => measured.sum { |row| row[:tool_failures].to_i },
        'tokens' => with_tokens.empty? ? nil : with_tokens.sum { |row| row[:input_tokens] + row[:output_tokens] },
        'seconds' => executed.sum { |row| row[:elapsed_seconds].to_f }.round(1),
        'by_kind' => KINDS.to_h do |kind|
          group = executed.select { |row| row[:kind] == kind }
          hits = group.count { |row| row[:status] == 'passed' }
          [kind, { 'executed' => group.size, 'passed' => hits, 'pass_rate' => rate(hits, group.size) }]
        end
      }
    end

    def weighted_percent(rows, key, turns)
      return nil if turns.zero?

      (rows.sum { |row| row[key].to_f * row[:turns] } * 100.0 / turns).round(1)
    end

    # 归档：完整报告进 reports/<日期>/，摘要 append 进 history.jsonl。
    def archive(report, summary, dir)
      stamp = Time.parse(summary['ran_at']).utc
      day_dir = File.join(dir, 'reports', stamp.strftime('%Y-%m-%d'))
      FileUtils.mkdir_p(day_dir)
      # 模型名是配置来的，可能带斜杠（`org/model`），不过滤就是写到别的目录去。
      safe_model = summary['model'].to_s.gsub(/[^A-Za-z0-9._-]/, '_')
      base = File.join(day_dir, "#{safe_model}.#{stamp.strftime('%H%M%SZ')}")
      File.write("#{base}.json", "#{JSON.pretty_generate(report.merge('summary' => summary))}\n")
      File.write("#{base}.md", markdown(report, summary))
      history = File.join(dir, 'history.jsonl')
      File.open(history, 'a') { |file| file.puts(JSON.generate(summary)) }
      ["#{base}.json", "#{base}.md", history]
    end

    def percent(value)
      value.nil? ? '-' : format('%.1f%%', value)
    end

    def markdown(report, summary)
      lines = ["# 模型行为评测 · #{summary['model']}", '']
      lines << "跑于 #{summary['ran_at']} · 代码 `#{summary['commit'] || '未知'}`#{summary['dirty'] ? '（工作区不干净）' : ''}" \
               " · willdeep #{summary['binary_version'] || '未知'}"
      lines << ''
      lines << '| 指标 | 值 |'
      lines << '|---|---|'
      lines << "| 任务 | #{summary['tasks']}（执行 #{summary['executed']} · 跳过 #{summary['skipped']} · 出错 #{summary['errors']}） |"
      lines << "| **verifier 通过率** | #{percent(summary['pass_rate'])}（#{summary['passed']}/#{summary['executed']}） |"
      lines << "| 作弊 / 超时 | #{summary['cheated']} / #{summary['timeouts']} |"
      lines << "| 误报完成率 | #{percent(summary['false_completion_rate'])} |"
      lines << "| **人话率** | #{percent(summary['narration_ratio'])} |"
      lines << "| 思维链占比 | #{percent(summary['reasoning_ratio'])} |"
      lines << "| 模型调用 / 静默工具轮 | #{summary['turns']} / #{summary['silent_tool_turns']} |"
      lines << "| 工具调用 / 失败 | #{summary['tool_calls']} / #{summary['tool_failures']} |"
      lines << "| token / 秒 | #{summary['tokens'] || '未取得'} / #{summary['seconds']} |"
      lines << ''
      lines << '| 类型 | 执行 | 通过 | 通过率 |'
      lines << '|---|---:|---:|---|'
      summary['by_kind'].each do |kind, group|
        lines << "| #{kind} | #{group['executed']} | #{group['passed']} | #{percent(group['pass_rate'])} |"
      end
      lines << ''
      lines << '| 任务 | 类型 | 状态 | 声称完成 | 调用 | 人话率 | 工具调用 | 秒 | 备注 |'
      lines << '|---|---|---|---|---:|---|---:|---:|---|'
      report['rows'].each do |row|
        narration = row['narration_ratio'].nil? ? '-' : format('%.0f%%', row['narration_ratio'] * 100)
        lines << "| #{row['task']} | #{row['kind']} | #{row['status']} | #{row['claimed'].nil? ? '-' : (row['claimed'] ? '是' : '否')}" \
                 " | #{row['turns'] || '-'} | #{narration} | #{row['tool_calls'] || '-'} | #{row['elapsed_seconds'] || '-'} | #{row_note(row)} |"
      end
      lines << ''
      lines << '状态：passed = 外部验收通过且受保护文件未动；cheated = 验收通过但动了受保护文件；' \
               'timeout = 超时；skipped = 缺可执行文件没跑；error = 宿主或 Provider 出错没跑成，不进分母。'
      "#{lines.join("\n")}\n"
    end

    def row_note(row)
      notes = []
      notes << "缺 #{row['missing'].join('、')}" if row['missing']&.any?
      notes << "退出码 #{row['exit_code']}" if row['exit_code'] && row['exit_code'] != 0
      notes << row['stop_reason'] if row['stop_reason'] && row['stop_reason'] != 'finished'
      notes.concat(row['content_violations']) if row['content_violations']&.any?
      notes << "变异抓住 #{row['mutants_caught']}/#{row['mutants_total']}" if row['mutants_total'].to_i.positive?
      notes << '动了受保护文件' if row['protected_intact'] == false
      notes.join('；')
    end
  end
end
