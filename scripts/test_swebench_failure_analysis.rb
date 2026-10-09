require 'minitest/autorun'
require_relative 'swebench_failure_analysis'

class SwebenchFailureAnalysisTest < Minitest::Test
  def observe(messages, patch = '')
    SwebenchFailureAnalysis.observe('trace' => {'sessions' => [{'messages' => messages}]}, 'patch' => patch)
  end

  def test_dispatch_is_only_observed_when_return_matches_the_call
    result = observe([
      {'role' => 'assistant', 'tool_calls' => [{'id' => 'dispatch', 'name' => 'spawn_agent'}]},
      {'role' => 'tool', 'tool_call_id' => 'another-call'}
    ])
    assert_equal 0, result['spawn_calls_with_saved_result']
    assert_equal ['spawn_agent'], result['last_proposed_tools_without_saved_result']
  end

  def test_matching_return_is_not_classified_as_pending
    result = observe([
      {'role' => 'assistant', 'tool_calls' => [{'id' => 'dispatch', 'name' => 'spawn_agent'}]},
      {'role' => 'tool', 'tool_call_id' => 'dispatch', 'content' => 'Tool reported an error'}
    ])
    assert_equal 1, result['spawn_calls_with_saved_result']
    assert_empty result['last_proposed_tools_without_saved_result']
  end

  def test_missing_trace_is_distinct_from_an_observed_empty_patch
    missing = SwebenchFailureAnalysis.observe('patch' => '')
    assert missing['trace_missing']
    refute observe([{'role' => 'assistant', 'content' => 'No change'}])['trace_missing']
  end

  def test_test_only_patch_is_not_a_mixed_implementation_patch
    tests = "diff --git a/tests/test_x.py b/tests/test_x.py\n"
    assert observe([], tests)['diagnostic_or_core_only']
    refute observe([], tests + "diff --git a/pkg/x.py b/pkg/x.py\n")['diagnostic_or_core_only']
  end

  def test_core_artifact_is_observed_without_claiming_a_model_cause
    result = observe([], "diff --git a/core b/core\n")
    assert result['diagnostic_or_core_only']
    assert result['trace_missing']
    refute result.key?('root_cause')
  end
end
