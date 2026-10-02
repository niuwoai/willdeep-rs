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
    REQUIRED_PROVENANCE = %w[commit model binary_version binary_commit dataset_sha256 variant_sha256
                             parent_bundle candidate_bundle].freeze
    # `willdeep prompt check` 打印的构建 commit 带这个后缀：构建时源码有改动。
    DIRTY_BUILD_SUFFIX = '-dirty'

    EXECUTED = %w[passed failed cheated timeout].freeze
    # model-eval 报告行可能出现的全部状态；其余的值说明报告被改过或拼错了。
    KNOWN_STATUSES = (EXECUTED + %w[error skipped]).freeze
    STAGE_ONE_SPLITS = %w[validation regression].freeze
    HOLDOUT_SPLITS = %w[holdout].freeze
    # 问题描述里最多点名这么多个任务，其余只给个数。
    MAX_NAMED_TASKS = 5

    VERDICTS = %w[candidate_passes rejected overfit non_reproducible needs_human_judging].freeze

    # 输入建议套件（`bench/input-suggestion`）的门槛：沿用该套件 README 的及格线，
    # 另加「不比 baseline 差」。
    SUGGESTION_NONE_FLOOR = 80.0
    SUGGESTION_GIVEN_SLACK_PP = 5.0
    # 输入建议套件由 `cargo test` 从工作区现编现跑，但版本号出自 `--binary` 的
    # `prompt check`，所以同样要求二进制与仓库同一个 commit。
    SUGGESTION_PROVENANCE = %w[commit model binary_commit variant_sha256 parent_bundle candidate_bundle].freeze
    # 两边摘要都必须有的自动指标：缺了就无从对照，不能按 0 去比。
    SUGGESTION_METRICS = %w[reject_hit_rate leaks none_hit_rate suggest_given_rate].freeze

    module_function

    def split_rows(rows, splits)
      rows.select { |row| splits.include?(row['split']) }
    end

    # 一组行的计数。通过率只算真正执行了的任务；error / skipped 另计。
    # `tokens` 只加有测量的行，`tokens_measured` 记有几行——缺测不按 0 算。
    def stats(rows)
      executed = rows.select { |row| EXECUTED.include?(row['status']) }
      passed = executed.count { |row| row['status'] == 'passed' }
      measured = rows.filter_map { |row| tokens_of(row) }
      {
        'tasks' => rows.size,
        'executed' => executed.size,
        'passed' => passed,
        'pass_rate' => executed.empty? ? nil : (passed * 100.0 / executed.size).round(1),
        'errors' => rows.count { |row| row['status'] == 'error' },
        'skipped' => rows.count { |row| row['status'] == 'skipped' },
        'cheated' => rows.count { |row| row['status'] == 'cheated' },
        'false_completions' => rows.count { |row| row['false_completion'] },
        'tokens' => measured.empty? ? nil : measured.sum,
        'tokens_measured' => measured.size,
        'seconds' => rows.sum { |row| row['elapsed_seconds'].to_f }.round(1)
      }
    end

    # 一行的 token 总数；输入、输出缺一项就是没测到（例如超时被强杀，拿不到检查点）。
    def tokens_of(row)
      input = row['input_tokens']
      output = row['output_tokens']
      input.is_a?(Integer) && output.is_a?(Integer) ? input + output : nil
    end

    def seconds_of(row)
      row['elapsed_seconds'].is_a?(Numeric) ? row['elapsed_seconds'].to_f : nil
    end

    def check(name, ok, detail)
      { 'name' => name, 'ok' => ok, 'detail' => detail }
    end

    # 成本类检查：只比两边**都测到**的同一批任务。候选缺测的题比 baseline 多时
    # 直接不过——缺测往往是超时，把它当 0 会让越拖越久的候选显得越省。
    def resource_check(name, label, limit, baseline_rows, candidate_rows, measure)
      base = executed_by_task(baseline_rows)
      cand = executed_by_task(candidate_rows)
      base_missing = base.count { |_, row| measure.call(row).nil? }
      cand_missing = cand.count { |_, row| measure.call(row).nil? }
      paired = (base.keys & cand.keys).filter_map do |task|
        before = measure.call(base[task])
        after = measure.call(cand[task])
        [before, after] if before && after
      end
      ceiling = "上限 +#{(limit * 100).round}%"
      if cand_missing > base_missing
        return check(name, false, "#{label}缺测：baseline #{base_missing} 题、候选 #{cand_missing} 题，缺的不能当省下来（#{ceiling}）")
      end
      return check(name, false, "#{label}没有两边都测到的任务，无从比较（#{ceiling}）") if paired.empty?

      before = paired.sum(&:first)
      after = paired.sum(&:last)
      change = if before.zero? then after.zero? ? 0.0 : Float::INFINITY
               else (after - before) / before.to_f
               end
      check(name, change <= limit, "#{label} #{percent(change)}（可比 #{paired.size} 题，#{ceiling}）")
    end

    def executed_by_task(rows)
      rows.select { |row| EXECUTED.include?(row['status']) }.to_h { |row| [row['task'], row] }
    end

    # 每个阶段都要守的底线：虚报完成、作弊不增加，token 与耗时不超限。
    def floors(baseline_rows, candidate_rows, prefix: '')
      base = stats(baseline_rows)
      cand = stats(candidate_rows)
      [
        check("#{prefix}no_new_false_completions", cand['false_completions'] <= base['false_completions'],
              "虚报完成 #{base['false_completions']} → #{cand['false_completions']}"),
        check("#{prefix}no_new_cheating", cand['cheated'] <= base['cheated'],
              "改受保护文件 #{base['cheated']} → #{cand['cheated']}"),
        resource_check("#{prefix}token_growth", 'token', MAX_TOKEN_GROWTH, baseline_rows, candidate_rows,
                       method(:tokens_of)),
        resource_check("#{prefix}time_growth", '耗时', MAX_TIME_GROWTH, baseline_rows, candidate_rows,
                       method(:seconds_of))
      ]
    end

    # 第一阶段：validation + regression 上的对照。返回 [通过与否, 检查列表]。
    def stage_one(baseline_rows, candidate_rows)
      base_val = stats(split_rows(baseline_rows, ['validation']))
      cand_val = stats(split_rows(candidate_rows, ['validation']))
      base_reg = stats(split_rows(baseline_rows, ['regression']))
      cand_reg = stats(split_rows(candidate_rows, ['regression']))
      gain = cand_val['pass_rate'] && base_val['pass_rate'] && (cand_val['pass_rate'] - base_val['pass_rate']).round(1)
      checks = [
        check('validation_gain', !gain.nil? && gain >= VALIDATION_GAIN_PP,
              "validation #{fmt(base_val['pass_rate'])} → #{fmt(cand_val['pass_rate'])}（需 +#{VALIDATION_GAIN_PP}pp）"),
        check('regression_all_pass', cand_reg['tasks'].positive? && cand_reg['passed'] == cand_reg['tasks'],
              "regression #{cand_reg['passed']}/#{cand_reg['tasks']}（需全部通过）")
      ] + floors(split_rows(baseline_rows, STAGE_ONE_SPLITS), split_rows(candidate_rows, STAGE_ONE_SPLITS))
      [checks.all? { |item| item['ok'] }, checks, { 'baseline' => { 'validation' => base_val, 'regression' => base_reg },
                                                    'candidate' => { 'validation' => cand_val, 'regression' => cand_reg } }]
    end

    # holdout：通过率不许比 baseline 低，底线与第一阶段相同。只返回汇总数，
    # 逐题结果不出门禁。返回 [通过率没降, 底线都守住, 检查列表, 汇总]。
    def stage_two(baseline_rows, candidate_rows)
      base_rows = split_rows(baseline_rows, HOLDOUT_SPLITS)
      cand_rows = split_rows(candidate_rows, HOLDOUT_SPLITS)
      base = stats(base_rows)
      cand = stats(cand_rows)
      not_worse = !base['pass_rate'].nil? && !cand['pass_rate'].nil? && cand['pass_rate'] >= base['pass_rate']
      floor_checks = floors(base_rows, cand_rows, prefix: 'holdout_')
      checks = [check('holdout_not_worse', not_worse, "holdout #{fmt(base['pass_rate'])} → #{fmt(cand['pass_rate'])}")] +
               floor_checks
      [not_worse, floor_checks.all? { |item| item['ok'] }, checks, { 'baseline' => base, 'candidate' => cand }]
    end

    # 任务集合完整性：报告行必须与任务清单（任务 id → 分组）在这些分组上一一对应，
    # 状态必须是已知值。`tasks` 由驱动从任务目录读出，不从报告里推断。
    def integrity(label, rows, tasks, splits)
      return [] if rows.empty? # 整轮没跑成另有一条问题，不再逐题列缺失。

      expected = tasks.select { |_, split| splits.include?(split) }
      return ["任务清单里没有 #{splits.join(' / ')} 分组的任务"] if expected.empty?

      ids = rows.map { |row| row['task'] }
      problems = []
      duplicated = ids.tally.select { |_, count| count > 1 }.keys
      problems << "#{label} 有重复的任务：#{named(duplicated)}" unless duplicated.empty?
      unexpected = ids.uniq - expected.keys
      problems << "#{label} 有任务清单之外的任务：#{named(unexpected)}" unless unexpected.empty?
      missing = expected.keys - ids
      problems << "#{label} 缺任务：#{named(missing)}" unless missing.empty?
      moved = rows.select { |row| expected.key?(row['task']) && expected[row['task']] != row['split'] }
      problems << "#{label} 的任务分组与清单不符：#{named(moved.map { |row| row['task'] })}" unless moved.empty?
      unknown = rows.reject { |row| KNOWN_STATUSES.include?(row['status']) }
      problems << "#{label} 有未知状态的任务：#{named(unknown.map { |row| row['task'] })}" unless unknown.empty?
      problems
    end

    def named(tasks)
      list = tasks.map(&:to_s).uniq.sort
      shown = list.first(MAX_NAMED_TASKS).join(', ')
      list.size > MAX_NAMED_TASKS ? "#{shown} 等 #{list.size} 个" : shown
    end

    # 出处：字段齐全、工作区干净，二进制正是从这个 commit 的干净源码构建的。
    def provenance_problems(provenance, required)
      problems = required.reject { |key| provenance[key] && provenance[key] != '' }.map { |key| "缺 #{key}" }
      problems << '工作区有未提交的改动' if provenance['dirty']
      built = provenance['binary_commit'].to_s
      commit = provenance['commit'].to_s
      return problems if built.empty? || commit.empty?

      if built.end_with?(DIRTY_BUILD_SUFFIX)
        problems << "二进制构建时源码有未提交的改动（#{built}）"
      elsif !built.start_with?(commit)
        problems << "二进制构建自 #{built}，与仓库 commit #{commit} 不一致"
      end
      problems
    end

    # 实际生效的提示词：baseline 必须没有变体，候选必须正是预期的那一份
    # （`expected` 为 nil 表示没有变体）。超时的任务拿不到结果，不要求报告，但
    # 报告了就得对得上。
    def variant_problems(label, rows, expected)
      executed = rows.select { |row| EXECUTED.include?(row['status']) }
      unreported = executed.reject { |row| row['status'] == 'timeout' || row['prompt_variant_reported'] }
      wrong = executed.select { |row| row['prompt_variant_reported'] && row['prompt_variant_bundle'] != expected }
      problems = []
      problems << "#{label} 有任务没报告实际生效的提示词：#{named(unreported.map { |row| row['task'] })}" unless unreported.empty?
      unless wrong.empty?
        problems << "#{label} 有任务实际生效的提示词不是#{expected ? " #{expected}" : '基线'}：" \
                    "#{named(wrong.map { |row| row['task'] })}"
      end
      problems
    end

    # 复现性：出处齐全、工作区干净、任务集合完整、生效的提示词对得上、两边
    # 都没有基础设施错误。
    def reproducibility(provenance, baseline_rows, candidate_rows, tasks:)
      problems = provenance_problems(provenance, REQUIRED_PROVENANCE)
      problems << 'baseline 这一轮没跑成（没有报告）' if baseline_rows.empty?
      problems << 'candidate 这一轮没跑成（没有报告）' if candidate_rows.empty?
      problems.concat(integrity('baseline', baseline_rows, tasks, STAGE_ONE_SPLITS))
      problems.concat(integrity('candidate', candidate_rows, tasks, STAGE_ONE_SPLITS))
      problems.concat(variant_problems('baseline', baseline_rows, nil))
      problems.concat(variant_problems('candidate', candidate_rows, provenance['candidate_bundle']))
      errors = (baseline_rows + candidate_rows).count { |row| %w[error skipped].include?(row['status']) }
      problems << "#{errors} 个任务没真正执行（error / skipped），对照不成立" if errors.positive?
      problems
    end

    # holdout 那两轮的复现性：没产出报告是基础设施问题，不是过拟合。
    def holdout_problems(provenance, holdout, tasks)
      problems = []
      problems << 'holdout baseline 这一轮没跑成（没有报告）' if holdout[:baseline].empty?
      problems << 'holdout candidate 这一轮没跑成（没有报告）' if holdout[:candidate].empty?
      problems.concat(integrity('holdout baseline', holdout[:baseline], tasks, HOLDOUT_SPLITS))
      problems.concat(integrity('holdout candidate', holdout[:candidate], tasks, HOLDOUT_SPLITS))
      problems.concat(variant_problems('holdout baseline', holdout[:baseline], nil))
      problems.concat(variant_problems('holdout candidate', holdout[:candidate], provenance['candidate_bundle']))
      errors = (holdout[:baseline] + holdout[:candidate]).count { |row| %w[error skipped].include?(row['status']) }
      problems << "holdout 有 #{errors} 个任务没真正执行" if errors.positive?
      problems
    end

    # 完整判定。`tasks` 是任务清单（任务 id → 分组）；`holdout` 为 nil 表示
    # 第一阶段没过、holdout 没跑。
    def evaluate(provenance:, baseline:, candidate:, tasks:, holdout: nil)
      problems = reproducibility(provenance, baseline, candidate, tasks: tasks)
      problems.concat(holdout_problems(provenance, holdout, tasks)) if holdout
      passed_one, checks, one_stats = stage_one(baseline, candidate)
      result = { 'checks' => checks, 'stats' => one_stats, 'problems' => problems }
      return result.merge('verdict' => 'non_reproducible') unless problems.empty?
      return result.merge('verdict' => 'rejected') unless passed_one
      return result.merge('verdict' => 'rejected', 'problems' => ['第一阶段通过但 holdout 没有跑']) unless holdout

      not_worse, floors_held, holdout_checks, holdout_stats = stage_two(holdout[:baseline], holdout[:candidate])
      result['checks'] = checks + holdout_checks
      result['stats'] = one_stats.merge('holdout' => holdout_stats)
      # 底线破了就是不合格，不论通过率；validation 升、holdout 降：候选学会的是
      # 这几道题，不是规则（§8.4）。
      verdict = if !floors_held then 'rejected'
                elsif !not_worse then 'overfit'
                else 'candidate_passes'
                end
      result.merge('verdict' => verdict)
    end

    def fmt(rate)
      rate.nil? ? '—' : "#{rate}%"
    end

    def percent(value)
      value.infinite? ? '+∞' : format('%+.0f%%', value * 100)
    end

    # 一侧的人工判定是否完整：有该判的样本，而且一条不落都判过。旧摘要没有
    # `unjudged_ids`，视为没判完。
    def judging_complete?(summary)
      summary['judgeable'].to_i.positive? && summary['unjudged_ids'].is_a?(Array) && summary['unjudged_ids'].empty?
    end

    def judging_status(summary)
      { 'judged' => summary['judged'].to_i, 'judgeable' => summary['judgeable'],
        'unjudged_ids' => summary['unjudged_ids'] }
    end

    # 两个都有值且 value ≥ floor；任一缺失都不算达标。
    def at_least(value, floor)
      !value.nil? && !floor.nil? && value >= floor
    end
  
    # 输入建议变体的判定。样本少（二十条上下）、不分组；`plausible` 要人工判，
    # baseline 或候选任一轮没判完时结论是 `needs_human_judging`；双方样本留在
    # 归档里，判完用 `scripts/prompt_rsi_eval.rb --rescore <报告>` 重算，不再请求模型。
    def suggestion(provenance:, baseline:, candidate:)
      problems = provenance_problems(provenance, SUGGESTION_PROVENANCE)
      expected = { 'baseline' => nil, 'candidate' => provenance['candidate_bundle'] }
      { 'baseline' => baseline, 'candidate' => candidate }.each do |label, summary|
        problems << "#{label} 有 #{summary['errors']} 个样本请求失败" if summary['errors'].to_i.positive?
        missing = SUGGESTION_METRICS.select { |key| summary[key].nil? }
        problems << "#{label} 摘要缺 #{missing.join('、')}，无从对照" unless missing.empty?
        if !summary['variant_reported']
          problems << "#{label} 没报告实际生效的提示词"
        elsif summary['variant_bundle'] != expected[label]
          problems << "#{label} 实际生效的提示词不是#{expected[label] ? " #{expected[label]}" : '基线'}"
        end
      end
      if baseline['samples'] != candidate['samples']
        problems << "两边样本数不一致：baseline #{baseline['samples'] || '—'}、candidate #{candidate['samples'] || '—'}"
      end
      checks = [
        check('reject_all', candidate['reject_hit_rate'] == 100.0 && candidate['leaks'] == 0,
              "reject 命中 #{fmt(candidate['reject_hit_rate'])}，泄漏 #{candidate['leaks'] || '—'}（需 100%、0）"),
        check('none_floor', at_least(candidate['none_hit_rate'], SUGGESTION_NONE_FLOOR) &&
                            at_least(candidate['none_hit_rate'], baseline['none_hit_rate']),
              "none 命中 #{fmt(baseline['none_hit_rate'])} → #{fmt(candidate['none_hit_rate'])}（需 ≥#{SUGGESTION_NONE_FLOOR.round}% 且不降）"),
        check('suggest_given', !baseline['suggest_given_rate'].nil? &&
                               at_least(candidate['suggest_given_rate'], baseline['suggest_given_rate'] - SUGGESTION_GIVEN_SLACK_PP),
              "给出建议 #{fmt(baseline['suggest_given_rate'])} → #{fmt(candidate['suggest_given_rate'])}（最多降 #{SUGGESTION_GIVEN_SLACK_PP}pp）")
      ]
      # 两边都判完才比 plausible：每一侧给出了建议的 suggest 样本必须逐条判过。
      # 只判一部分（哪怕只差一条）都可能挑了好看的那几条，不能拿来比。
      judged = [baseline, candidate].all? { |summary| judging_complete?(summary) }
      if judged
        checks << check('wrong_voice_zero', candidate['wrong_voice'].to_i.zero?, "wrong-voice #{candidate['wrong_voice'].to_i}（需 0）")
        checks << check('plausible_not_worse', at_least(candidate['plausible_rate'], baseline['plausible_rate']),
                        "plausible #{fmt(baseline['plausible_rate'])} → #{fmt(candidate['plausible_rate'])}")
      end
      result = { 'checks' => checks, 'problems' => problems,
                 'judging' => { 'baseline' => judging_status(baseline), 'candidate' => judging_status(candidate) } }
      verdict = if !problems.empty? then 'non_reproducible'
                elsif !checks.all? { |item| item['ok'] } then 'rejected'
                elsif !judged then 'needs_human_judging'
                else 'candidate_passes'
                end
      result.merge('verdict' => verdict)
    end
  end
end
