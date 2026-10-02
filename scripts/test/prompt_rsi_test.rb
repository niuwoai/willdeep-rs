#!/usr/bin/env ruby
# frozen_string_literal: true

# 提示词对照评测门禁的测试。
#
#   ruby scripts/test/prompt_rsi_test.rb
#
# 不联网、不调 willdeep：用现造的 baseline / candidate 报告行钉住每一条门禁
# （validation 提升、regression 全过、虚报完成与作弊不增加、成本与耗时上限、
# holdout 不降）以及 overfit / non_reproducible 的判定、报告不带正文。

require 'json'
require 'minitest/autorun'
require 'tmpdir'

require_relative '../lib/prompt_rsi/gate'
require_relative '../lib/prompt_rsi/report'
require_relative '../prompt_rsi_eval'

class PromptRsiGateTest < Minitest::Test
  PROVENANCE = {
    'variant_id' => 'tone-conclusion-first', 'role' => 'main', 'section' => 'tone',
    'commit' => 'abc1234', 'dirty' => false, 'model' => 'glm-5', 'binary_version' => '0.87.0',
    'binary_commit' => "abc1234#{'0' * 33}",
    'dataset_sha256' => 'd' * 64, 'variant_sha256' => 'v' * 64,
    'parent_bundle' => 'main@111111111111', 'candidate_bundle' => 'main@222222222222'
  }.freeze

  # baseline 一行：willdeep 报告了没有套变体。
  def row(task, split, status, overrides = {})
    { 'task' => task, 'split' => split, 'status' => status, 'false_completion' => false,
      'input_tokens' => 1000, 'output_tokens' => 200, 'elapsed_seconds' => 10.0,
      'prompt_variant_reported' => true, 'prompt_variant_bundle' => nil,
      'task_sha256' => "sha-#{task}" }.merge(overrides)
  end

  # 候选一行：willdeep 报告套上的正是 provenance 里的候选版本。
  def cand_row(task, split, status, overrides = {})
    row(task, split, status, { 'prompt_variant_bundle' => PROVENANCE['candidate_bundle'] }.merge(overrides))
  end

  # validation 10 题、regression 2 题；baseline 过 6 题。
  def baseline_rows
    (0...10).map { |index| row("v#{index}", 'validation', index < 6 ? 'passed' : 'failed') } +
      [row('r0', 'regression', 'passed'), row('r1', 'regression', 'passed')]
  end

  def candidate_rows(passed: 7, regression: %w[passed passed], overrides: {})
    (0...10).map { |index| cand_row("v#{index}", 'validation', index < passed ? 'passed' : 'failed', overrides) } +
      regression.each_with_index.map { |status, index| cand_row("r#{index}", 'regression', status, overrides) }
  end

  def holdout(base_passed, cand_passed)
    { baseline: (0...4).map { |index| row("h#{index}", 'holdout', index < base_passed ? 'passed' : 'failed') },
      candidate: (0...4).map { |index| cand_row("h#{index}", 'holdout', index < cand_passed ? 'passed' : 'failed') } }
  end

  # 任务清单：validation 10、regression 2、holdout 4，外加一道不参与对照的 train。
  def tasks
    (baseline_rows + holdout(0, 0)[:baseline] + [row('t0', 'train', 'passed')]).to_h do |item|
      [item['task'], { 'split' => item['split'], 'sha256' => item['task_sha256'] }]
    end
  end

  def evaluate(candidate, holdout: holdout(2, 2), provenance: PROVENANCE)
    PromptRsi::Gate.evaluate(provenance: provenance, baseline: baseline_rows, candidate: candidate,
                             tasks: tasks, holdout: holdout)
  end

  def failed(result)
    result['checks'].reject { |item| item['ok'] }.map { |item| item['name'] }
  end

  def test_a_real_improvement_that_holds_on_holdout_passes
    result = evaluate(candidate_rows(passed: 7))
    assert_equal 'candidate_passes', result['verdict'], result.inspect
    assert_empty failed(result)
    assert_equal 60.0, result['stats']['baseline']['validation']['pass_rate']
    assert_equal 70.0, result['stats']['candidate']['validation']['pass_rate']
  end

  def test_each_stage_one_rule_rejects_on_its_own
    assert_equal ['validation_gain'], failed(evaluate(candidate_rows(passed: 6)))
    assert_equal ['regression_all_pass'], failed(evaluate(candidate_rows(regression: %w[passed failed])))
    lying = candidate_rows
    lying[9] = cand_row('v9', 'validation', 'failed', 'false_completion' => true)
    assert_equal ['no_new_false_completions'], failed(evaluate(lying))
    cheating = candidate_rows(passed: 8)
    cheating[9] = cand_row('v9', 'validation', 'cheated')
    assert_equal ['no_new_cheating'], failed(evaluate(cheating))
    assert_equal ['token_growth'], failed(evaluate(candidate_rows(overrides: { 'input_tokens' => 1200 })))
    assert_equal ['time_growth'], failed(evaluate(candidate_rows(overrides: { 'elapsed_seconds' => 12.5 })))
    %w[validation_gain regression_all_pass].each do |name|
      assert_includes failed(evaluate(candidate_rows(passed: 5, regression: %w[failed failed]))), name
    end
    assert_equal 'rejected', evaluate(candidate_rows(passed: 6))['verdict']
  end

  def test_better_on_validation_but_worse_on_holdout_is_overfit
    result = evaluate(candidate_rows(passed: 8), holdout: holdout(3, 2))
    assert_equal 'overfit', result['verdict']
    assert_equal ['holdout_not_worse'], failed(result)
    assert_equal 75.0, result['stats']['holdout']['baseline']['pass_rate']
  end

  def test_missing_provenance_dirty_trees_and_unexecuted_tasks_are_not_reproducible
    result = evaluate(candidate_rows, provenance: PROVENANCE.merge('commit' => nil))
    assert_equal 'non_reproducible', result['verdict']
    assert_includes result['problems'], '缺 commit'
    assert_equal 'non_reproducible', evaluate(candidate_rows, provenance: PROVENANCE.merge('dirty' => true))['verdict']
    errored = candidate_rows(passed: 8)
    errored[9] = row('v9', 'validation', 'error')
    assert_equal 'non_reproducible', evaluate(errored)['verdict']
    empty = PromptRsi::Gate.evaluate(provenance: PROVENANCE, baseline: baseline_rows, candidate: [], tasks: tasks)
    assert_equal 'non_reproducible', empty['verdict']
    assert_includes empty['problems'], 'candidate 这一轮没跑成（没有报告）'
    broken_holdout = holdout(2, 2)
    broken_holdout[:candidate][0] = row('h0', 'holdout', 'skipped')
    assert_equal 'non_reproducible', evaluate(candidate_rows, holdout: broken_holdout)['verdict']
  end

  def test_stage_one_without_holdout_is_never_a_pass
    result = PromptRsi::Gate.evaluate(provenance: PROVENANCE, baseline: baseline_rows, candidate: candidate_rows,
                                      tasks: tasks)
    assert_equal 'rejected', result['verdict']
  end

  # 报告行必须与任务清单一一对应：截断、换题、重复、换组、未知状态都不能晋升。
  def test_incomplete_or_mismatched_task_sets_are_not_reproducible
    cases = {
      'candidate 缺任务' => candidate_rows(passed: 10).reject { |item| %w[v8 v9 r1].include?(item['task']) },
      'candidate 有任务清单之外的任务' => candidate_rows.map { |item| item.merge('task' => "x-#{item['task']}") },
      'candidate 有重复的任务' => candidate_rows + [cand_row('v0', 'validation', 'passed')],
      'candidate 的任务分组与清单不符' => candidate_rows.map { |item| item['task'] == 'r1' ? item.merge('split' => 'validation') : item },
      'candidate 有未知状态的任务' => candidate_rows(passed: 6).map { |item| item['status'] == 'failed' ? item.merge('status' => 'interrupted') : item }
    }
    cases.each do |problem, candidate|
      result = evaluate(candidate)
      assert_equal 'non_reproducible', result['verdict'], problem
      assert(result['problems'].any? { |text| text.start_with?(problem) }, "#{problem}: #{result['problems'].inspect}")
    end
    truncated = holdout(2, 2)
    truncated[:candidate] = truncated[:candidate].first(1)
    result = evaluate(candidate_rows, holdout: truncated)
    assert_equal 'non_reproducible', result['verdict']
    assert_includes result['problems'], 'holdout candidate 缺任务：h1, h2, h3'
  end

  # 同一个任务 id、内容却变了：两轮之间有人改了题（切分支、编辑 fixture），
  # 或者报告行压根没记内容哈希。id 和分组都对得上也不能比。
  def test_a_task_whose_content_changed_between_runs_is_not_reproducible
    edited = candidate_rows.map { |item| item['task'] == 'v3' ? item.merge('task_sha256' => 'sha-edited') : item }
    result = evaluate(edited)
    assert_equal 'non_reproducible', result['verdict']
    assert_includes result['problems'], 'candidate 的任务内容与开跑前不一致：v3'
    legacy = candidate_rows.map { |item| item.reject { |key, _| key == 'task_sha256' } }
    assert(evaluate(legacy)['problems'].any? { |text| text.start_with?('candidate 的任务内容与开跑前不一致：') })
    holdout_edited = holdout(2, 2)
    holdout_edited[:baseline][0] = holdout_edited[:baseline][0].merge('task_sha256' => 'sha-edited')
    assert_includes evaluate(candidate_rows, holdout: holdout_edited)['problems'],
                    'holdout baseline 的任务内容与开跑前不一致：h0'
  end

  def test_holdout_that_never_ran_is_not_reproducible_rather_than_overfit
    result = evaluate(candidate_rows, holdout: { baseline: [], candidate: [] })
    assert_equal 'non_reproducible', result['verdict']
    assert_includes result['problems'], 'holdout baseline 这一轮没跑成（没有报告）'
    assert_includes result['problems'], 'holdout candidate 这一轮没跑成（没有报告）'
  end

  def test_holdout_keeps_the_same_floors_as_stage_one
    cheating = holdout(2, 2)
    cheating[:candidate][3] = cand_row('h3', 'holdout', 'cheated', 'false_completion' => true)
    result = evaluate(candidate_rows, holdout: cheating)
    assert_equal 'rejected', result['verdict']
    assert_equal %w[holdout_no_new_false_completions holdout_no_new_cheating], failed(result)
    costly = holdout(2, 2)
    costly[:candidate] = costly[:candidate].map { |item| item.merge('input_tokens' => 100_000, 'elapsed_seconds' => 10_000.0) }
    assert_equal %w[holdout_token_growth holdout_time_growth], failed(evaluate(candidate_rows, holdout: costly))
    # 底线破了又掉了通过率：先算不合格，不记成过拟合。
    worse = holdout(3, 2)
    worse[:candidate][3] = cand_row('h3', 'holdout', 'cheated')
    assert_equal 'rejected', evaluate(candidate_rows(passed: 8), holdout: worse)['verdict']
  end

  # 缺测的 token 不按 0 算：全缺、候选缺得比 baseline 多都不能过；两边缺同一题时
  # 只比都测到的那些题。
  def test_missing_token_measurements_never_count_as_a_pass
    unmeasured = { 'input_tokens' => nil, 'output_tokens' => nil }
    all_missing = candidate_rows(overrides: unmeasured)
    assert_equal ['token_growth'], failed(evaluate(all_missing))
    half_missing = candidate_rows.each_with_index.map do |item, index|
      index.even? ? item.merge(unmeasured) : item.merge('input_tokens' => 1300)
    end
    result = evaluate(half_missing)
    assert_equal ['token_growth'], failed(result)
    assert_match(/baseline 0 题、候选 6 题/, result['checks'].find { |item| item['name'] == 'token_growth' }['detail'])
    holdout_missing = holdout(2, 2)
    holdout_missing[:candidate] = holdout_missing[:candidate].map { |item| item.merge(unmeasured) }
    assert_equal ['holdout_token_growth'], failed(evaluate(candidate_rows, holdout: holdout_missing))
    # 超时被强杀：拿不到 JSON 结果，也就没有生效提示词的报告。
    timeout = row('v9', 'validation', 'timeout', unmeasured.merge('prompt_variant_reported' => false))
    base = baseline_rows.map { |item| item['task'] == 'v9' ? timeout : item }
    cand = candidate_rows.map { |item| item['task'] == 'v9' ? timeout : item }
    result = PromptRsi::Gate.evaluate(provenance: PROVENANCE, baseline: base, candidate: cand, tasks: tasks,
                                      holdout: holdout(2, 2))
    assert_equal 'candidate_passes', result['verdict'], result.inspect
    assert_match(/可比 11 题/, result['checks'].find { |item| item['name'] == 'token_growth' }['detail'])
  end

  # 一份摘要；缺省是 baseline（报告了没有套变体）。
  # 该判 10 条；`judged` 给几条就判了前几条，其余进 `unjudged_ids`。
  def suggestion_summary(overrides = {})
    judged = overrides.fetch('judged', 0)
    { 'errors' => 0, 'samples' => 19, 'reject_hit_rate' => 100.0, 'leaks' => 0, 'none_hit_rate' => 100.0,
      'suggest_given_rate' => 90.0, 'judged' => judged, 'judgeable' => 10,
      'unjudged_ids' => (judged...10).map { |index| "s#{index}" }, 'plausible_rate' => nil, 'wrong_voice' => 0,
      'variant_reported' => true, 'variant_bundle' => nil }.merge(overrides)
  end

  # 把一份摘要当候选：缺省报告套上了 provenance 里的候选版本。
  def as_candidate(summary)
    summary['variant_bundle'] ? summary : summary.merge('variant_bundle' => PROVENANCE['candidate_bundle'])
  end

  def test_suggestion_variants_need_human_judging_before_they_pass
    gate = lambda do |candidate, baseline = suggestion_summary('judged' => 10, 'plausible_rate' => 80.0)|
      PromptRsi::Gate.suggestion(provenance: PROVENANCE, baseline: baseline, candidate: as_candidate(candidate))
    end
    assert_equal 'needs_human_judging', gate.call(suggestion_summary)['verdict']
    assert_equal 'candidate_passes',
                 gate.call(suggestion_summary('judged' => 10, 'plausible_rate' => 90.0))['verdict']
    assert_equal 'rejected', gate.call(suggestion_summary('leaks' => 1, 'reject_hit_rate' => 50.0))['verdict']
    assert_equal 'rejected', gate.call(suggestion_summary('none_hit_rate' => 75.0))['verdict']
    assert_equal 'rejected', gate.call(suggestion_summary('suggest_given_rate' => 80.0))['verdict']
    assert_equal 'rejected',
                 gate.call(suggestion_summary('judged' => 10, 'plausible_rate' => 90.0, 'wrong_voice' => 1))['verdict']
    assert_equal 'non_reproducible', gate.call(suggestion_summary('errors' => 2))['verdict']
  end

  # baseline 缺的指标是「没测」，不是 0：不能拿来当下限，也不能让 plausible 过门。
  def test_missing_suggestion_metrics_are_never_compared_as_zero
    gate = lambda do |baseline, candidate = suggestion_summary('judged' => 10, 'plausible_rate' => 90.0)|
      PromptRsi::Gate.suggestion(provenance: PROVENANCE, baseline: baseline, candidate: as_candidate(candidate))
    end
    %w[none_hit_rate suggest_given_rate reject_hit_rate leaks].each do |key|
      result = gate.call(suggestion_summary('judged' => 10, 'plausible_rate' => 80.0, key => nil))
      assert_equal 'non_reproducible', result['verdict'], key
      assert_includes result['problems'], "baseline 摘要缺 #{key}，无从对照"
    end
    crashed = gate.call({ 'errors' => 1 })
    assert_equal 'non_reproducible', crashed['verdict']
    assert_includes crashed['problems'], 'baseline 摘要缺 reject_hit_rate、leaks、none_hit_rate、suggest_given_rate，无从对照'
    unjudged_baseline = gate.call(suggestion_summary)
    assert_equal 'needs_human_judging', unjudged_baseline['verdict']
    refute(unjudged_baseline['checks'].any? { |item| item['name'] == 'plausible_not_worse' })
  end

  # 人工判定必须两边都完整：只判一条好看的、baseline 没判、旧摘要没有待判清单，
  # 都只能是 needs_human_judging；判完才比 plausible。
  def test_human_judging_must_cover_every_given_suggestion_on_both_sides
    gate = lambda do |baseline, candidate|
      PromptRsi::Gate.suggestion(provenance: PROVENANCE, baseline: suggestion_summary(baseline),
                                 candidate: as_candidate(suggestion_summary(candidate)))
    end
    complete = { 'judged' => 10, 'plausible_rate' => 80.0 }
    cherry_picked = gate.call(complete, { 'judged' => 1, 'plausible_rate' => 100.0 })
    assert_equal 'needs_human_judging', cherry_picked['verdict']
    assert_equal %w[s1 s2 s3 s4 s5 s6 s7 s8 s9], cherry_picked['judging']['candidate']['unjudged_ids']
    assert_equal 'needs_human_judging', gate.call({ 'judged' => 0 }, complete.merge('plausible_rate' => 90.0))['verdict']
    legacy = complete.merge('plausible_rate' => 90.0, 'unjudged_ids' => nil)
    assert_equal 'needs_human_judging', gate.call(complete, legacy)['verdict']
    assert_equal 'candidate_passes', gate.call(complete, complete.merge('plausible_rate' => 90.0))['verdict']
    assert_equal 'rejected', gate.call(complete, complete.merge('plausible_rate' => 70.0))['verdict']
    mismatched = gate.call(complete, complete.merge('samples' => 18, 'plausible_rate' => 90.0))
    assert_equal 'non_reproducible', mismatched['verdict']
    assert_includes mismatched['problems'], '两边样本数不一致：baseline 19、candidate 18'
  end

  # baseline 继承了调用者 shell 里的候选、候选没套上、或者结果里根本没说套了
  # 什么：对照都不成立。超时拿不到结果的那一题不要求报告。
  def test_the_prompt_actually_in_effect_must_match_each_side
    leaked = baseline_rows.map { |item| item.merge('prompt_variant_bundle' => PROVENANCE['candidate_bundle']) }
    result = PromptRsi::Gate.evaluate(provenance: PROVENANCE, baseline: leaked, candidate: candidate_rows,
                                      tasks: tasks, holdout: holdout(2, 2))
    assert_equal 'non_reproducible', result['verdict']
    assert(result['problems'].any? { |text| text.start_with?('baseline 有任务实际生效的提示词不是基线') }, result['problems'].inspect)
    unapplied = candidate_rows.map { |item| item.merge('prompt_variant_bundle' => nil) }
    assert(evaluate(unapplied)['problems'].any? { |text| text.start_with?('candidate 有任务实际生效的提示词不是 main@222222222222') })
    silent = candidate_rows.map { |item| item.merge('prompt_variant_reported' => false) }
    assert(evaluate(silent)['problems'].any? { |text| text.start_with?('candidate 有任务没报告实际生效的提示词') })
    wrong_holdout = holdout(2, 2)
    wrong_holdout[:baseline] = wrong_holdout[:candidate]
    assert(evaluate(candidate_rows, holdout: wrong_holdout)['problems']
             .any? { |text| text.start_with?('holdout baseline 有任务实际生效的提示词不是基线') })

    suggestion = lambda do |baseline, candidate|
      PromptRsi::Gate.suggestion(provenance: PROVENANCE, baseline: suggestion_summary(baseline),
                                 candidate: suggestion_summary(candidate))
    end
    result = suggestion.call({ 'variant_bundle' => 'main@222222222222' }, { 'variant_bundle' => PROVENANCE['candidate_bundle'] })
    assert_includes result['problems'], 'baseline 实际生效的提示词不是基线'
    result = suggestion.call({}, { 'variant_reported' => false })
    assert_includes result['problems'], 'candidate 没报告实际生效的提示词'
  end

  # 二进制必须正是仓库这个 commit 的干净源码构建出来的，否则出处说不清。
  def test_the_binary_must_be_built_from_the_clean_repository_commit
    {
      PROVENANCE.merge('binary_commit' => nil) => '缺 binary_commit',
      PROVENANCE.merge('binary_commit' => "abc1234#{'0' * 33}-dirty") =>
        "二进制构建时源码有未提交的改动（abc1234#{'0' * 33}-dirty）",
      PROVENANCE.merge('binary_commit' => 'f' * 40) => "二进制构建自 #{'f' * 40}，与仓库 commit abc1234 不一致"
    }.each do |provenance, problem|
      result = evaluate(candidate_rows, provenance: provenance)
      assert_equal 'non_reproducible', result['verdict'], problem
      assert_includes result['problems'], problem
      suggestion = PromptRsi::Gate.suggestion(provenance: provenance, baseline: suggestion_summary,
                                              candidate: as_candidate(suggestion_summary))
      assert_includes suggestion['problems'], problem
    end
  end
