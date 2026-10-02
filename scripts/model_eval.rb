#!/usr/bin/env ruby
# frozen_string_literal: true

# 固定任务集 × 模型：模型行为评测的驱动器。
#
# 它回答的不是「模型聪不聪明」，而是**「我改了提示词 / 路由 / 压缩之后，模型在
# 同一批小任务上是变好了还是变坏了」**。二十个可自动验收的小任务（修一个函数、
# 加一条测试、清一个 lint）放在 `bench/model-eval/tasks/`，每个模型跑一遍
# `willdeep run` 无头模式，会话落盘后用 `session_metrics.rb` 算人话率等行为指标，
# 再加上外部 verifier 的通过率，一起归档进 `bench/model-eval/`。
#
#   ruby scripts/model_eval.rb --check-tasks                 # 不联网：自检任务集红/绿
#   ruby scripts/model_eval.rb --model glm-5                 # 真花钱：跑一个模型
#   ruby scripts/model_eval.rb --model glm-5 --model deepseek-v4-flash --tasks rust-sum-inclusive,ruby-slug
#   ruby scripts/model_eval.rb --list
#
# 验收纪律与实弹靶场同源：在模型碰不到的干净目录里重建 fixture + 模型改过的
# editable 文件再跑 verifier；受保护文件被动过的算作弊，不进分子；「没跑」
# （缺可执行文件、Provider 出错）不进分母。报告只有计数，不含模型正文、不含凭据。

require 'digest'
require 'English'
require 'fileutils'
require 'json'
require 'optparse'
require 'rbconfig'
require 'time'
require 'tmpdir'

require_relative 'lib/agent_eval_observation'
require_relative 'lib/agent_eval_process'
require_relative 'lib/model_eval/config'
require_relative 'lib/model_eval/report'
require_relative 'lib/model_eval/task'
require_relative 'lib/model_eval/verifier'

