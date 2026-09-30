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

  def evaluate(candidate, holdout: holdout(2, 2), provenance: PROVENANCE)
    PromptRsi::Gate.evaluate(provenance: provenance, baseline: baseline_rows, candidate: candidate,
                             holdout: holdout)
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
    empty = PromptRsi::Gate.evaluate(provenance: PROVENANCE, baseline: baseline_rows, candidate: [])
    assert_equal 'non_reproducible', empty['verdict']
    assert_includes empty['problems'], 'candidate 这一轮没跑成（没有报告）'
    broken_holdout = holdout(2, 2)
    broken_holdout[:candidate][0] = row('h0', 'holdout', 'skipped')
    assert_equal 'non_reproducible', evaluate(candidate_rows, holdout: broken_holdout)['verdict']
  end

  def test_stage_one_without_holdout_is_never_a_pass
    result = PromptRsi::Gate.evaluate(provenance: PROVENANCE, baseline: baseline_rows, candidate: candidate_rows)
    assert_equal 'rejected', result['verdict']
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
end

class PromptRsiReportTest < Minitest::Test
  def test_reports_list_flips_hide_holdout_rows_and_append_history
    gate_test = PromptRsiGateTest.new('report')
    candidate = gate_test.candidate_rows(passed: 7)
    result = PromptRsi::Gate.evaluate(provenance: PromptRsiGateTest::PROVENANCE, baseline: gate_test.baseline_rows,
                                      candidate: candidate, holdout: gate_test.holdout(2, 2))
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
