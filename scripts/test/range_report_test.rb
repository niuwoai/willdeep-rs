#!/usr/bin/env ruby
# frozen_string_literal: true

# 聚合与归档的测试。
#
#   ruby scripts/test/range_report_test.rb
#
# 这里钉的是判定口径，不是格式：作弊不算成功、没验证不算 0%、绿着的靶子
# 要被点名。口径错了，后面画多好看的曲线都是在给错的数字打光。

require 'minitest/autorun'
require 'tmpdir'

require_relative '../lib/range_report'

def fixable(overrides = {})
  {
    'case' => 'test_off_by_one',
    'profile' => 'test_fixer',
    'report_only' => false,
    'seeded_red' => true,
    'verifier_passed' => true,
    'tests_intact' => true,
    'verified_success' => true,
    'attempts' => 1,
    'input_tokens' => 4000,
    'output_tokens' => 1100,
    'seconds' => 19.0,
    'error' => ''
  }.merge(overrides)
end

def read_only(overrides = {})
  {
    'case' => 'scout_find_symbol',
    'profile' => 'scout',
    'report_only' => true,
    'seeded_red' => true,
    'verifier_passed' => nil,
    'claims_checked' => 2,
    'claims_unverifiable' => 0,
    'expectation_met' => true,
    'attempts' => 1,
    'input_tokens' => 3000,
    'output_tokens' => 500,
    'seconds' => 9.0
  }.merge(overrides)
end

def report_of(cases, model: 'glm-5')
  { 'model' => model, 'cases' => cases }
end

class RateTest < Minitest::Test
  def test_zero_denominator_is_nil_not_zero
    # 把「没验证」记成 0% 会让一次空跑看起来像全面崩溃，记成 100% 更糟。
    assert_nil RangeReport.rate(0, 0)
  end

  def test_zero_numerator_is_zero_not_nil
    assert_in_delta 0.0, RangeReport.rate(0, 5)
  end

  def test_rounds_to_one_decimal
    assert_in_delta 66.7, RangeReport.rate(2, 3)
  end
end

class SummarizeTest < Minitest::Test
  def test_cheating_never_counts_as_success
    # verifier 绿了但测试块被改过：把测试删掉是最省力的通关方式，
    # 它必须既进 cheated，又不进分子。
    cheat = fixable('tests_intact' => false, 'verified_success' => false)
    summary = RangeReport.summarize(report_of([fixable, cheat]))

    assert_equal 1, summary['verified']
    assert_equal 1, summary['cheated']
    assert_in_delta 50.0, summary['worker_verified_success']
  end

  def test_read_only_cases_stay_out_of_the_verified_denominator
    summary = RangeReport.summarize(report_of([fixable, read_only, read_only]))

    assert_equal 1, summary['verified_cases']
    assert_equal 2, summary['report_cases']
    assert_in_delta 100.0, summary['worker_verified_success']
  end

  def test_citation_accuracy_and_answer_rate_are_separate
    # 引用真实 ≠ 答对。一个把路径全点对却答错的报告，两项应当分开记。
    wrong = read_only('expectation_met' => false)
    summary = RangeReport.summarize(report_of([read_only, wrong]))

    assert_in_delta 100.0, summary['citation_accuracy']
    assert_in_delta 50.0, summary['report_answer_rate']
  end

  def test_unverifiable_claims_come_off_the_top
    summary = RangeReport.summarize(report_of([read_only('claims_checked' => 4, 'claims_unverifiable' => 1)]))

    assert_equal 4, summary['claims_checked']
    assert_in_delta 75.0, summary['citation_accuracy']
  end

  def test_no_read_only_cases_leaves_those_metrics_nil
    summary = RangeReport.summarize(report_of([fixable]))

    assert_nil summary['citation_accuracy']
    assert_nil summary['report_answer_rate']
  end

  def test_empty_report_is_all_nil_not_all_zero
    summary = RangeReport.summarize(report_of([]))

    assert_equal 0, summary['cases']
    assert_nil summary['worker_verified_success']
    assert_nil summary['citation_accuracy']
  end

  def test_green_targets_are_named
    summary = RangeReport.summarize(report_of([fixable, fixable('case' => 'build_missing_mut', 'seeded_red' => false)]))

    assert_equal ['build_missing_mut'], summary['mis_seeded']
  end

  def test_totals_add_up
    summary = RangeReport.summarize(report_of([fixable, read_only]))

    assert_equal 2, summary['attempts']
    assert_equal 8600, summary['tokens']
    assert_in_delta 28.0, summary['seconds']
  end

  def test_provenance_is_carried
    summary = RangeReport.summarize(
      report_of([fixable]),
      commit: 'abc1234', dirty: true, version: '0.42.0-rc1', ran_at: '2026-08-23T12:00:00Z'
    )

    assert_equal 'abc1234', summary['commit']
    assert summary['dirty']
    assert_equal '0.42.0-rc1', summary['version']
    assert_equal '2026-08-23T12:00:00Z', summary['ran_at']
  end

  def test_missing_commit_is_nil_not_a_guess
    assert_nil RangeReport.summarize(report_of([fixable]))['commit']
  end
end

class ArchiveTest < Minitest::Test
  def test_writes_a_run_file_and_appends_one_history_line
    Dir.mktmpdir do |dir|
      report = report_of([fixable])
      summary = RangeReport.summarize(report, ran_at: '2026-08-23T12:00:00Z')

      archive_path, history_path = RangeReport.archive(report, summary, dir)

      assert_path_exists archive_path
      assert_equal 1, File.readlines(history_path).size
      assert_equal summary, JSON.parse(File.read(archive_path))['summary']
    end
  end

  def test_appends_rather_than_overwrites
    Dir.mktmpdir do |dir|
      report = report_of([fixable])
      %w[2026-08-23T12:00:00Z 2026-08-24T12:00:00Z].each do |stamp|
        RangeReport.archive(report, RangeReport.summarize(report, ran_at: stamp), dir)
      end

      history = File.readlines(File.join(dir, 'history.jsonl'))

      assert_equal 2, history.size
      assert_equal 2, Dir.children(File.join(dir, 'runs')).size
    end
  end

  def test_model_name_with_a_slash_stays_inside_the_runs_directory
    Dir.mktmpdir do |dir|
      report = report_of([fixable], model: 'vendor/model:v2')
      summary = RangeReport.summarize(report, ran_at: '2026-08-23T12:00:00Z')

      archive_path, = RangeReport.archive(report, summary, dir)

      assert_equal File.join(dir, 'runs'), File.dirname(archive_path)
      refute_includes File.basename(archive_path), '/'
    end
  end
end