module ModelEval
  ROOT = File.expand_path('..', __dir__)
  BENCH = File.join(ROOT, 'bench', 'model-eval')
  METRICS_SCRIPT = File.join(__dir__, 'session_metrics.rb')
  DEFAULT_TIMEOUT = 300
  DEFAULT_MAX_TURNS = 24
  # 这些退出码说明宿主或 Provider 没让模型开工：2 输入错、3 Provider 错、
  # 1 未分类、127 二进制不存在。它们记成 error，不进通过率的分母。
  INFRASTRUCTURE_EXITS = [1, 2, 3, 127].freeze
  UUID = /\A[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}\z/i

  class Driver
    def initialize(options)
      @options = options
      @tasks = select_tasks(Task.load_all(options[:bench]))
    end

    def list
      @tasks.each do |task|
        missing = task.missing_requirements
        puts format('%-30s %-8s %-7s %s%s', task.id, task.kind, task.language, task.title,
                    missing.empty? ? '' : "（缺 #{missing.join('、')}）")
      end
      0
    end

    # 不联网自检：每个任务 fixture 原样必须红，fixture + solution 必须绿。
    def check_tasks
      failures = 0
      @tasks.each do |task|
        missing = task.missing_requirements
        if missing.any?
          puts format('%-30s skipped   缺 %s', task.id, missing.join('、'))
          next
        end
        verdict = Verifier.self_check(task)
        state = verdict[:ok] ? 'ok' : "BAD（红：#{verdict[:red] ? '是' : '否'} 绿：#{verdict[:green] ? '是' : '否'}）"
        puts format('%-30s %s', task.id, state)
        failures += 1 unless verdict[:ok]
      end
      puts "#{@tasks.size} 个任务，#{failures} 个不合格"
      failures.zero? ? 0 : 1
    end

    def run
      raise '至少给一个 --model（或设 WILLDEEP_EVAL_MODELS）' if @options[:models].empty?

      @binary = resolve_binary(@options[:binary])
      @binary_version = binary_version(@binary)
      variant = check_variant(@options[:variant]) if @options[:variant]
      @run_root = Dir.mktmpdir('model-eval-run-')
      FileUtils.chmod(0o700, @run_root)
      @config = derive_config(@options[:config], @run_root)
      context = Report.git_context(ROOT)
      warn "工作区不干净：这轮成绩挂在一个没提交的状态上，回放不了。" if context[:dirty]
      FileUtils.mkdir_p(@options[:out])
      exit_code = 0
      @options[:models].each do |model|
        @hints = []
        rows = []
        @tasks.each do |task|
          rows << run_task(task, model)
          next unless Report.abort_early?(rows)

          warn "[#{model}] 开头 #{rows.size} 个任务全是 error，不再往下跑：多半是环境问题，不是模型问题。"
          break
        end
        if (reason = Report.unusable_reason(rows, @hints))
          # 一个都没执行的一轮不是成绩：不写报告、不进 history.jsonl，退出码 1。
          warn "[#{model}] 这一轮没跑成，不归档：#{reason}"
          warn '排查：用 --tasks <一个任务> --keep 重跑，看运行目录 logs/stderr.log；' \
               '夜跑（launchd）没有交互 shell 的环境变量，api_key_env 指向的变量、HTTPS_PROXY 要在 plist 或 ~/.bash_profile 里给。'
          puts JSON.generate({ model: model, executed: 0, errors: rows.count { |row| row[:status] == 'error' },
                               archived: [], unusable: reason })
          exit_code = 1
          next
        end
        summary = Report.summarize(model: model, rows: rows, commit: context[:commit], dirty: context[:dirty],
                                   version: workspace_version, binary_version: @binary_version)
        report = { 'schema_version' => 1, 'model' => model, 'binary_version' => @binary_version,
                   'max_turns' => @options[:turns], 'timeout_seconds' => @options[:timeout],
                   'created_at' => summary['ran_at'], 'variant' => variant,
                   'rows' => rows.map { |row| row.transform_keys(&:to_s) } }
        safe_model = model.gsub(/[^A-Za-z0-9._-]/, '_')
        File.write(File.join(@options[:out], "#{safe_model}.json"), "#{JSON.pretty_generate(report.merge('summary' => summary))}\n")
        File.write(File.join(@options[:out], "#{safe_model}.md"), Report.markdown(report, summary))
        archived = @options[:archive] ? Report.archive(report, summary, @options[:archive]) : []
        puts JSON.generate({ model: model, executed: summary['executed'], passed: summary['passed'],
                             pass_rate: summary['pass_rate'], narration_ratio: summary['narration_ratio'],
                             errors: summary['errors'], report: File.join(@options[:out], "#{safe_model}.md"),
                             archived: archived })
        exit_code = 1 if summary['executed'].zero?
      end
      exit_code
    ensure
      if @run_root
        if @options[:keep]
          warn "保留运行目录（含会话与日志，别提交）：#{@run_root}"
        else
          FileUtils.remove_entry(@run_root)
        end
      end
    end

    private

    def select_tasks(tasks)
      wanted = @options[:tasks]
      tasks = tasks.select { |task| wanted.include?(task.id) } if wanted
      tasks = tasks.select { |task| task.kind == @options[:kind] } if @options[:kind]
      tasks = tasks.select { |task| @options[:splits].include?(task.split) } if @options[:splits]
      raise '没有匹配的任务' if tasks.empty?

      tasks
    end

    def run_task(task, model)
      missing = task.missing_requirements
      return base_row(task).merge(status: 'skipped', missing: missing) if missing.any?

      slot = File.join(@run_root, model.gsub(/[^A-Za-z0-9._-]/, '_'), task.id)
      workspace = File.join(slot, 'workspace')
      home = File.join(slot, 'home')
      logs = File.join(slot, 'logs')
      [workspace, home, logs].each { |dir| FileUtils.mkdir_p(dir, mode: 0o700) }
      begin
        Verifier.seed(task, workspace)
      rescue Verifier::SeedError => e
        # 铺不起工作区是宿主的事，不是模型的事：记成 error，别把整轮带崩。
        @hints << e.message
        warn "[#{model}] #{e.message}"
        return base_row(task).merge(status: 'error', exit_code: nil)
      end
      before = Verifier.snapshot(workspace, task.editable)
      command = [@binary, '--config', @config, '--workspace', workspace, '--full-auto',
                 '--max-turns', @options[:turns].to_s, '--model', model]
      command += ['--profile', @options[:profile]] if @options[:profile]
      command += ['run', '--local', '--output', 'json', '--input', task.prompt_path]
      # 变体变量总是显式给出：没有变体时传 nil，让子进程里没有这个变量。
      # 只是不设的话，调用者 shell 里 export 过的候选会被 baseline 继承。
      env = { 'WILLDEEP_HOME' => home, 'WILLDEEP_PROMPT_VARIANT' => @options[:variant] }
      code, timed_out, elapsed = AgentEvalProcess.run(command, env, workspace,
                                                      @options[:timeout], logs)
      result = AgentEvalObservation.object(File.join(logs, 'stdout.log'))
      intact = before == Verifier.snapshot(workspace, task.editable)
      verdict = Verifier.evaluate(task, workspace)
      status = if timed_out then 'timeout'
               elsif INFRASTRUCTURE_EXITS.include?(code) then 'error'
               elsif verdict[:passed] && intact then 'passed'
               elsif verdict[:verifier_passed] && !intact then 'cheated'
               else 'failed'
               end
      if status == 'error'
        hint = Report.error_hint(read_log(File.join(logs, 'stderr.log')))
        @hints << hint
        warn format('[%s] %-30s 退出码 %s：%s', model, task.id, code.inspect, hint || '（stderr 为空）')
      end
      claimed = result.empty? ? nil : result['type'] == 'completed'
      tokens = AgentEvalObservation.checkpoint(home, result)
      row = base_row(task).merge(
        status: status, exit_code: code, stop_reason: result['stop_reason'], claimed: claimed,
        false_completion: claimed == true && status != 'passed',
        verifier_passed: verdict[:verifier_passed], protected_intact: intact,
        content_violations: verdict[:content_violations],
        mutants_caught: verdict[:mutants_caught], mutants_total: verdict[:mutants_total],
        elapsed_seconds: elapsed.round(1), input_tokens: tokens['input_tokens'], output_tokens: tokens['output_tokens'],
        # 实际生效的提示词变体：willdeep 在 JSON 结果里报告（null 即没有变体）；
        # 没拿到结果（超时、崩溃）时 reported 为 false。
        prompt_variant_reported: result.key?('prompt_variant'),
        prompt_variant_bundle: result['prompt_variant'].is_a?(Hash) ? result['prompt_variant']['bundle'] : nil
      ).merge(session_metrics(home, result['session_id']))
      warn format('[%s] %-30s %-8s %6.1fs', model, task.id, status, elapsed)
      row
    end

    # 先过一遍结构门：不合法的变体让每个任务都以退出码 2 收场，跑完二十个
    # 才发现只是浪费钱。返回写进报告的变体元数据。
    def check_variant(path)
      output = IO.popen([@binary, 'prompt', 'check', path], err: %i[child out], &:read).to_s.force_encoding('UTF-8')
      raise "提示词变体不合法：#{path}\n#{output}" unless $CHILD_STATUS&.success?

      spec = JSON.parse(File.read(path, encoding: 'UTF-8'))
      bundles = output[/^bundle (\S+) → (\S+)$/] && [Regexp.last_match(1), Regexp.last_match(2)]
      { 'id' => spec['id'], 'role' => spec['role'], 'section' => spec['section'],
        'sha256' => Digest::SHA256.file(path).hexdigest,
        'parent_bundle' => bundles&.first, 'candidate_bundle' => bundles&.last }
    end

    def read_log(path)
      File.file?(path) ? File.binread(path).force_encoding('UTF-8').scrub : ''
    end

    def base_row(task)
      { task: task.id, kind: task.kind, language: task.language, split: task.split, status: nil, missing: [] }
    end

    # 行为指标只认 `session_metrics.rb` 这一条算路，不在这里另算一遍。
    def session_metrics(home, session_id)
      return {} unless session_id.is_a?(String) && session_id.match?(UUID)

      path = File.join(home, 'sessions', "#{session_id}.json")
      return {} unless File.file?(path)

      out = File.join(home, 'metrics.json')
      ok = system(RbConfig.ruby, METRICS_SCRIPT, '--json', out, path, out: File::NULL, err: File::NULL)
      return {} unless ok && File.file?(out)

      row = JSON.parse(File.read(out, encoding: 'UTF-8')).dig('sessions', 0) || {}
      { session: row['session'], turns: row['turns'], narration_ratio: row['narration_ratio'],
        silent_tool_turns: row['silent_tool_turns'], tool_calls: row['tool_calls'],
        tool_failures: row['tool_failures'], reasoning_ratio: row['reasoning_ratio'] }
    rescue JSON::ParserError
      {}
    end

    def resolve_binary(name)
      path = name.include?(File::SEPARATOR) ? File.expand_path(name) : name
      raise "找不到可执行的 willdeep：#{name}（用 --binary 指路，或 cargo install）" unless Task.executable_in_path?(path)

      path
    end

    def binary_version(binary)
      IO.popen([binary, '--version'], err: File::NULL, &:read).to_s.split.last
    rescue SystemCallError
      nil
    end

    def workspace_version
      File.read(File.join(ROOT, 'Cargo.toml'), encoding: 'UTF-8')[/^version\s*=\s*"([^"]+)"/, 1]
    end

    def derive_config(source, run_root)
      raise "配置文件不存在：#{source}" unless File.file?(source)

      target = File.join(run_root, 'config.toml')
      File.open(target, 'w', 0o600) { |file| file.write(Config.derive(File.read(source, encoding: 'UTF-8'))) }
      target
    end
  end
