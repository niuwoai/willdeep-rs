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

class PromptRsiGateTest < Minitest::Test
  PROVENANCE = {
    'variant_id' => 'tone-conclusion-first', 'role' => 'main', 'section' => 'tone',
    'commit' => 'abc1234', 'dirty' => false, 'model' => 'glm-5', 'binary_version' => '0.87.0',
    'dataset_sha256' => 'd' * 64, 'variant_sha256' => 'v' * 64,
    'parent_bundle' => 'main@111111111111', 'candidate_bundle' => 'main@222222222222'
  }.freeze

  def row(task, split, status, overrides = {})
    { 'task' => task, 'split' => split, 'status' => status, 'false_completion' => false,
      'input_tokens' => 1000, 'output_tokens' => 200, 'elapsed_seconds' => 10.0 }.merge(overrides)
  end

  # validation 10 题、regression 2 题；baseline 过 6 题。
  def baseline_rows
    (0...10).map { |index| row("v#{index}", 'validation', index < 6 ? 'passed' : 'failed') } +
      [row('r0', 'regression', 'passed'), row('r1', 'regression', 'passed')]
  end

  def candidate_rows(passed: 7, regression: %w[passed passed], overrides: {})
    (0...10).map { |index| row("v#{index}", 'validation', index < passed ? 'passed' : 'failed', overrides) } +
      regression.each_with_index.map { |status, index| row("r#{index}", 'regression', status, overrides) }
  end

  def holdout(base_passed, cand_passed)
    { baseline: (0...4).map { |index| row("h#{index}", 'holdout', index < base_passed ? 'passed' : 'failed') },
      candidate: (0...4).map { |index| row("h#{index}", 'holdout', index < cand_passed ? 'passed' : 'failed') } }
  end

  # 任务清单：validation 10、regression 2、holdout 4，外加一道不参与对照的 train。
  def tasks
    (baseline_rows + holdout(0, 0)[:baseline]).to_h { |row| [row['task'], row['split']] }.merge('t0' => 'train')
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
    lying[9] = row('v9', 'validation', 'failed', 'false_completion' => true)
    assert_equal ['no_new_false_completions'], failed(evaluate(lying))
    cheating = candidate_rows(passed: 8)
    cheating[9] = row('v9', 'validation', 'cheated')
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
      'candidate 有重复的任务' => candidate_rows + [row('v0', 'validation', 'passed')],
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

  def test_holdout_that_never_ran_is_not_reproducible_rather_than_overfit
    result = evaluate(candidate_rows, holdout: { baseline: [], candidate: [] })
    assert_equal 'non_reproducible', result['verdict']
    assert_includes result['problems'], 'holdout baseline 这一轮没跑成（没有报告）'
    assert_includes result['problems'], 'holdout candidate 这一轮没跑成（没有报告）'
  end

  def test_holdout_keeps_the_same_floors_as_stage_one
    cheating = holdout(2, 2)
    cheating[:candidate][3] = row('h3', 'holdout', 'cheated', 'false_completion' => true)
    result = evaluate(candidate_rows, holdout: cheating)
    assert_equal 'rejected', result['verdict']
    assert_equal %w[holdout_no_new_false_completions holdout_no_new_cheating], failed(result)
    costly = holdout(2, 2)
    costly[:candidate] = costly[:candidate].map { |item| item.merge('input_tokens' => 100_000, 'elapsed_seconds' => 10_000.0) }
    assert_equal %w[holdout_token_growth holdout_time_growth], failed(evaluate(candidate_rows, holdout: costly))
    # 底线破了又掉了通过率：先算不合格，不记成过拟合。
    worse = holdout(3, 2)
    worse[:candidate][3] = row('h3', 'holdout', 'cheated')
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
    timeout = row('v9', 'validation', 'timeout', unmeasured)
    base = baseline_rows.map { |item| item['task'] == 'v9' ? timeout : item }
    cand = candidate_rows.map { |item| item['task'] == 'v9' ? timeout : item }
    result = PromptRsi::Gate.evaluate(provenance: PROVENANCE, baseline: base, candidate: cand, tasks: tasks,
                                      holdout: holdout(2, 2))
    assert_equal 'candidate_passes', result['verdict'], result.inspect
    assert_match(/可比 11 题/, result['checks'].find { |item| item['name'] == 'token_growth' }['detail'])
  end

  def suggestion_summary(overrides = {})
    { 'errors' => 0, 'reject_hit_rate' => 100.0, 'leaks' => 0, 'none_hit_rate' => 100.0,
      'suggest_given_rate' => 90.0, 'judged' => 0, 'plausible_rate' => nil, 'wrong_voice' => 0 }.merge(overrides)
  end

  def test_suggestion_variants_need_human_judging_before_they_pass
    gate = lambda do |candidate, baseline = suggestion_summary('judged' => 10, 'plausible_rate' => 80.0)|
      PromptRsi::Gate.suggestion(provenance: PROVENANCE, baseline: baseline, candidate: candidate)
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
      PromptRsi::Gate.suggestion(provenance: PROVENANCE, baseline: baseline, candidate: candidate)
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
    Dir.mktmpdir do |dir|
      path = PromptRsi::Report.archive(report, dir)
      assert_equal File.join(dir, 'reports', '2026-09-30', 'tone-conclusion-first-glm-5.json'), path
      history = File.readlines(File.join(dir, 'history.jsonl')).map { |line| JSON.parse(line) }
      assert_equal 'candidate_passes', history.last['verdict']
      assert_equal [], history.last['failed_checks']
      PromptRsi::Report.archive(report, dir, history: false)
      assert_equal 1, File.readlines(File.join(dir, 'history.jsonl')).size
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
