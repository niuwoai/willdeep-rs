# frozen_string_literal: true

# 靶场报告的聚合与归档。
#
# 抽出来是因为同一批数字此前有两条算路：一条渲染 Markdown 给人看，一条写进
# 历史给趋势用。两条算路迟早会分叉，而分叉的那天，报告说 10/10、历史说 9/10，
# 没有任何东西会报警——两边都「自洽」。现在只有这一条。

require 'json'
require 'fileutils'

module RangeReport
  module_function

  # 分母为 0 返回 nil 而不是 0。
  #
  # 这是整个指标体系里最容易被抹掉的一条纪律：「什么都没验证」和「什么都没
  # 通过」是两件事，把前者记成 0% 会让一次空跑看起来像一次全面崩溃，把它记成
  # 100% 则更糟。nil 一路传到渲染层，在 Markdown 里是 `-`，在曲线里是 `·`。
  def rate(part, total)
    total.to_i.zero? ? nil : (part * 100.0 / total).round(1)
  end

  # 一份报告的全部聚合。渲染和归档都只认这里算出来的数。
  def summarize(report, commit: nil, dirty: false, version: nil, ran_at: nil)
    cases = report['cases'] || []
    verifiable = cases.reject { |row| row['report_only'] }
    read_only = cases.select { |row| row['report_only'] }

    # 成功 = verifier 通过 **且** 测试块逐字未改。把测试删了也能变绿，
    # 而那是最省力的通关方式，所以作弊的不进分子——这条口径与
    # `SKILL_WORKERS.md` 的判定纪律第一条同源。
    verified = verifiable.count { |row| row['verified_success'] }
    cheated = verifiable.count { |row| row['verifier_passed'] && !row['tests_intact'] }
    claims = read_only.sum { |row| row['claims_checked'].to_i }
    bad_claims = read_only.sum { |row| row['claims_unverifiable'].to_i }
    answered = read_only.count { |row| row['expectation_met'] }

    {
      'ran_at' => ran_at || Time.now.utc.strftime('%Y-%m-%dT%H:%M:%SZ'),
      'model' => report['model'],
      # 没有 commit 的成绩是无主的：既不知道测的是哪版代码，也没法回答
      # 「这次改动让它变好还是变坏」——而那正是把靶场常态化的全部理由。
      'commit' => commit,
      'dirty' => dirty,
      'version' => version,
      'cases' => cases.size,
      'verified_cases' => verifiable.size,
      'report_cases' => read_only.size,
      'verified' => verified,
      'cheated' => cheated,
      # 派工前 verifier 就没变红的样本：绿着的靶子测不出任何东西，
      # 它进了分母就是在稀释成绩，所以单独点名。
      'mis_seeded' => cases.reject { |row| row['seeded_red'] }.map { |row| row['case'] },
      'claims_checked' => claims,
      'claims_unverifiable' => bad_claims,
      'answered' => answered,
      'attempts' => cases.sum { |row| row['attempts'].to_i },
      'tokens' => cases.sum { |row| row['input_tokens'].to_i + row['output_tokens'].to_i },
      'seconds' => cases.sum { |row| row['seconds'].to_f }.round(1),
      'worker_verified_success' => rate(verified, verifiable.size),
      'citation_accuracy' => rate(claims - bad_claims, claims),
      'report_answer_rate' => rate(answered, read_only.size)
    }
  end

  # 归档一轮成绩：完整报告存一份带时间戳的副本，摘要 append 进 history.jsonl。
  # 返回两个路径，调用方负责打印。
  def archive(report, summary, dir)
    runs_dir = File.join(dir, 'runs')
    FileUtils.mkdir_p(runs_dir)

    # 模型名会出现在文件名里，而模型名是配置来的，可能带斜杠（`org/model`）。
    # 不过滤就是让一次归档写到别的目录去。
    safe_model = summary['model'].to_s.gsub(/[^A-Za-z0-9._-]/, '_')
    slug = "#{summary['ran_at'].gsub(':', '')}-#{safe_model}"
    archive_path = File.join(runs_dir, "#{slug}.json")
    File.write(archive_path, "#{JSON.pretty_generate(report.merge('summary' => summary))}\n")

    history_path = File.join(dir, 'history.jsonl')
    File.open(history_path, 'a') { |file| file.puts(JSON.generate(summary)) }

    [archive_path, history_path]
  end
end
