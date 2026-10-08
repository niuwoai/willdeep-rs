#!/usr/bin/env ruby
# frozen_string_literal: true

require 'minitest/autorun'
require 'tmpdir'
require_relative '../lib/model_eval/execution'
require_relative '../lib/prompt_rsi/preflight'

class EvalExecutionTest < Minitest::Test
  Task = Struct.new(:id, :content_sha256)

  def test_bounded_parallelism_and_stable_result_order
    active = peak = 0
    mutex = Mutex.new
    rows = ModelEval::Execution.run((0...8).to_a, jobs: 3) do |id|
      mutex.synchronize { active += 1; peak = [peak, active].max }
      sleep((8 - id) * 0.003)
      mutex.synchronize { active -= 1 }
      id
    end
    assert_equal (0...8).to_a, rows
    assert_equal 3, peak
    assert_equal 0, active
  end

  def test_worker_exception_is_not_silently_turned_into_a_missing_result
    previous = Thread.report_on_exception
    Thread.report_on_exception = false
    error = assert_raises(RuntimeError) { ModelEval::Execution.run([1, 2], jobs: 2) { raise 'failed task' } }
    assert_equal 'failed task', error.message
  ensure
    Thread.report_on_exception = previous
  end

  def test_resume_reuses_completed_rows_and_retries_infrastructure_errors
    Dir.mktmpdir do |dir|
      path = File.join(dir, 'state.json')
      manifest = { 'model' => 'test', 'binary' => 'abc' }
      checkpoint = ModelEval::Checkpoint.new(path, manifest)
      checkpoint.store('test', { task: 'ok', task_sha256: 'sha', status: 'passed' })
      checkpoint.store('test', { task: 'error', task_sha256: 'sha', status: 'error' })
      assert_raises(RuntimeError) { ModelEval::Checkpoint.new(path, manifest, resume: true) }
      checkpoint.close
      resumed = ModelEval::Checkpoint.new(path, manifest, resume: true)
      assert_equal 'passed', resumed.fetch('test', Task.new('ok', 'sha'))[:status]
      assert_nil resumed.fetch('test', Task.new('error', 'sha'))
      assert_raises(RuntimeError) { resumed.fetch('test', Task.new('ok', 'changed')) }
      resumed.close
      assert_raises(RuntimeError) { ModelEval::Checkpoint.new(path, { 'model' => 'different' }, resume: true) }
      assert_equal 0o600, File.stat(path).mode & 0o777
    end
  end

  def test_parallel_checkpoint_writes_preserve_all_completed_tasks
    Dir.mktmpdir do |dir|
      checkpoint = ModelEval::Checkpoint.new(File.join(dir, 'state.json'), {})
      ModelEval::Execution.run((0...30).to_a, jobs: 4) do |id|
        checkpoint.store('test', { task: id.to_s, task_sha256: 'sha', status: 'passed' })
      end
      checkpoint.close
      resumed = ModelEval::Checkpoint.new(File.join(dir, 'state.json'), {}, resume: true)
      30.times { |id| assert_equal id.to_s, resumed.fetch('test', Task.new(id.to_s, 'sha'))[:task] }
      resumed.close
    end
  end

  def test_preflight_rejects_dirty_source_or_wrong_binary_before_paid_execution
    Dir.mktmpdir do |root|
      File.write(File.join(root, 'Cargo.toml'), "version = \"1.0.0\"\n")
      ModelEval::Report.stub(:git_context, { commit: 'abc123', dirty: true }) do
        assert_raises(RuntimeError) { PromptRsi::Preflight.check!(root: root, binary_version: '1.0.0', binary_commit: 'abc123') }
      end
      ModelEval::Report.stub(:git_context, { commit: 'abc123', dirty: false }) do
        %w[wrong abc123-dirty].each do |commit|
          assert_raises(RuntimeError) { PromptRsi::Preflight.check!(root: root, binary_version: '1.0.0', binary_commit: commit) }
        end
        assert_raises(RuntimeError) { PromptRsi::Preflight.check!(root: root, binary_version: 'other', binary_commit: 'abc123') }
        assert_equal 'abc123', PromptRsi::Preflight.check!(root: root, binary_version: '1.0.0', binary_commit: 'abc123')[:commit]
      end
    end
  end
end
