#!/usr/bin/env ruby
# frozen_string_literal: true

# 模型行为评测驱动器的测试。
#
#   ruby scripts/test/model_eval_test.rb
#
# 不联网、不调 willdeep、不碰真实任务集：用临时目录里现造的 Ruby 小任务钉住
# 验收纪律（受保护文件动了算作弊、变异没抓住不算过、内容规则）、聚合口径
# （分母为 0 存 null、error 不进分母）、配置派生（凭据行原样、两张表被砍）
# 和趋势报警（掉 10 个点才报、没得比不报）。

require 'fileutils'
require 'json'
require 'minitest/autorun'
require 'tmpdir'

require_relative '../lib/model_eval/config'
require_relative '../lib/model_eval/report'
require_relative '../lib/model_eval/task'
require_relative '../lib/model_eval/verifier'
require_relative '../model_eval_trend'

module TaskFixture
  LIB = "module Adder\n  def self.add(a, b)\n    a - b\n  end\nend\n"
  FIXED = "module Adder\n  def self.add(a, b)\n    a + b\n  end\nend\n"
  TEST = "require 'minitest/autorun'\nrequire 'adder'\nclass AdderTest < Minitest::Test\n  def test_adds\n    assert_equal 3, Adder.add(1, 2)\n  end\nend\n"
  # 变异：只对 (1, 2) 返回 3，别的都错。只有补上第二个用例才能抓住它。
  MUTANT = "module Adder\n  def self.add(a, b)\n    a == 1 && b == 2 ? 3 : 0\n  end\nend\n"
  EXTRA_TEST = TEST.sub("  end\nend\n", "  end\n\n  def test_adds_more\n    assert_equal 9, Adder.add(4, 5)\n  end\nend\n")

  module_function

  def write(root, relative, text)
    path = File.join(root, relative)
    FileUtils.mkdir_p(File.dirname(path))
    File.write(path, text)
  end

  # 一个 fix 任务：lib/adder.rb 可改，test/ 受保护。
  def fix_task(root, extra_spec = {})
    dir = File.join(root, 'tasks', 'fix-adder')
    write(dir, 'task.json', JSON.generate({
      id: 'fix-adder', title: 'adder', kind: 'fix', language: 'ruby', requires: ['ruby'],
      editable: ['lib/adder.rb'], verify: [['ruby', '-Ilib', 'test/adder_test.rb']]
    }.merge(extra_spec)))
    write(dir, 'prompt.md', "fix it\n")
    write(dir, 'fixture/lib/adder.rb', LIB)
    write(dir, 'fixture/test/adder_test.rb', TEST)
    write(dir, 'solution/lib/adder.rb', FIXED)
    ModelEval::Task.load(dir)
  end

  # 一个 test 任务：test/ 可改，lib/ 受保护，带一个变异。
  def test_task(root)
    dir = File.join(root, 'tasks', 'test-adder')
    write(dir, 'task.json', JSON.generate({
      id: 'test-adder', title: 'adder tests', kind: 'test', language: 'ruby', requires: ['ruby'],
      editable: ['test/adder_test.rb'], verify: [['ruby', '-Ilib', 'test/adder_test.rb']], mutants: ['mutants/narrow']
    }))
    write(dir, 'prompt.md', "add tests\n")
    write(dir, 'fixture/lib/adder.rb', FIXED)
    write(dir, 'fixture/test/adder_test.rb', TEST)
    write(dir, 'solution/test/adder_test.rb', EXTRA_TEST)
    write(dir, 'mutants/narrow/lib/adder.rb', MUTANT)
    ModelEval::Task.load(dir)
  end

  def workspace_from(task, root, overrides = {})
    workspace = File.join(root, "ws-#{task.id}-#{overrides.keys.join('-').tr('/', '_')}")
    ModelEval::Verifier.copy_tree(task.fixture_dir, workspace)
    overrides.each { |relative, text| write(workspace, relative, text) }
    workspace
  end
end

