# frozen_string_literal: true
require 'minitest/autorun'
require 'rbconfig'
require_relative '../agent_reliability_eval'

class AgentReliabilityEvalTest < Minitest::Test
  def test_recovery_requires_a_durable_completed_write_and_rejects_replay
    Dir.mktmpdir('evaluation-recovery-') do |root|
      home = File.join(root, 'home')
      workspace = File.join(root, 'workspace')
      id = '00000000-0000-4000-8000-000000000001'
      path = File.join(home, 'sessions', "#{id}.json")
      call = {'id' => 'marker-call', 'name' => 'run_command', 'arguments' => JSON.generate({'command' => AgentEvalRecovery::COMMAND})}
      session = {'id' => id, 'messages' => [{'role' => 'assistant', 'tool_calls' => [call]}, {'role' => 'tool', 'tool_call_id' => 'marker-call'}], 'execution_checkpoint' => {'status' => 'running', 'pending_call_ids' => []}}
      AgentReliabilityEval.write(File.join(workspace, AgentEvalRecovery::MARKER_PATH), AgentEvalRecovery::MARKER)
      AgentReliabilityEval.write(path, JSON.generate(session))
      boundary = AgentEvalRecovery.boundary(home, workspace)
      assert_equal({session_id: id, marker_call_id: 'marker-call'}, boundary)
      session['execution_checkpoint']['pending_call_ids'] = ['marker-call']
      File.write(path, JSON.generate(session))
      assert_nil AgentEvalRecovery.boundary(home, workspace)
      session['execution_checkpoint'] = {'status' => 'completed', 'pending_call_ids' => []}
      File.write(path, JSON.generate(session))
      assert_nil AgentEvalRecovery.boundary(home, workspace)
      assert AgentEvalRecovery.recovered?(home, workspace, {'session_id' => id}, boundary)
      refute AgentEvalRecovery.recovered?(home, workspace, {'session_id' => 'other'}, boundary)
      session['messages'] << {'role' => 'assistant', 'tool_calls' => [call.merge('id' => 'replayed')]}
      File.write(path, JSON.generate(session))
      refute AgentEvalRecovery.recovered?(home, workspace, {'session_id' => id}, boundary)
      session['messages'].pop
      File.write(path, JSON.generate(session))
      File.open(File.join(workspace, AgentEvalRecovery::MARKER_PATH), 'a') { |file| file.write(AgentEvalRecovery::MARKER) }
      refute AgentEvalRecovery.recovered?(home, workspace, {'session_id' => id}, boundary)
    end
  end

  def test_normal_exit_cleans_descendants_that_keep_output_pipes_open
    Dir.mktmpdir('evaluation-descendants-') do |root|
      script = 'fork { File.write("started", "yes"); sleep 1; File.write("escaped", "bad") }; sleep 0.01 until File.exist?("started"); exit 7'
      code, timeout, elapsed = AgentReliabilityEval.run_process([RbConfig.ruby, '-e', script], {}, root, 5, root)
      assert_equal 7, code
      refute timeout
      assert File.exist?(File.join(root, 'started'))
      assert_operator elapsed, :<, 3
      sleep 1.2
      refute File.exist?(File.join(root, 'escaped'))
    end
  end

  def test_injected_interruption_is_distinct_from_timeout_and_cleans_the_group
    Dir.mktmpdir('evaluation-interruption-') do |root|
      script = 'fork { sleep 1; File.write("escaped", "bad") }; File.write("ready", "yes"); sleep 30'
      code, timeout, _, interrupted = AgentReliabilityEval.run_process([RbConfig.ruby, '-e', script], {}, root, 5, root, interrupt_when: -> { File.exist?(File.join(root, 'ready')) })
      assert_nil code
      refute timeout
      assert interrupted
      sleep 1.2
      refute File.exist?(File.join(root, 'escaped'))
    end
  end

  def test_owner_death_closes_liveness_pipe_and_kills_the_supervised_group
    Dir.mktmpdir('evaluation-owner-death-') do |root|
      owner = fork do
        AgentReliabilityEval.run_process([RbConfig.ruby, '-e', 'File.write("ready", "yes"); sleep 2; File.write("escaped", "bad")'], {}, root, 30, root)
        exit! 0
      end
      begin
        deadline = Process.clock_gettime(Process::CLOCK_MONOTONIC) + 5
        until File.exist?(File.join(root, 'ready'))
          raise 'supervised command did not start' if Process.clock_gettime(Process::CLOCK_MONOTONIC) >= deadline
          sleep 0.01
        end
        Process.kill('KILL', owner)
        Process.wait2(owner)
        owner = nil
        sleep 2.2
        refute File.exist?(File.join(root, 'escaped'))
      ensure
        if owner
          Process.kill('KILL', owner)
          Process.wait2(owner)
        end
      end
    end
  end

  def test_observer_failure_still_cleans_the_supervised_command
    Dir.mktmpdir('evaluation-observer-failure-') do |root|
      script = 'File.write("ready", "yes"); sleep 1; File.write("escaped", "bad")'
      assert_raises(RuntimeError) do
        AgentReliabilityEval.run_process([RbConfig.ruby, '-e', script], {}, root, 5, root,
          interrupt_when: -> { raise 'observer failed' if File.exist?(File.join(root, 'ready')) })
      end
      sleep 1.2
      refute File.exist?(File.join(root, 'escaped'))
    end
  end

  def test_compression_requires_real_event_smaller_checkpoint_and_original_constraint
    Dir.mktmpdir('evaluation-compression-') do |home|
      id = AgentReliabilityEval.seed_compression_session(home, '/fixture')
      path = File.join(home, 'sessions', "#{id}.json")
      session = AgentEvalObservation.object(path)
      events = [{'type' => 'compression_completed'}]
      refute AgentReliabilityEval.compression_evidence(home, id, events)
      session['compression_checkpoint'] = {'generation' => 1, 'previous_message_count' => 11, 'compressed_message_count' => 8}
      File.write(path, JSON.generate(session))
      assert AgentReliabilityEval.compression_evidence(home, id, events)
      refute AgentReliabilityEval.compression_evidence(home, id, [])
      session['messages'].first['content'] = 'constraint lost'
      File.write(path, JSON.generate(session))
      refute AgentReliabilityEval.compression_evidence(home, id, events)
    end
  end

  def test_process_input_reaches_the_cli_bridge
    Dir.mktmpdir('evaluation-input-') do |root|
      code, timeout, = AgentReliabilityEval.run_process([RbConfig.ruby, '-e', 'STDOUT.write(STDIN.read)'], {}, root, 5, root, stdin_data: '{"prompt":"/compress"}')
      assert_equal 0, code
      refute timeout
      assert_equal '{"prompt":"/compress"}', File.read(File.join(root, 'stdout.log'))
    end
  end

  def test_subtask_evidence_requires_matching_ordered_completed_foreground_reviewer
    id = '00000000-0000-4000-8000-000000000001'
    started = {'type' => 'subagent_started', 'id' => id, 'profile' => AgentEvalObservation::READ_ONLY_PROFILE, 'background' => false}
    completed = {'type' => 'subagent_completed', 'id' => id, 'status' => 'completed'}
    assert AgentEvalObservation.completed_read_only_child?([started, completed])
    [[], [started], [completed, started], [started, completed.merge('id' => 'other')],
     [started, completed.merge('status' => 'partial')],
     [started.merge('background' => true), completed],
     [started.merge('profile' => 'editor'), completed],
     [{'type' => 'assistant_text', 'text' => JSON.generate([started, completed])}]].each do |events|
      refute AgentEvalObservation.completed_read_only_child?(events)
    end
  end

  def test_ndjson_requires_a_complete_final_record_and_never_parses_nested_prose
    Dir.mktmpdir('evaluation-ndjson-') do |root|
      path = File.join(root, 'stdout.log')
      final = {'type' => 'completed', 'session_id' => '00000000-0000-4000-8000-000000000001'}
      File.write(path, JSON.generate({'type' => 'assistant_text', 'text' => JSON.generate(final)}) + "\n")
      assert_equal [{}, []], AgentEvalObservation.cli_output(path, ndjson: true)
      File.write(path, JSON.generate(final) + "\n")
      result, events = AgentEvalObservation.cli_output(path, ndjson: true)
      assert_equal final, result
      assert_equal [final], events
      File.open(path, 'a') { |file| file.write('{truncated') }
      assert_equal [{}, []], AgentEvalObservation.cli_output(path, ndjson: true)
    end
  end

  def test_non_object_or_broken_json_is_not_a_completion
    Dir.mktmpdir('evaluation-json-') do |root|
      path = File.join(root, 'output.json')
      ['', '[]', 'null', '{broken', '"completed"'].each do |value|
        File.write(path, value)
        assert_equal({}, AgentEvalObservation.object(path))
      end
    end
  end

  def test_checkpoint_must_belong_to_the_reported_session_and_have_valid_counts
    Dir.mktmpdir('evaluation-checkpoint-') do |root|
      id = '00000000-0000-4000-8000-000000000001'
      path = File.join(root, 'sessions', "#{id}.json")
      AgentReliabilityEval.write(path, JSON.generate({id: id, execution_checkpoint: {input_tokens: -1, output_tokens: 23}}))
      assert_equal({'input_tokens' => nil, 'output_tokens' => 23}, AgentEvalObservation.checkpoint(root, {'session_id' => id}))
      assert_equal({}, AgentEvalObservation.checkpoint(root, {'session_id' => '../other'}))
      AgentReliabilityEval.write(path, JSON.generate({id: 'other', execution_checkpoint: {input_tokens: 100}}))
      assert_equal({}, AgentEvalObservation.checkpoint(root, {'session_id' => id}))
    end
  end

  def test_extra_build_configuration_cannot_hide_from_fixture_integrity
    Dir.mktmpdir('evaluation-integrity-') do |root|
      AgentReliabilityEval.write(File.join(root, 'src/lib.rs'), 'before')
      before = AgentEvalObservation.fixture_files(root, mutable: ['src/lib.rs'])
      AgentReliabilityEval.write(File.join(root, 'src/lib.rs'), 'after')
      assert_equal before, AgentEvalObservation.fixture_files(root, mutable: ['src/lib.rs'])
      AgentReliabilityEval.write(File.join(root, '.cargo/config.toml'), '[alias]')
      refute_equal before, AgentEvalObservation.fixture_files(root, mutable: ['src/lib.rs'])
    end
  end

  def test_external_verifier_ignores_replaced_workspace_tests
    Dir.mktmpdir('evaluation-independent-') do |root|
      AgentReliabilityEval.write(File.join(root, 'src/lib.rs'), AgentReliabilityEval::SOURCE)
      AgentReliabilityEval.write(File.join(root, 'tests/acceptance.rs'), '')
      refute AgentReliabilityEval.external_verify(root)
      AgentReliabilityEval.write(File.join(root, 'src/lib.rs'), AgentReliabilityEval::FIXED_SOURCE)
      assert AgentReliabilityEval.external_verify(root)
    end
  end

  def test_external_verifier_has_a_deadline_for_nonterminating_implementations
    Dir.mktmpdir('evaluation-loop-') do |root|
      AgentReliabilityEval.write(File.join(root, 'src/lib.rs'), 'pub fn sum_to(_: u64) -> u64 { loop { std::thread::yield_now(); } }')
      started = Process.clock_gettime(Process::CLOCK_MONOTONIC)
      refute AgentReliabilityEval.external_verify(root, seconds: 2)
      assert_operator Process.clock_gettime(Process::CLOCK_MONOTONIC) - started, :<, 10
    end
  end

  def test_capture_records_a_real_child_exit_and_logs
    Dir.mktmpdir('evaluation-capture-') do |root|
      code, timeout, elapsed = AgentReliabilityEval.run_process(
        [RbConfig.ruby, '-e', 'STDOUT.write("out"); STDERR.write("err"); exit 7'], {}, root, 5, root
      )
      assert_equal 7, code
      refute timeout
      assert_operator elapsed, :>=, 0
      assert_equal 'out', File.read(File.join(root, 'stdout.log'))
      assert_equal 'err', File.read(File.join(root, 'stderr.log'))
    end
  end

  def test_timeout_reaps_the_child_instead_of_leaving_the_runner_waiting
    Dir.mktmpdir('evaluation-timeout-') do |root|
      code, timeout, elapsed = AgentReliabilityEval.run_process(
        [RbConfig.ruby, '-e', 'sleep 30'], {}, root, 0.2, root
      )
      assert_nil code
      assert timeout
      assert_operator elapsed, :<, 5
    end
  end

  def test_deleted_protected_files_are_verification_failures_not_reader_crashes
    Dir.mktmpdir('evaluation-missing-') do |root|
      assert_nil AgentReliabilityEval.contents(File.join(root, 'missing'))
      assert_nil AgentReliabilityEval.contents(root)
    end
  end
end
