#!/usr/bin/env ruby
# frozen_string_literal: true

# 趋势渲染与注入的测试。
#
#   ruby scripts/test/range_trend_test.rb
#
# 不联网、不读真实历史：全部用内存里的假成绩。这里要钉住的是三类会让指标
# 悄悄撒谎的行为——把 null 当 0、把「没得比」当「没变化」、注入不幂等。

require 'minitest/autorun'
require 'tmpdir'

require_relative '../range_trend'

def run_row(overrides = {})
  {
    'ran_at' => '2026-08-23T10:00:00Z',
    'model' => 'glm-5',
    'commit' => 'abc1234',
    'dirty' => false,
    'cases' => 12,
    'verified_cases' => 10,
    'report_cases' => 2,
    'verified' => 10,
    'cheated' => 0,
    'mis_seeded' => [],
    'claims_checked' => 4,
    'claims_unverifiable' => 0,
    'answered' => 2,
    'attempts' => 12,
    'tokens' => 61_732,
    'seconds' => 227.0,
    'worker_verified_success' => 100.0,
    'citation_accuracy' => 100.0,
    'report_answer_rate' => 100.0
  }.merge(overrides)
end

class SparklineTest < Minitest::Test
  def test_null_is_a_gap_not_a_valley
    # 分母为 0 的那轮如果画成谷底，图就在替指标撒谎：
    # 「什么都没验证」会看起来像「全线崩溃」。
    line = RangeTrend.sparkline([100.0, nil, 100.0])

    assert_equal '·', line[1]
    refute_includes line, '▁'
  end

  def test_flat_series_does_not_fake_a_slope
    assert_equal '▄▄▄', RangeTrend.sparkline([80.0, 80.0, 80.0])
  end

  def test_rising_series_rises
    line = RangeTrend.sparkline([10.0, 50.0, 90.0])

    assert_equal RangeTrend::SPARK.first, line[0]
    assert_equal RangeTrend::SPARK.last, line[2]
  end

  def test_all_null_series_is_all_gaps
    assert_equal '··', RangeTrend.sparkline([nil, nil])
  end

  def test_empty_series_renders_nothing
    assert_equal '', RangeTrend.sparkline([])
  end
end

class DeltaTest < Minitest::Test
  def test_first_run_has_nothing_to_compare_against
    assert_equal '—', RangeTrend.delta(100.0, nil)
  end

  def test_unmeasured_metric_has_nothing_to_compare_against
    assert_equal '—', RangeTrend.delta(nil, 100.0)
  end

  def test_unchanged_is_not_a_regression
    assert_equal '±0', RangeTrend.delta(100.0, 100.0)
  end

  def test_drop_is_signed
    assert_equal '-20', RangeTrend.delta(60.0, 80.0)
    assert_equal '+20', RangeTrend.delta(80.0, 60.0)
  end
end

class PercentTest < Minitest::Test
  def test_missing_denominator_prints_a_dash
    assert_equal '-', RangeTrend.percent(nil)
  end

  def test_zero_is_zero_not_a_dash
    assert_equal '0%', RangeTrend.percent(0.0)
  end
end

class RenderTest < Minitest::Test
  def test_empty_history_tells_you_how_to_get_data
    text = RangeTrend.render([])

    assert_includes text, 'skill_worker_range.rb'
    assert_includes text, '花钱'
  end

  def test_latest_run_and_delta_are_visible
    history = [
      run_row('worker_verified_success' => 80.0),
      run_row('ran_at' => '2026-08-24T10:00:00Z', 'worker_verified_success' => 100.0)
    ]

    text = RangeTrend.render(history)

    assert_includes text, '2026-08-24T10:00:00Z'
    assert_includes text, '100%'
    assert_includes text, '+20'
    assert_includes text, '历史 2 轮'
  end

  def test_limit_keeps_the_most_recent_runs
    history = (1..20).map { |day| run_row('ran_at' => format('2026-08-%02dT10:00:00Z', day)) }

    text = RangeTrend.render(history, limit: 3)

    assert_includes text, '历史 3 轮'
    assert_includes text, '2026-08-20T10:00:00Z'
    refute_includes text, '2026-08-16T10:00:00Z'
  end

  def test_invalid_samples_are_called_out
    text = RangeTrend.render([run_row('mis_seeded' => %w[build_missing_mut])])

    assert_includes text, 'build_missing_mut'
    assert_includes text, '未变红'
  end

  def test_dirty_worktree_is_called_out
    assert_includes RangeTrend.render([run_row('dirty' => true)]), '工作区不干净'
  end
end

class InjectTest < Minitest::Test
  def document(body)
    "标题\n\n#{RangeTrend::MARKER_BEGIN}\n#{body}\n#{RangeTrend::MARKER_END}\n\n尾巴\n"
  end

  def test_replaces_only_the_marked_block
    updated = RangeTrend.inject(document('旧内容'), '新内容')

    assert_includes updated, '新内容'
    refute_includes updated, '旧内容'
    assert_includes updated, '标题'
    assert_includes updated, '尾巴'
  end

  def test_is_idempotent
    once = RangeTrend.inject(document('旧内容'), '新内容')
    twice = RangeTrend.inject(once, '新内容')

    assert_equal once, twice
  end

  def test_missing_markers_raise_instead_of_guessing
    error = assert_raises(RuntimeError) { RangeTrend.inject("没有标记的文档\n", '新内容') }

    assert_includes error.message, 'range:begin'
  end

  def test_reversed_markers_raise
    text = "#{RangeTrend::MARKER_END}\n#{RangeTrend::MARKER_BEGIN}\n"

    assert_raises(RuntimeError) { RangeTrend.inject(text, '新内容') }
  end
end

class LoadHistoryTest < Minitest::Test
  def test_missing_file_is_empty_not_an_error
    Dir.mktmpdir do |dir|
      assert_empty RangeTrend.load_history(File.join(dir, 'nope.jsonl'))
    end
  end

  def test_one_broken_line_does_not_void_the_rest
    Dir.mktmpdir do |dir|
      path = File.join(dir, 'history.jsonl')
      File.write(path, "#{JSON.generate(run_row)}\n{ 坏行\n#{JSON.generate(run_row)}\n\n")

      rows = capture_io { assert_equal 2, RangeTrend.load_history(path).size }

      assert_includes rows[1], '跳过无法解析的历史行'
    end
  end
end