class TaskLoadingTest < Minitest::Test
  def test_solution_must_stay_inside_editable
    Dir.mktmpdir do |root|
      dir = File.join(root, 'tasks', 'bad')
      TaskFixture.write(dir, 'task.json', JSON.generate({ id: 'bad', title: 'x', kind: 'fix', language: 'ruby',
                                                          editable: ['lib/a.rb'], verify: [['true']] }))
      TaskFixture.write(dir, 'prompt.md', "x\n")
      TaskFixture.write(dir, 'fixture/lib/a.rb', "a\n")
      TaskFixture.write(dir, 'solution/test/a_test.rb', "sneaky\n")
      error = assert_raises(ArgumentError) { ModelEval::Task.load(dir) }
      assert_includes error.message, 'test/a_test.rb'
    end
  end

  def test_test_kind_requires_mutants_and_mutants_must_not_touch_editable
    Dir.mktmpdir do |root|
      dir = File.join(root, 'tasks', 'nomut')
      TaskFixture.write(dir, 'task.json', JSON.generate({ id: 'nomut', title: 'x', kind: 'test', language: 'ruby',
                                                          editable: ['test/a_test.rb'], verify: [['true']] }))
      TaskFixture.write(dir, 'prompt.md', "x\n")
      FileUtils.mkdir_p(File.join(dir, 'fixture'))
      FileUtils.mkdir_p(File.join(dir, 'solution'))
      assert_raises(ArgumentError) { ModelEval::Task.load(dir) }

      TaskFixture.write(dir, 'task.json', JSON.generate({ id: 'nomut', title: 'x', kind: 'test', language: 'ruby',
                                                          editable: ['test/a_test.rb'], verify: [['true']],
                                                          mutants: ['mutants/m'] }))
      TaskFixture.write(dir, 'mutants/m/test/a_test.rb', "changing the model's file\n")
      error = assert_raises(ArgumentError) { ModelEval::Task.load(dir) }
      assert_includes error.message, '变异不能改 editable'
    end
  end

  def test_missing_requirements_are_named_not_fatal
    Dir.mktmpdir do |root|
      task = TaskFixture.fix_task(root, requires: ['ruby', 'definitely-not-a-real-binary-xyz'])
      assert_equal ['definitely-not-a-real-binary-xyz'], task.missing_requirements
    end
  end
end

class VerifierTest < Minitest::Test
  def test_self_check_is_red_then_green_for_both_kinds
    Dir.mktmpdir do |root|
      fix = ModelEval::Verifier.self_check(TaskFixture.fix_task(root))
      assert fix[:ok], fix.inspect
      test = ModelEval::Verifier.self_check(TaskFixture.test_task(root))
      assert test[:ok], test.inspect
      # test 类任务的「红」不是 verifier 红，而是变异活着。
      assert test[:red_detail][:verifier_passed]
      assert_equal 0, test[:red_detail][:mutants_caught]
    end
  end

  def test_editing_a_protected_test_cannot_turn_the_verifier_green
    Dir.mktmpdir do |root|
      task = TaskFixture.fix_task(root)
      # 模型把测试改成永远通过，但没修实现。
      workspace = TaskFixture.workspace_from(task, root, 'test/adder_test.rb' => "require 'minitest/autorun'\n")
      verdict = ModelEval::Verifier.evaluate(task, workspace)
      refute verdict[:verifier_passed]
      refute verdict[:passed]
    end
  end

  def test_snapshot_flags_protected_changes_and_ignores_artifacts
    Dir.mktmpdir do |root|
      task = TaskFixture.fix_task(root)
      workspace = TaskFixture.workspace_from(task, root)
      before = ModelEval::Verifier.snapshot(workspace, task.editable)
      TaskFixture.write(workspace, 'lib/adder.rb', TaskFixture::FIXED)
      TaskFixture.write(workspace, 'Cargo.lock', "generated\n")
      TaskFixture.write(workspace, '__pycache__/x.pyc', "bytes\n")
      TaskFixture.write(workspace, '.willdeep/state.json', "{}\n")
      assert_equal before, ModelEval::Verifier.snapshot(workspace, task.editable), 'editable 与产物不该触发'

      TaskFixture.write(workspace, 'notes.txt', "new file\n")
      refute_equal before, ModelEval::Verifier.snapshot(workspace, task.editable), '新加文件也算动了受保护区域'
    end
  end

  def test_mutant_survival_fails_a_test_task_even_when_tests_pass
    Dir.mktmpdir do |root|
      task = TaskFixture.test_task(root)
      # 模型只是把测试重写了一遍，没有加新用例：verifier 绿，变异活着。
      workspace = TaskFixture.workspace_from(task, root, 'test/adder_test.rb' => TaskFixture::TEST)
      verdict = ModelEval::Verifier.evaluate(task, workspace)
      assert verdict[:verifier_passed]
      assert_equal 0, verdict[:mutants_caught]
      refute verdict[:passed]

      good = TaskFixture.workspace_from(task, root, 'test/adder_test.rb' => TaskFixture::EXTRA_TEST)
      assert ModelEval::Verifier.evaluate(task, good)[:passed]
    end
  end

  def test_content_rules_are_enforced
    Dir.mktmpdir do |root|
      task = TaskFixture.fix_task(root, must_contain: { 'lib/adder.rb' => ['module Adder'] },
                                        must_not_contain: { 'lib/adder.rb' => ['eval('] })
      workspace = TaskFixture.workspace_from(task, root, 'lib/adder.rb' => TaskFixture::FIXED.sub('a + b', 'eval("a + b")'))
      verdict = ModelEval::Verifier.evaluate(task, workspace)
      assert verdict[:verifier_passed]
      assert_equal ['lib/adder.rb 不该出现 `eval(`'], verdict[:content_violations]
      refute verdict[:passed]
    end
  end

  def test_deleted_editable_file_is_absent_from_the_verifier_dir
    Dir.mktmpdir do |root|
      task = TaskFixture.fix_task(root)
      workspace = TaskFixture.workspace_from(task, root)
      FileUtils.rm(File.join(workspace, 'lib/adder.rb'))
      refute ModelEval::Verifier.evaluate(task, workspace)[:verifier_passed]
    end
  end
