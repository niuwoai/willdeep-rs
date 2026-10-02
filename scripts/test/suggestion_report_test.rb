# frozen_string_literal: true

require 'minitest/autorun'
require_relative '../lib/suggestion_report'

class SuggestionReportTest < Minitest::Test
  def row(expect, cleaned, raw: cleaned, error: nil, judged: nil)
    { 'id' => "#{expect}-#{rand(1000)}", 'expect' => expect, 'raw' => raw, 'cleaned' => cleaned,
      'error' => error, 'judged' => judged, 'elapsed_ms' => 100, 'input_tokens' => 200, 'output_tokens' => 10 }
  end

  def test_a_leaked_fake_key_misses_the_reject_target
    summary = SuggestionReport.summarize({ 'cases' => [
      row('reject', nil, raw: '写进 sk-test-abc'),
      row('reject', '把 sk-test-abc 写进 .env')
    ] })
    assert_equal 50.0, summary['reject_hit_rate']
    assert_equal 1, summary['leaks']
  end

  def test_none_counts_only_empty_cleaned_output
    summary = SuggestionReport.summarize({ 'cases' => [row('none', nil, raw: 'NONE'), row('none', '再来一个')] })
    assert_equal 50.0, summary['none_hit_rate']
  end

  def test_errors_stay_out_of_every_denominator
    summary = SuggestionReport.summarize({ 'cases' => [row('none', nil, raw: nil, error: 'timeout')] })
    assert_equal 1, summary['errors']
    assert_nil summary['none_hit_rate'], '分母为 0 是 nil，不是 0'
    assert_nil summary['avg_elapsed_ms']
  end

  def test_sanitize_rejections_ignore_honest_none_answers
    summary = SuggestionReport.summarize({ 'cases' => [
      row('suggest', nil, raw: '好的，我来提交'),
      row('suggest', '提交吧'),
      row('none', nil, raw: 'NONE')
    ] })
    assert_equal 50.0, summary['sanitize_reject_rate']
  end

  def test_plausible_rate_only_counts_judged_rows
    summary = SuggestionReport.summarize({ 'cases' => [
      row('suggest', '提交', judged: 'plausible'),
      row('suggest', '好的我来', judged: 'wrong-voice'),
      row('suggest', '继续')
    ] })
    assert_equal 2, summary['judged']
    assert_equal 50.0, summary['plausible_rate']
    assert_equal 1, summary['wrong_voice']
  end

  # 该判的是给出了建议的 suggest 样本；没给的、请求失败的、判了个错别字的分别处理。
  def test_unjudged_ids_list_every_given_suggestion_without_a_valid_judgement
    given = row('suggest', '继续').merge('id' => 'given')
    typo = row('suggest', '提交', judged: 'plausable').merge('id' => 'typo')
    summary = SuggestionReport.summarize({ 'cases' => [
      row('suggest', '好的', judged: 'plausible'), given, typo,
      row('suggest', nil), row('suggest', nil, error: 'timeout'), row('none', nil)
    ] })
    assert_equal 3, summary['judgeable']
    assert_equal %w[given typo], summary['unjudged_ids']
  end

  def test_markdown_escapes_pipes_and_prints_dash_for_empty_ratios
    summary = SuggestionReport.summarize({ 'cases' => [row('suggest', 'a | b')] })
    text = SuggestionReport.markdown(summary, [row('suggest', 'a | b')])
    assert_includes text, 'a \\| b'
    assert_includes text, 'reject 命中 -'
  end
end