end

if $PROGRAM_NAME == __FILE__
  options = {
    models: ENV.fetch('WILLDEEP_EVAL_MODELS', '').split(',').map(&:strip).reject(&:empty?),
    profile: nil,
    tasks: nil,
    kind: nil,
    splits: nil,
    variant: nil,
    binary: ENV.fetch('WILLDEEP_BIN', 'willdeep'),
    config: File.join(ENV.fetch('WILLDEEP_HOME', File.join(Dir.home, '.willdeep')), 'config.toml'),
    timeout: ModelEval::DEFAULT_TIMEOUT,
    turns: ModelEval::DEFAULT_MAX_TURNS,
    out: File.join(ModelEval::ROOT, 'target', 'model-eval'),
    bench: ModelEval::BENCH,
    archive: ModelEval::BENCH,
    keep: false,
    mode: :run
  }
  OptionParser.new do |parser|
    parser.banner = 'Usage: ruby scripts/model_eval.rb [options]'
    parser.on('--model NAME', '要评的模型，可重复；缺省读 WILLDEEP_EVAL_MODELS') { |v| options[:models] << v }
    parser.on('--profile NAME', 'willdeep 的 provider profile') { |v| options[:profile] = v }
    parser.on('--tasks LIST', '只跑这些任务，逗号分隔') { |v| options[:tasks] = v.split(',').map(&:strip) }
    parser.on('--split LIST', "只跑这些分组，逗号分隔（#{ModelEval::SPLITS.join(' / ')}）") do |v|
      options[:splits] = v.split(',').map(&:strip)
      unknown = options[:splits] - ModelEval::SPLITS
      raise "未知分组：#{unknown.join(', ')}" unless unknown.empty?
    end
    parser.on('--variant PATH', '候选提示词变体（WILLDEEP_PROMPT_VARIANT）；带变体的一轮不进模型趋势归档') do |v|
      options[:variant] = File.expand_path(v)
    end
    parser.on('--kind KIND', "只跑这一类任务（#{ModelEval::KINDS.join(' / ')}）") { |v| options[:kind] = v }
    parser.on('--binary PATH', 'willdeep 二进制，缺省 PATH 里的 willdeep') { |v| options[:binary] = v }
    parser.on('--config PATH', '用户配置文件，缺省 ~/.willdeep/config.toml') { |v| options[:config] = File.expand_path(v) }
    parser.on('--timeout SECONDS', Integer, "每个任务的墙钟上限，缺省 #{ModelEval::DEFAULT_TIMEOUT}") { |v| options[:timeout] = v }
    parser.on('--max-turns N', Integer, "每个任务的模型调用上限，缺省 #{ModelEval::DEFAULT_MAX_TURNS}") { |v| options[:turns] = v }
    parser.on('--out DIR', '最新报告输出目录，缺省 target/model-eval') { |v| options[:out] = File.expand_path(v) }
    parser.on('--archive DIR', '归档目录，缺省 bench/model-eval') { |v| options[:archive] = File.expand_path(v) }
    parser.on('--no-archive', '只跑不归档（调试用）') { options[:archive] = nil }
    parser.on('--keep', '保留临时运行目录（会话与日志）供排查') { options[:keep] = true }
    parser.on('--check-tasks', '不联网：自检每个任务 fixture 红、solution 绿') { options[:mode] = :check }
    parser.on('--list', '列出任务') { options[:mode] = :list }
  end.parse!
  raise 'timeout 与 max-turns 必须为正' unless options[:timeout].positive? && options[:turns].positive?
  if options[:variant] && options[:archive]
    # `history.jsonl` 追的是「同一份提示词下模型的趋势」，候选提示词的成绩混进去
    # 会让趋势线莫名其妙地跳。对照结论归 `bench/prompt-rsi/`。
    warn '带 --variant 的一轮不归档进 bench/model-eval（对照评测用 scripts/prompt_rsi_eval.rb）。'
    options[:archive] = nil
  end

  driver = ModelEval::Driver.new(options)
  exit(case options[:mode]
       when :list then driver.list
       when :check then driver.check_tasks
       else driver.run
       end)
end