end

class ReportTest < Minitest::Test
  def row(overrides = {})
    { task: 't', kind: 'fix', language: 'ruby', status: 'passed', claimed: true, false_completion: false,
      turns: 4, narration_ratio: 0.5, reasoning_ratio: 0.0, silent_tool_turns: 1, tool_calls: 3, tool_failures: 0,
      input_tokens: 100, output_tokens: 50, elapsed_seconds: 10.0 }.merge(overrides)
  end

  def test_errors_and_skips_stay_out_of_the_denominator
    rows = [row, row(status: 'failed', claimed: true, false_completion: true), row(status: 'error', turns: nil),
            row(status: 'skipped', turns: nil, missing: ['node'])]
    summary = ModelEval::Report.summarize(model: 'm', rows: rows)
    assert_equal 2, summary['executed']
    assert_equal 50.0, summary['pass_rate']
    assert_equal 50.0, summary['false_completion_rate']
    assert_equal 1, summary['errors']
    assert_equal 1, summary['skipped']
  end

  def test_empty_denominator_is_null_not_zero
    summary = ModelEval::Report.summarize(model: 'm', rows: [row(status: 'error', turns: nil)])
    assert_nil summary['pass_rate']
    assert_nil summary['narration_ratio']
    assert_nil summary['tokens']
    assert_equal 0, summary['executed']
  end

  def test_narration_is_weighted_by_turns
    rows = [row(turns: 9, narration_ratio: 1.0), row(turns: 1, narration_ratio: 0.0)]
    assert_equal 90.0, ModelEval::Report.summarize(model: 'm', rows: rows)['narration_ratio']
  end

  def test_archive_writes_dated_report_and_appends_history_with_a_safe_name
    Dir.mktmpdir do |dir|
      rows = [row]
      summary = ModelEval::Report.summarize(model: 'org/model', rows: rows, ran_at: '2026-09-21T03:30:00Z')
      report = { 'rows' => rows.map { |r| r.transform_keys(&:to_s) } }
      json, markdown, history = ModelEval::Report.archive(report, summary, dir)
      assert_equal File.join(dir, 'reports', '2026-09-21', 'org_model.033000Z.json'), json
      assert File.file?(markdown)
      ModelEval::Report.archive(report, summary, dir)
      assert_equal 2, File.readlines(history).size
      assert_equal 'org/model', JSON.parse(File.readlines(history).first)['model']
      refute_includes File.read(markdown), 'api_key'
    end
  end
end

