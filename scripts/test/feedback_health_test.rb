#!/usr/bin/env ruby
# frozen_string_literal: true

require 'minitest/autorun'
require 'tmpdir'
require_relative '../lib/feedback_health'

class FeedbackHealthTest < Minitest::Test
  def row(signal, extra = {})
    { 'schema' => FeedbackHealth::SCHEMA, 'ts' => '2026-10-08T00:00:00Z', 'signal' => signal,
      'text' => 'PRIVATE-SENTINEL', 'sent_text' => 'PRIVATE-SENTINEL' }.merge(extra)
  end

  def test_health_separates_missing_measurements_from_zero_and_does_not_emit_text
    rows = [row('suggestion_shown', 'suggestion_id' => 'a'), row('suggestion_accepted', 'suggestion_id' => 'a'),
            row('suggestion_shown', 'suggestion_id' => 'b'), row('suggestion_sent_verbatim', 'suggestion_id' => 'b'),
            row('user_followup', 'followup_hint' => 'other'), row('user_followup', 'prev_turn_id' => 't1'),
            row('run_verified', 'verification' => 'unverified'), row('tool_failed', 'tool' => 'edit_file', 'error_class' => 'io')]
    report = FeedbackHealth.summarize(rows)
    assert_equal 50.0, report['followups']['linked_percent']
    assert_equal 1, report['suggestions']['unsettled_unique']
    assert_nil report['verification']['coverage_percent']
    assert_nil report['suggestions']['generation_attempts']
    assert_equal 'investigate_only', report['investigations'].first['status']
    refute_includes JSON.generate(report), 'PRIVATE-SENTINEL'
    refute_includes FeedbackHealth.markdown(report), 'PRIVATE-SENTINEL'
  end

  def test_bad_rows_duplicates_and_unknown_schema_do_not_inflate_counts
    Dir.mktmpdir do |dir|
      valid = JSON.generate(row('user_followup', 'id' => 'id'))
      File.write(File.join(dir, '2026-10.jsonl'), [valid, valid, '{broken', JSON.generate(row('x', 'schema' => 'other'))].join("\n"))
      rows, problems = FeedbackHealth.read(dir)
      assert_equal 1, rows.size
      assert_equal({ 'duplicates' => 1, 'malformed' => 1, 'unsupported_or_invalid' => 1 }, problems)
    end
  end

  def test_new_run_denominators_do_not_mix_old_partial_telemetry
    report = FeedbackHealth.summarize([
      row('run_started', 'run_id' => 'r1'), row('run_started', 'run_id' => 'r2'),
      row('run_verified', 'run_id' => 'r1', 'verification' => 'passed'),
      row('run_verified', 'verification' => 'unverified'),
      row('tool_succeeded', 'run_id' => 'r1'), row('tool_failed', 'run_id' => 'r1'), row('tool_failed')
    ])
    assert_equal 2, report['verification']['started_runs']
    assert_equal 50.0, report['verification']['coverage_percent']
    assert_equal 2, report['resolved_tools']['total']
    assert_equal 50.0, report['resolved_tools']['failure_percent']
  end
end