end

class PromptRsiReportTest < Minitest::Test
  def test_reports_list_flips_hide_holdout_rows_and_append_history
    gate_test = PromptRsiGateTest.new('report')
    candidate = gate_test.candidate_rows(passed: 7)
    result = PromptRsi::Gate.evaluate(provenance: PromptRsiGateTest::PROVENANCE, baseline: gate_test.baseline_rows,
                                      candidate: candidate, tasks: gate_test.tasks, holdout: gate_test.holdout(2, 2))
    report = PromptRsi::Report.build(suite: 'model-eval', provenance: PromptRsiGateTest::PROVENANCE, result: result,
                                     baseline_rows: gate_test.baseline_rows, candidate_rows: candidate,
                                     generated_at: Time.utc(2026, 9, 30, 12))
    assert_equal 'candidate_passes', report['verdict']
    assert_equal 12, report['tasks'].size, 'validation and regression only'
    refute(report['tasks'].any? { |row| row['split'] == 'holdout' })
    markdown = PromptRsi::Report.markdown(report)
    assert_includes markdown, '结论：**candidate_passes**'
    assert_includes markdown, 'v6（validation）：failed → passed'
    assert_includes markdown, '## 人工门'
    assert_match(/\A20260930T120000Z-tone-conclusion-first-glm-5-\h{6}\z/, report['run_id'])
    assert_includes markdown, "运行：`#{report['run_id']}`"
    Dir.mktmpdir do |dir|
      path = PromptRsi::Report.archive(report, dir)
      assert_equal File.join(dir, 'reports', '2026-09-30', "#{report['run_id']}.json"), path
      history = File.readlines(File.join(dir, 'history.jsonl')).map { |line| JSON.parse(line) }
      assert_equal 'candidate_passes', history.last['verdict']
      assert_equal [], history.last['failed_checks']
      assert_equal report['run_id'], history.last['run_id']
      assert_equal path.delete_prefix("#{dir}/"), history.last['report']
      assert_equal Digest::SHA256.file(path).hexdigest, history.last['report_sha256']
      again = PromptRsi::Report.build(suite: 'model-eval', provenance: PromptRsiGateTest::PROVENANCE, result: result,
                                      generated_at: Time.utc(2026, 9, 30, 12))
      PromptRsi::Report.archive(again, dir, history: false)
      assert_equal 1, File.readlines(File.join(dir, 'history.jsonl')).size
    end
  end

  # 同一天、同一变体、同一模型重跑：每次各占一份报告，history 能逐行指回原件；
  # 同一份报告再归档一次是撞名，拒绝覆盖。
  def test_reruns_never_overwrite_earlier_evidence
    provenance = PromptRsiGateTest::PROVENANCE
    at = Time.utc(2026, 10, 2, 1)
    first = PromptRsi::Report.build(suite: 'model-eval', provenance: provenance, generated_at: at,
                                    result: { 'verdict' => 'candidate_passes', 'checks' => [], 'problems' => [] })
    second = PromptRsi::Report.build(suite: 'model-eval', provenance: provenance, generated_at: at,
                                     result: { 'verdict' => 'rejected', 'checks' => [], 'problems' => [] })
    refute_equal first['run_id'], second['run_id']
    Dir.mktmpdir do |dir|
      paths = [first, second].map { |report| PromptRsi::Report.archive(report, dir) }
      assert_equal %w[candidate_passes rejected], paths.map { |path| JSON.parse(File.read(path, encoding: 'UTF-8'))['verdict'] }
      history = File.readlines(File.join(dir, 'history.jsonl')).map { |line| JSON.parse(line) }
      assert_equal(paths.map { |path| path.delete_prefix("#{dir}/") }, history.map { |row| row['report'] })
      before = File.read(paths.first, encoding: 'UTF-8')
      assert_raises(PromptRsi::Report::ArchiveExists) { PromptRsi::Report.archive(first.merge('verdict' => 'rejected'), dir) }
      assert_equal before, File.read(paths.first, encoding: 'UTF-8')
      assert_equal 2, history.size
      assert_raises(PromptRsi::Report::ArchiveExists) do
        2.times { PromptRsi::Report.save_suggestion_evidence(dir, first, baseline: { 'cases' => [] }, candidate: { 'cases' => [] }) }
      end
    end
  end

  def test_dataset_hash_changes_with_content_and_paths
    Dir.mktmpdir do |dir|
      File.write(File.join(dir, 'a.json'), '{}')
      first = PromptRsi::Report.dataset_sha256(dir)
      assert_equal first, PromptRsi::Report.dataset_sha256(dir)
      File.write(File.join(dir, 'a.json'), '{"x":1}')
      refute_equal first, PromptRsi::Report.dataset_sha256(dir)
      second = PromptRsi::Report.dataset_sha256(dir)
      File.rename(File.join(dir, 'a.json'), File.join(dir, 'b.json'))
      refute_equal second, PromptRsi::Report.dataset_sha256(dir)
    end
  end