class ConfigTest < Minitest::Test
  SOURCE = <<~TOML
    default_provider = "some-im"

    [agent]
    approval = "smart" # 注释
    max_turns = 200

    [providers.some-im]
    api_base = "https://example.invalid/v1"
    api_key = "sk-do-not-print"

    [notifications]
    webhook_enabled = true
    webhook_url = "http://127.0.0.1:1/x"

    [mcp_servers.filesystem]
    command = "npx"

    [mcp_servers."dotted.name"]
    command = "x"

    [subagents.reviewer]
    model = "glm-5"
  TOML

  def test_strips_notifications_and_mcp_but_keeps_everything_else_verbatim
    derived = ModelEval::Config.derive(SOURCE)
    refute_includes derived, '[notifications]'
    refute_includes derived, 'webhook_url'
    refute_includes derived, 'mcp_servers'
    refute_includes derived, 'command ='
    assert_includes derived, 'default_provider = "some-im"'
    assert_includes derived, 'api_key = "sk-do-not-print"'
    assert_includes derived, 'approval = "smart" # 注释'
    assert_includes derived, "[subagents.reviewer]\nmodel = \"glm-5\""
    assert_equal derived, ModelEval::Config.derive(derived), '派生应幂等'
  end
end

class TrendTest < Minitest::Test
  def run_row(overrides = {})
    { 'ran_at' => '2026-09-21T03:30:00Z', 'model' => 'glm-5', 'commit' => 'abc1234', 'dirty' => false,
      'tasks' => 20, 'executed' => 20, 'passed' => 18, 'cheated' => 0, 'errors' => 0, 'timeouts' => 0,
      'pass_rate' => 90.0, 'narration_ratio' => 60.0, 'false_completion_rate' => 5.0, 'reasoning_ratio' => 0.0,
      'silent_tool_turns' => 3, 'tool_failures' => 1, 'seconds' => 400.0 }.merge(overrides)
  end

  def test_single_run_has_no_baseline_and_no_alarm
    assert_empty ModelEvalTrend.alarms([run_row])
    assert_includes ModelEvalTrend.render([run_row]), '### glm-5'
  end

  def test_baseline_prefers_the_run_a_week_or_more_ago
    history = [run_row('ran_at' => '2026-09-10T03:30:00Z', 'pass_rate' => 95.0),
               run_row('ran_at' => '2026-09-20T03:30:00Z', 'pass_rate' => 80.0),
               run_row('ran_at' => '2026-09-21T03:30:00Z', 'pass_rate' => 84.0)]
    base = ModelEvalTrend.baseline(ModelEvalTrend.by_model(history)['glm-5'], history.last)
    assert_equal '2026-09-10T03:30:00Z', base['ran_at']
    alarms = ModelEvalTrend.alarms(history)
    assert_equal 1, alarms.size
    assert_equal 'pass_rate', alarms.first[:key]
    assert_equal 11.0, alarms.first[:drop]
  end

  def test_drop_of_exactly_ten_points_does_not_alarm_and_null_metrics_are_skipped
    history = [run_row('ran_at' => '2026-09-20T03:30:00Z', 'pass_rate' => 90.0, 'narration_ratio' => nil),
               run_row('ran_at' => '2026-09-21T03:30:00Z', 'pass_rate' => 80.0, 'narration_ratio' => 10.0)]
    assert_empty ModelEvalTrend.alarms(history)
  end

  def test_models_are_alarmed_independently
    history = [run_row('model' => 'a', 'ran_at' => '2026-09-20T03:30:00Z', 'narration_ratio' => 70.0),
               run_row('model' => 'a', 'ran_at' => '2026-09-21T03:30:00Z', 'narration_ratio' => 40.0),
               run_row('model' => 'b', 'ran_at' => '2026-09-20T03:30:00Z'),
               run_row('model' => 'b', 'ran_at' => '2026-09-21T03:30:00Z')]
    alarms = ModelEvalTrend.alarms(history)
    assert_equal [['a', 'narration_ratio']], alarms.map { |alarm| [alarm[:model], alarm[:key]] }
    assert_includes ModelEvalTrend.render(history), '⚠️ **报警**'
  end

  def test_inject_is_idempotent
    doc = "# 标题\n\n<!-- model-eval:begin -->\n旧内容\n<!-- model-eval:end -->\n\n尾巴\n"
    once = ModelEvalTrend.inject(doc, '新内容')
    assert_equal "# 标题\n\n<!-- model-eval:begin -->\n新内容\n<!-- model-eval:end -->\n\n尾巴\n", once
    assert_equal once, ModelEvalTrend.inject(once, '新内容')
  end
end
