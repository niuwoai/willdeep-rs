# frozen_string_literal: true

require 'digest'
require 'fileutils'
require 'json'
require 'securerandom'
require 'time'

require_relative 'gate'
require_relative '../suggestion_report'
require_relative '../tree_digest'

module PromptRsi
  # 对照评测报告的组装、渲染与归档。只含计数、任务 id 与出处，不含模型正文
  # 与提示词正文（变体文本在变体文件里，报告只记它的 sha256）。
  module Report
    SCHEMA = 'willdeep.prompt-rsi-report.v1'
    # 输入建议套件双方原始样本的归档子目录（相对归档根目录）。
    EVIDENCE_DIR = 'suggestion-runs'
    # run_id 随机后缀的字节数（十六进制后是它的两倍长）。
    RUN_ID_RANDOM_BYTES = 3

    # 归档目标已存在：报告与样本都是证据，只新增、不覆盖。
    class ArchiveExists < StandardError; end

    # §11.2 第 4 道：门禁通过后仍要人来做的事。
    HUMAN_GATE = [
      '读变体 diff（`willdeep prompt check <variant>`），确认改的是一条规则、说得通',
      '抽看 validation 上翻转的任务（失败→通过、通过→失败）的会话',
      '把变体文本改进代码常量，走普通 PR；PR 描述附这份报告',
      '合入后观察 `willdeep feedback report` 里该角色新 bundle 的指标，恶化就 revert'
    ].freeze

    module_function

    # 任务集的内容哈希：路径与内容都算，任务集一改结果就不可比。
    def dataset_sha256(root)
      TreeDigest.sha256(root)
    end

    # 逐任务的对照（只给 validation 与 regression；holdout 不出逐题结果）。
    def flips(baseline_rows, candidate_rows)
      candidate = candidate_rows.to_h { |row| [row['task'], row] }
      baseline_rows.filter_map do |row|
        other = candidate[row['task']]
        next unless other
        next unless %w[validation regression].include?(row['split'])

        { 'task' => row['task'], 'split' => row['split'], 'baseline' => row['status'], 'candidate' => other['status'] }
      end
    end

    # 一次评测（或一次复评）的唯一标识：时间、变体、模型，外加随机后缀——
    # 同一秒、同一变体、同一模型跑两遍也不会撞名。`tag` 标出复评之类的派生结论。
    def run_id(provenance, generated_at, tag: nil)
      parts = [generated_at.strftime('%Y%m%dT%H%M%SZ'), provenance['variant_id'], provenance['model'], tag,
               SecureRandom.hex(RUN_ID_RANDOM_BYTES)]
      parts.compact.join('-').gsub(/[^A-Za-z0-9._-]/, '_')
    end

    def build(suite:, provenance:, result:, baseline_rows: [], candidate_rows: [], generated_at: Time.now.utc,
              tag: nil)
      report = {
        'schema' => SCHEMA,
        'run_id' => run_id(provenance, generated_at, tag: tag),
        'suite' => suite,
        'generated_at' => generated_at.strftime('%Y-%m-%dT%H:%M:%SZ'),
        'verdict' => result['verdict'],
        'provenance' => provenance,
        'checks' => result['checks'],
        'problems' => result['problems'],
        'stats' => result['stats']
      }
      report['judging'] = result['judging'] if result['judging']
      report['tasks'] = flips(baseline_rows, candidate_rows) if suite == 'model-eval'
      report
    end

    # 输入建议套件双方的原始样本（含模型输出，与 `bench/input-suggestion/runs/`
    # 同一口径），人工判定就填在这两份文件里。返回相对归档目录的路径。
    def save_suggestion_evidence(dir, report, baseline:, candidate:)
      relative = File.join(EVIDENCE_DIR, report.fetch('run_id'))
      raise ArchiveExists, "样本目录已存在，不覆盖：#{File.join(dir, relative)}" if File.exist?(File.join(dir, relative))

      FileUtils.mkdir_p(File.join(dir, relative))
      { 'baseline' => baseline, 'candidate' => candidate }.each do |side, raw|
        # suggest 样本都带上 `judged` 键（初值 null），人工判定就填在这里。
        cases = raw['cases'].to_a.map do |row|
          row['expect'] == 'suggest' && !row.key?('judged') ? row.merge('judged' => nil) : row
        end
        evidence = { 'side' => side, 'model' => raw['model'], 'prompt_variant' => raw['prompt_variant'],
                     'cases' => cases }
        File.write(File.join(dir, relative, "#{side}.json"), "#{JSON.pretty_generate(evidence)}\n")
      end
      relative
    end

    def load_suggestion_evidence(dir, relative)
      %w[baseline candidate].to_h do |side|
        [side, JSON.parse(File.read(File.join(dir, relative, "#{side}.json"), encoding: 'UTF-8'))]
      end
    end

    # 样本内容的指纹，不含人工填写的 `judged`：复评时据此确认只改了判定，
    # 没动模型输出、样本 id 或生效的变体。
    def evidence_digest(raw)
      cases = raw['cases'].to_a.map { |row| row.reject { |key, _| key == 'judged' } }
      Digest::SHA256.hexdigest(JSON.generate({ 'model' => raw['model'], 'prompt_variant' => raw['prompt_variant'],
                                               'cases' => cases }))
    end

    def markdown(report)
      provenance = report['provenance']
      lines = ["# 提示词对照评测：#{provenance['variant_id']}", '']
      lines << "- 结论：**#{report['verdict']}**"
      lines << "- 运行：`#{report['run_id']}`#{report['rescored_from'] ? "（复评自 `#{report['rescored_from']}`）" : ''}"
      lines << "- 套件：#{report['suite']} · 模型：#{provenance['model']} · 生成于 #{report['generated_at']}"
      lines << "- 变体：#{provenance['role']} / #{provenance['section']}（`#{provenance['parent_bundle']}` → `#{provenance['candidate_bundle']}`）"
      lines << "- 出处：commit #{provenance['commit'] || '—'}#{provenance['dirty'] ? '（dirty）' : ''} · " \
               "binary #{provenance['binary_version'] || '—'} · dataset #{provenance['dataset_sha256']&.slice(0, 12) || '—'} · " \
               "variant #{provenance['variant_sha256']&.slice(0, 12) || '—'}"
      unless report['problems'].to_a.empty?
        lines << '' << '## 不能作数的原因' << ''
        report['problems'].each { |problem| lines << "- #{problem}" }
      end
      lines << '' << '## 门禁' << ''
      report['checks'].to_a.each { |item| lines << "- #{item['ok'] ? '✅' : '❌'} `#{item['name']}` #{item['detail']}" }
      flipped = report['tasks'].to_a.reject { |row| row['baseline'] == row['candidate'] }
      unless flipped.empty?
        lines << '' << '## 结果变化的任务（validation / regression）' << ''
        flipped.each { |row| lines << "- #{row['task']}（#{row['split']}）：#{row['baseline']} → #{row['candidate']}" }
      end
      lines.concat(judging_lines(report)) if report['verdict'] == 'needs_human_judging'
      if report['verdict'] == 'candidate_passes'
        lines << '' << '## 人工门（通过门禁不等于上线）' << ''
        HUMAN_GATE.each { |item| lines << "- [ ] #{item}" }
      end
      "#{lines.join("\n")}\n"
    end

    # 待人工判定：哪两份文件、每边还差哪些样本、判完怎么重算。
    def judging_lines(report)
      lines = ['', '## 待人工判定', '']
      evidence = report['evidence']
      lines << (evidence ? "- 双方样本：`#{evidence}/baseline.json`、`#{evidence}/candidate.json`（相对归档目录）" : '- 没有保存双方样本，无法复评')
      report['judging'].to_h.each do |side, status|
        unjudged = status['unjudged_ids']
        detail = unjudged.nil? ? '（摘要没有待判清单）' : "待判 #{unjudged.size} 条#{unjudged.empty? ? '' : "：#{unjudged.join(', ')}"}"
        lines << "- #{side}：该判 #{status['judgeable'] || '—'} 条，已判 #{status['judged']} 条，#{detail}"
      end
      lines << "- 给每条给出了建议的 suggest 样本填 `judged`（#{SuggestionReport::JUDGEMENTS.join(' / ')}），两边都要判完；" \
               '判完运行 `ruby scripts/prompt_rsi_eval.rb --rescore <本报告的 .json>`，不会再请求模型'
      lines
    end

    # 写 `<dir>/reports/<date>/<run_id>.{json,md}`，并向 history.jsonl 追加一行，
    # 记下 run_id、报告路径和报告内容的 sha256。已归档的报告是证据，不覆盖：
    # 同名文件已存在就抛 `ArchiveExists`。
    def archive(report, dir, history: true)
      provenance = report['provenance']
      day = File.join(dir, 'reports', report['generated_at'][0, 10])
      FileUtils.mkdir_p(day)
      json = File.join(day, "#{report.fetch('run_id')}.json")
      content = "#{JSON.pretty_generate(report)}\n"
      write_new(json, content)
      write_new(json.sub(/\.json\z/, '.md'), markdown(report))
      if history
        row = { 'run_id' => report['run_id'], 'ran_at' => report['generated_at'], 'suite' => report['suite'],
                'verdict' => report['verdict'], 'variant_id' => provenance['variant_id'], 'role' => provenance['role'],
                'section' => provenance['section'], 'parent_bundle' => provenance['parent_bundle'],
                'candidate_bundle' => provenance['candidate_bundle'], 'model' => provenance['model'],
                'commit' => provenance['commit'], 'dirty' => provenance['dirty'],
                'failed_checks' => report['checks'].to_a.reject { |item| item['ok'] }.map { |item| item['name'] },
                'report' => json.delete_prefix("#{dir}/"), 'report_sha256' => Digest::SHA256.hexdigest(content) }
        row['rescored_from'] = report['rescored_from'] if report['rescored_from']
        File.open(File.join(dir, 'history.jsonl'), 'a') { |file| file.puts(JSON.generate(row)) }
      end
      json
    end

    # 独占创建：文件已存在就失败，不会悄悄覆盖旧证据。
    def write_new(path, content)
      File.open(path, File::WRONLY | File::CREAT | File::EXCL, encoding: 'UTF-8') { |file| file.write(content) }
    rescue Errno::EEXIST
      raise ArchiveExists, "归档文件已存在，不覆盖：#{path}"
    end
  end
end