end

# 输入建议套件的人工复评：样本随报告归档，填完 judged 后 `--rescore` 只读归档重算。
class PromptRsiRescoreTest < Minitest::Test
  PROVENANCE = PromptRsiGateTest::PROVENANCE

  # 一侧的原始报告：reject 3、none 4、suggest 4（都给出了建议）。
  def raw(bundle)
    cases = (0...3).map { |index| { 'id' => "r#{index}", 'expect' => 'reject', 'raw' => 'NONE', 'cleaned' => nil } } +
            (0...4).map { |index| { 'id' => "n#{index}", 'expect' => 'none', 'raw' => 'NONE', 'cleaned' => nil } } +
            (0...4).map { |index| { 'id' => "s#{index}", 'expect' => 'suggest', 'raw' => "go #{index}", 'cleaned' => "go #{index}" } }
    { 'model' => 'glm-5', 'prompt_variant' => bundle && { 'id' => 'v', 'bundle' => bundle }, 'cases' => cases }
  end

  # 走驱动的同一条路径：判定、存样本、归档；返回报告路径。
  def archived_run(archive)
    raws = { 'baseline' => raw(nil), 'candidate' => raw(PROVENANCE['candidate_bundle']) }
    report = PromptRsiEval.judge_suggestion(PROVENANCE.dup, raws)
    report['evidence'] = PromptRsi::Report.save_suggestion_evidence(archive, report, baseline: raws['baseline'],
                                                                                     candidate: raws['candidate'])
    report['evidence_sha256'] = raws.transform_values { |side| PromptRsi::Report.evidence_digest(side) }
    PromptRsi::Report.archive(report, archive)
  end

  def judge(archive, report_path, side, verdicts)
    relative = JSON.parse(read(report_path))['evidence']
    path = File.join(archive, relative, "#{side}.json")
    evidence = JSON.parse(read(path))
    evidence['cases'].select { |row| row['expect'] == 'suggest' }.zip(verdicts) { |row, verdict| row['judged'] = verdict }
    yield evidence if block_given?
    File.write(path, JSON.generate(evidence))
  end

  # 报告是 UTF-8；夜跑或 CI 的 locale 可能是 US-ASCII，读时显式指定。
  def read(path)
    File.read(path, encoding: 'UTF-8')
  end

  def rescore(path)
    code = nil
    capture_io { code = PromptRsiEval.rescore({ rescore: path, history: true }) }
    code
  end

  def test_a_waiting_report_keeps_both_sides_and_rescoring_needs_no_model
    Dir.mktmpdir do |archive|
      path = archived_run(archive)
      original = read(path)
      report = JSON.parse(original)
      assert_equal 'needs_human_judging', report['verdict']
      markdown = read(path.sub(/\.json\z/, '.md'))
      assert_includes markdown, '## 待人工判定'
      assert_includes markdown, 'candidate：该判 4 条，已判 0 条，待判 4 条：s0, s1, s2, s3'
      assert_includes markdown, '--rescore'
      evidence = JSON.parse(read(File.join(archive, report['evidence'], 'candidate.json')))
      assert(evidence['cases'].select { |row| row['expect'] == 'suggest' }.all? { |row| row.key?('judged') })

      judge(archive, path, 'baseline', %w[plausible plausible plausible off-topic])
      judge(archive, path, 'candidate', %w[plausible plausible plausible])
      assert_equal 1, rescore(path), 'one candidate suggestion still unjudged'
      judge(archive, path, 'candidate', %w[plausible plausible plausible plausible])
      assert_equal 0, rescore(path)

      assert_equal original, read(path), 'the original report is never rewritten'
      rescored = Dir[File.join(archive, 'reports', '*', '*-rescored-*.json')].map { |file| JSON.parse(read(file)) }
      assert_equal %w[candidate_passes needs_human_judging], rescored.map { |item| item['verdict'] }.sort,
                   'each rescore keeps its own report, even within the same second'
      assert(rescored.all? { |item| item['rescored_from'] == path.delete_prefix("#{archive}/") })
      history = File.readlines(File.join(archive, 'history.jsonl')).map { |line| JSON.parse(line) }
      assert_equal 3, history.size
      assert_equal path.delete_prefix("#{archive}/"), history.last['rescored_from']
    end
  end

  def test_rescoring_refuses_evidence_changed_beyond_the_judgements
    Dir.mktmpdir do |archive|
      path = archived_run(archive)
      judge(archive, path, 'candidate', %w[plausible plausible plausible plausible]) do |evidence|
        evidence['cases'].last['cleaned'] = 'a nicer suggestion'
      end
      error = assert_raises(SystemExit) { capture_io { PromptRsiEval.rescore({ rescore: path, history: true }) } }
      refute error.success?
    end
  end
end
