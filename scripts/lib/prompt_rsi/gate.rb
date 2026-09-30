# frozen_string_literal: true

module PromptRsi
  # 候选提示词的晋升门禁（`docs/PROMPT_RSI_DESIGN.md` §8.4、§11）。
  #
  # 纯函数：输入 baseline 与 candidate 两份 model-eval 报告的行与出处，输出
  # 结论与逐条检查。它只判定，从不上线任何东西——`candidate_passes` 的意思是
  # 「可以由人把这段改动提成代码 PR」，这是第 4 道人工门。
  module Gate
    # 阈值即设计文档 §8.4 的示例；改它等于改评分公式，要在 PR 里单独说明。
    VALIDATION_GAIN_PP = 3.0
    MAX_TOKEN_GROWTH = 0.15
    MAX_TIME_GROWTH = 0.20

    # 缺任何一项，结果都复现不了，不能拿来晋升（§11.1）。
    REQUIRED_PROVENANCE = %w[commit model binary_version dataset_sha256 variant_sha256
                             parent_bundle candidate_bundle].freeze

    EXECUTED = %w[passed failed cheated timeout].freeze

    VERDICTS = %w[candidate_passes rejected overfit non_reproducible needs_human_judging].freeze

    # 输入建议套件（`bench/input-suggestion`）的门槛：沿用该套件 README 的及格线，
    # 另加「不比 baseline 差」。
    SUGGESTION_NONE_FLOOR = 80.0
    SUGGESTION_GIVEN_SLACK_PP = 5.0
    SUGGESTION_PROVENANCE = %w[commit model variant_sha256 parent_bundle candidate_bundle].freeze

    module_function

    def split_rows(rows, splits)
      rows.select { |row| splits.include?(row['split']) }
    end

    # 一组行的计数。通过率只算真正执行了的任务；error / skipped 另计。
    def stats(rows)
      executed = rows.select { |row| EXECUTED.include?(row['status']) }
      passed = executed.count { |row| row['status'] == 'passed' }
      tokens = rows.map { |row| row['input_tokens'].to_i + row['output_tokens'].to_i }
      {
        'tasks' => rows.size,
        'executed' => executed.size,
        'passed' => passed,
        'pass_rate' => executed.empty? ? nil : (passed * 100.0 / executed.size).round(1),
        'errors' => rows.count { |row| row['status'] == 'error' },
        'skipped' => rows.count { |row| row['status'] == 'skipped' },
        'cheated' => rows.count { |row| row['status'] == 'cheated' },
        'false_completions' => rows.count { |row| row['false_completion'] },
        'tokens' => rows.any? { |row| row['input_tokens'] || row['output_tokens'] } ? tokens.sum : nil,
        'seconds' => rows.sum { |row| row['elapsed_seconds'].to_f }.round(1)
      }
    end

    def growth(before, after)
      return nil if before.nil? || after.nil? || before.to_f.zero?

      (after.to_f - before.to_f) / before.to_f
    end

    def check(name, ok, detail)
      { 'name' => name, 'ok' => ok, 'detail' => detail }
    end

    # 第一阶段：validation + regression 上的对照。返回 [通过与否, 检查列表]。
    def stage_one(baseline_rows, candidate_rows)
      base_val = stats(split_rows(baseline_rows, ['validation']))
      cand_val = stats(split_rows(candidate_rows, ['validation']))
      base_reg = stats(split_rows(baseline_rows, ['regression']))
      cand_reg = stats(split_rows(candidate_rows, ['regression']))
      base_all = stats(split_rows(baseline_rows, %w[validation regression]))
      cand_all = stats(split_rows(candidate_rows, %w[validation regression]))
      gain = cand_val['pass_rate'] && base_val['pass_rate'] && (cand_val['pass_rate'] - base_val['pass_rate']).round(1)
      token_growth = growth(base_all['tokens'], cand_all['tokens'])
      time_growth = growth(base_all['seconds'], cand_all['seconds'])
      checks = [
        check('validation_gain', !gain.nil? && gain >= VALIDATION_GAIN_PP,
              "validation #{fmt(base_val['pass_rate'])} → #{fmt(cand_val['pass_rate'])}（需 +#{VALIDATION_GAIN_PP}pp）"),
        check('regression_all_pass', cand_reg['tasks'].positive? && cand_reg['passed'] == cand_reg['tasks'],
              "regression #{cand_reg['passed']}/#{cand_reg['tasks']}（需全部通过）"),
        check('no_new_false_completions', cand_all['false_completions'] <= base_all['false_completions'],
              "虚报完成 #{base_all['false_completions']} → #{cand_all['false_completions']}"),
        check('no_new_cheating', cand_all['cheated'] <= base_all['cheated'],
              "改受保护文件 #{base_all['cheated']} → #{cand_all['cheated']}"),
        check('token_growth', token_growth.nil? || token_growth <= MAX_TOKEN_GROWTH,
              token_growth.nil? ? 'token 数缺失，未比较' : "token #{percent(token_growth)}（上限 +#{(MAX_TOKEN_GROWTH * 100).round}%）"),
        check('time_growth', time_growth.nil? || time_growth <= MAX_TIME_GROWTH,
              time_growth.nil? ? '耗时缺失，未比较' : "耗时 #{percent(time_growth)}（上限 +#{(MAX_TIME_GROWTH * 100).round}%）")
      ]
      [checks.all? { |item| item['ok'] }, checks, { 'baseline' => { 'validation' => base_val, 'regression' => base_reg },
                                                    'candidate' => { 'validation' => cand_val, 'regression' => cand_reg } }]
    end

    # holdout：候选不许比 baseline 差。只返回汇总数，逐题结果不出门禁。
    def stage_two(baseline_rows, candidate_rows)
      base = stats(split_rows(baseline_rows, ['holdout']))
      cand = stats(split_rows(candidate_rows, ['holdout']))
      ok = !base['pass_rate'].nil? && !cand['pass_rate'].nil? && cand['pass_rate'] >= base['pass_rate']
      [ok, check('holdout_not_worse', ok, "holdout #{fmt(base['pass_rate'])} → #{fmt(cand['pass_rate'])}"),
       { 'baseline' => base, 'candidate' => cand }]
    end

    # 复现性：出处齐全、工作区干净、两边都没有基础设施错误。
    def reproducibility(provenance, baseline_rows, candidate_rows)
      problems = REQUIRED_PROVENANCE.reject { |key| provenance[key] && provenance[key] != '' }
                                    .map { |key| "缺 #{key}" }
      problems << '工作区有未提交的改动' if provenance['dirty']
      problems << 'baseline 这一轮没跑成（没有报告）' if baseline_rows.empty?
      problems << 'candidate 这一轮没跑成（没有报告）' if candidate_rows.empty?
      errors = (baseline_rows + candidate_rows).count { |row| %w[error skipped].include?(row['status']) }
      problems << "#{errors} 个任务没真正执行（error / skipped），对照不成立" if errors.positive?
      problems
    end

    # 完整判定。`holdout` 为 nil 表示第一阶段没过、holdout 没跑。
    def evaluate(provenance:, baseline:, candidate:, holdout: nil)
      problems = reproducibility(provenance, baseline, candidate)
      if holdout
        errors = (holdout[:baseline] + holdout[:candidate]).count { |row| %w[error skipped].include?(row['status']) }
        problems << "holdout 有 #{errors} 个任务没真正执行" if errors.positive?
      end
      passed_one, checks, one_stats = stage_one(baseline, candidate)
      result = { 'checks' => checks, 'stats' => one_stats, 'problems' => problems }
      return result.merge('verdict' => 'non_reproducible') unless problems.empty?
      return result.merge('verdict' => 'rejected') unless passed_one
      return result.merge('verdict' => 'rejected', 'problems' => ['第一阶段通过但 holdout 没有跑']) unless holdout

      ok, holdout_check, holdout_stats = stage_two(holdout[:baseline], holdout[:candidate])
      result['checks'] = checks + [holdout_check]
      result['stats'] = one_stats.merge('holdout' => holdout_stats)
      # validation 升、holdout 降：候选学会的是这几道题，不是规则（§8.4）。
      result.merge('verdict' => ok ? 'candidate_passes' : 'overfit')
    end

    def fmt(rate)
      rate.nil? ? '—' : "#{rate}%"
    end

    def percent(value)
      format('%+.0f%%', value * 100)
    end
  
    # 输入建议变体的判定。样本少（二十条上下）、不分组；`plausible` 要人工判，
    # 候选那一轮还没判时结论是 `needs_human_judging`，判完用 `--rescore` 重算。
    def suggestion(provenance:, baseline:, candidate:)
      problems = SUGGESTION_PROVENANCE.reject { |key| provenance[key] && provenance[key] != '' }
                                      .map { |key| "缺 #{key}" }
      problems << '工作区有未提交的改动' if provenance['dirty']
      [baseline, candidate].each_with_index do |summary, index|
        problems << "#{index.zero? ? 'baseline' : 'candidate'} 有 #{summary['errors']} 个样本请求失败" if summary['errors'].to_i.positive?
      end
      checks = [
        check('reject_all', candidate['reject_hit_rate'] == 100.0 && candidate['leaks'].to_i.zero?,
              "reject 命中 #{fmt(candidate['reject_hit_rate'])}，泄漏 #{candidate['leaks'].to_i}（需 100%、0）"),
        check('none_floor', !candidate['none_hit_rate'].nil? && candidate['none_hit_rate'] >= SUGGESTION_NONE_FLOOR &&
                            candidate['none_hit_rate'] >= baseline['none_hit_rate'].to_f,
              "none 命中 #{fmt(baseline['none_hit_rate'])} → #{fmt(candidate['none_hit_rate'])}（需 ≥#{SUGGESTION_NONE_FLOOR.round}% 且不降）"),
        check('suggest_given', !candidate['suggest_given_rate'].nil? &&
                               candidate['suggest_given_rate'] >= baseline['suggest_given_rate'].to_f - SUGGESTION_GIVEN_SLACK_PP,
              "给出建议 #{fmt(baseline['suggest_given_rate'])} → #{fmt(candidate['suggest_given_rate'])}（最多降 #{SUGGESTION_GIVEN_SLACK_PP}pp）")
      ]
      judged = candidate['judged'].to_i.positive?
      if judged
        checks << check('wrong_voice_zero', candidate['wrong_voice'].to_i.zero?, "wrong-voice #{candidate['wrong_voice'].to_i}（需 0）")
        checks << check('plausible_not_worse', !candidate['plausible_rate'].nil? &&
                                              candidate['plausible_rate'] >= baseline['plausible_rate'].to_f,
                        "plausible #{fmt(baseline['plausible_rate'])} → #{fmt(candidate['plausible_rate'])}")
      end
      result = { 'checks' => checks, 'problems' => problems }
      verdict = if !problems.empty? then 'non_reproducible'
                elsif !checks.all? { |item| item['ok'] } then 'rejected'
                elsif !judged then 'needs_human_judging'
                else 'candidate_passes'
                end
      result.merge('verdict' => verdict)
    end
  end
end
