#!/usr/bin/env ruby
# frozen_string_literal: true

# 候选提示词的对照评测（`docs/PROMPT_RSI_OPERATIONS.md`）。
#
# 同一模型、同一二进制、同一 commit 下，分别用 baseline 与候选变体
# （`willdeep prompt check` 过得了的 `prompt-variant.v1` 文件）跑一遍固定任务集，
# 按 `scripts/lib/prompt_rsi/gate.rb` 的门禁给出结论：
#
#   candidate_passes   可以由人把改动提成代码 PR（仍要过人工门）
#   rejected           validation 没提升够、regression 破了、成本或耗时超了……
#   overfit            validation 提升、holdout 下降
#   non_reproducible   出处不全、工作区脏、有任务没真正执行
#   needs_human_judging 输入建议套件：自动指标过了，plausible 还没人工判
#
# 它花真钱：model-eval 套件先跑 validation + regression 两遍（baseline、候选），
# 过了第一阶段才再跑 holdout 两遍。它从不改提示词、从不上线任何东西。
#
#   ruby scripts/prompt_rsi_eval.rb --model glm-5 --variant tone.json
#   ruby scripts/prompt_rsi_eval.rb --suite input-suggestion --model deepseek-v4-flash --variant suggest.json

require 'English'
require 'fileutils'
require 'digest'
require 'json'
require 'optparse'
require 'rbconfig'
require 'tmpdir'

require_relative 'lib/model_eval/report'
require_relative 'lib/model_eval/task'
require_relative 'lib/prompt_rsi/gate'
require_relative 'lib/prompt_rsi/report'
require_relative 'lib/prompt_rsi/preflight'
require_relative 'lib/suggestion_report'

module PromptRsiEval
  ROOT = File.expand_path('..', __dir__)
  TASKS = File.join(ROOT, 'bench', 'model-eval', 'tasks')
  SAMPLES = File.join(ROOT, 'bench', 'input-suggestion', 'samples')
  ARCHIVE = File.join(ROOT, 'bench', 'prompt-rsi')
  # 复评报告的 run_id 里带这个标记，一眼能和原始运行区分开。
  RESCORED_TAG = 'rescored'

  module_function

  # `willdeep prompt check` 的结论：不合法直接退出，合法返回新旧版本号。
  def check_variant(binary, path)
    output = IO.popen([binary, 'prompt', 'check', path], err: %i[child out], &:read).to_s.force_encoding('UTF-8')
    abort("提示词变体不合法：#{path}\n#{output}") unless $CHILD_STATUS.success?
    match = output.match(/^bundle (\S+) → (\S+)$/)
    abort("看不懂 `willdeep prompt check` 的输出：\n#{output}") unless match
    spec = JSON.parse(File.read(path, encoding: 'UTF-8'))
    # 构建 commit：旧二进制没有这一行，或者构建时不在 git 仓库里（`unknown`），
    # 都记为缺失，门禁据此判 non_reproducible。
    built = output[/^build (\S+)$/, 1]
    { 'variant_id' => spec['id'], 'role' => spec['role'], 'section' => spec['section'],
      'parent_bundle' => match[1], 'candidate_bundle' => match[2],
      'binary_commit' => built == 'unknown' ? nil : built,
      'variant_sha256' => Digest::SHA256.file(path).hexdigest }
  end

  def binary_version(binary)
    IO.popen([binary, '--version'], err: File::NULL, &:read).to_s.split.last
  rescue SystemCallError
    nil
  end

  # 跑一遍 model_eval.rb，返回报告行（跑不成返回空数组）。
  def model_eval(options, label, splits, variant)
    out = File.join(options[:work], label)
    command = [RbConfig.ruby, File.join(__dir__, 'model_eval.rb'), '--model', options[:model],
               '--split', splits.join(','), '--no-archive', '--out', out,
               '--binary', options[:binary], '--timeout', options[:timeout].to_s,
               '--max-turns', options[:turns].to_s]
    command += ['--jobs', options.fetch(:jobs, 1).to_s]
    if options[:checkpoint_dir]
      checkpoint = File.join(options[:checkpoint_dir], "#{label}.json")
      command += ['--checkpoint', checkpoint]
      command << '--resume' if options[:resume] && File.file?(checkpoint)
    end
    command += ['--config', options[:config]] if options[:config]
    command += ['--profile', options[:profile]] if options[:profile]
    command += ['--variant', variant] if variant
    warn "== #{label}: #{splits.join(' + ')}"
    system(*command)
    path = File.join(out, "#{options[:model].gsub(/[^A-Za-z0-9._-]/, '_')}.json")
    File.file?(path) ? JSON.parse(File.read(path, encoding: 'UTF-8'))['rows'] : []
  end

  # 任务清单（任务 id → 分组与内容哈希）：门禁拿它逐题核对报告，不从报告本身推断
  # 该有哪些题；内容哈希用来核对每一轮实际跑的是不是同一份题。
  def task_manifest
    ModelEval::Task.load_all(File.dirname(TASKS)).to_h do |task|
      [task.id, { 'split' => task.split, 'sha256' => task.content_sha256 }]
    end
  end

  def run_model_eval(options, provenance)
    provenance['dataset_sha256'] = PromptRsi::Report.dataset_sha256(TASKS)
    tasks = task_manifest
    first = PromptRsi::Gate::STAGE_ONE_SPLITS
    baseline = model_eval(options, 'baseline', first, nil)
    candidate = model_eval(options, 'candidate', first, options[:variant])
    passed_one, = PromptRsi::Gate.stage_one(baseline, candidate)
    reproducible = PromptRsi::Gate.reproducibility(provenance, baseline, candidate, tasks: tasks).empty?
    holdout = nil
    if passed_one && reproducible
      splits = PromptRsi::Gate::HOLDOUT_SPLITS
      holdout = { baseline: model_eval(options, 'holdout-baseline', splits, nil),
                  candidate: model_eval(options, 'holdout-candidate', splits, options[:variant]) }
    end
    result = PromptRsi::Gate.evaluate(provenance: provenance, baseline: baseline, candidate: candidate,
                                      tasks: tasks, holdout: holdout)
    PromptRsi::Report.build(suite: 'model-eval', provenance: provenance, result: result,
                            baseline_rows: baseline, candidate_rows: candidate)
  end

  # 跑一遍输入建议实弹，返回原始报告（跑不成返回 nil）。
  def suggestion_run(options, label, variant)
    out = File.join(options[:work], label)
    command = [RbConfig.ruby, File.join(__dir__, 'input_suggestion_eval.rb'), '--model', options[:model],
               '--no-history', '--out', out, '--jobs', options.fetch(:jobs, 1).to_s]
    command += ['--config', options[:config]] if options[:config]
    command += ['--variant', variant] if variant
    warn "== #{label}"
    system(*command)
    path = File.join(out, 'report.json')
    File.file?(path) ? JSON.parse(File.read(path, encoding: 'UTF-8')) : nil
  end

  # 样本清单（样本 id → 文件原始字节的 sha256），开跑前读出。实弹测试读样本时
  # 对同样的字节算哈希写进每条结果，门禁逐条核对两轮跑的是不是同一份样本。
  def sample_manifest(dir = SAMPLES)
    Dir[File.join(dir, '*.json')].sort.to_h do |path|
      [JSON.parse(File.read(path, encoding: 'UTF-8'))['id'], Digest::SHA256.hexdigest(File.binread(path))]
    end
  end

  # 摘要另带每条样本实际跑的内容哈希（只给门禁用，不进输入建议套件自己的 history）。
  def summarize_suggestion(raw, provenance, ran_at: nil)
    return { 'errors' => 1 } unless raw

    summary = SuggestionReport.summarize(raw, commit: provenance['commit'], dirty: provenance['dirty'],
                                              version: provenance['version'], ran_at: ran_at)
    summary.merge('sample_sha256' => raw['cases'].to_a.to_h { |row| [row['id'], row['sample_sha256']] })
  end

  def judge_suggestion(provenance, raws, samples:, ran_at: {}, tag: nil)
    summaries = raws.to_h { |side, raw| [side, summarize_suggestion(raw, provenance, ran_at: ran_at[side])] }
    result = PromptRsi::Gate.suggestion(provenance: provenance, baseline: summaries['baseline'],
                                        candidate: summaries['candidate'], samples: samples)
    result['stats'] = summaries
    report = PromptRsi::Report.build(suite: 'input-suggestion', provenance: provenance, result: result, tag: tag)
    # 复评时用开跑前的这份清单，不重读样本目录：要比的是当时跑的那份。
    report.merge('sample_manifest' => samples)
  end

  # 返回 [报告, 双方原始报告]；双方原始报告随后存进归档，供人工判定与 `--rescore`。
  def run_suggestion(options, provenance)
    provenance['dataset_sha256'] = PromptRsi::Report.dataset_sha256(SAMPLES)
    samples = sample_manifest
    raws = { 'baseline' => suggestion_run(options, 'baseline', nil),
             'candidate' => suggestion_run(options, 'candidate', options[:variant]) }
    [judge_suggestion(provenance, raws, samples: samples), raws]
  end

  # 人工在归档样本里填完 `judged` 之后重算：只读归档，不请求模型、不重跑二进制。
  # 结论另存一份带 `rescored_from` 的报告，原报告不动。
  def rescore(options)
    path = File.expand_path(options[:rescore])
    original = JSON.parse(File.read(path, encoding: 'UTF-8'))
    abort('--rescore 只适用于 input-suggestion 套件的报告') unless original['suite'] == 'input-suggestion'
    abort('这份报告没有保存双方样本（evidence），无法复评') unless original['evidence']
    # 报告在 `<归档>/reports/<日期>/` 下，样本路径相对归档根目录。
    archive = File.expand_path('../../..', path)
    raws = PromptRsi::Report.load_suggestion_evidence(archive, original['evidence'])
    changed = raws.reject { |side, raw| PromptRsi::Report.evidence_digest(raw) == original.dig('evidence_sha256', side) }
    unless changed.empty?
      abort("样本除 judged 以外被改过（或报告没有指纹）：#{changed.keys.join('、')}。只能填 judged，不能改模型输出与样本。")
    end
    ran_at = original['stats'].to_h.transform_values { |summary| summary['ran_at'] }
    report = judge_suggestion(original['provenance'], raws, samples: original['sample_manifest'].to_h,
                                                            ran_at: ran_at, tag: RESCORED_TAG)
    report['evidence'] = original['evidence']
    report['evidence_sha256'] = original['evidence_sha256']
    report['rescored_from'] = path.delete_prefix("#{archive}/")
    saved = PromptRsi::Report.archive(report, archive, history: options[:history])
    puts PromptRsi::Report.markdown(report)
    puts "报告：#{saved}"
    report['verdict'] == 'candidate_passes' ? 0 : 1
  end

  def main(options)
    return rescore(options) if options[:rescore]

    variant = check_variant(options[:binary], options[:variant])
    context = PromptRsi::Preflight.check!(root: ROOT, binary_version: binary_version(options[:binary]),
                                          binary_commit: variant['binary_commit'])
    provenance = variant.merge('model' => options[:model], 'commit' => context[:commit], 'dirty' => context[:dirty],
                               'binary_version' => binary_version(options[:binary]), 'parallel_jobs' => options.fetch(:jobs, 1))
    missing = ModelEval::Task.load_all(File.dirname(TASKS)).flat_map(&:missing_requirements).uniq if options[:suite] == 'model-eval'
    abort("RSI preflight：缺评测依赖 #{missing.join(', ')}") if missing&.any?
    if options[:preflight]
      puts JSON.pretty_generate(provenance.merge('preflight' => 'passed', 'paid_requests' => 0))
      return 0
    end
    options[:work] = Dir.mktmpdir('prompt-rsi-')
    if options[:suite] == 'input-suggestion'
      report, raws = run_suggestion(options, provenance)
      if raws.values.all?
        report['evidence'] = PromptRsi::Report.save_suggestion_evidence(options[:archive], report,
                                                                        baseline: raws['baseline'],
                                                                        candidate: raws['candidate'])
        report['evidence_sha256'] = raws.transform_values { |raw| PromptRsi::Report.evidence_digest(raw) }
      end
    else
      report = run_model_eval(options, provenance)
    end
    path = PromptRsi::Report.archive(report, options[:archive], history: options[:history])
    puts PromptRsi::Report.markdown(report)
    puts "报告：#{path}"
    report['verdict'] == 'candidate_passes' ? 0 : 1
  ensure
    FileUtils.remove_entry(options[:work]) if options[:work] && !options[:keep]
  end
end

if $PROGRAM_NAME == __FILE__
  options = {
    suite: 'model-eval', model: nil, variant: nil, profile: nil, config: nil,
    binary: ENV.fetch('WILLDEEP_BIN', 'willdeep'), timeout: 300, turns: 24, jobs: 1,
    archive: PromptRsiEval::ARCHIVE, history: true, keep: false
  }
  OptionParser.new do |parser|
    parser.banner = 'Usage: ruby scripts/prompt_rsi_eval.rb --model MODEL --variant PATH [options]'
    parser.on('--suite NAME', 'model-eval（缺省）或 input-suggestion') { |v| options[:suite] = v }
    parser.on('--model NAME', '评测用的模型（baseline 与候选同一个）') { |v| options[:model] = v }
    parser.on('--variant PATH', '候选提示词变体（prompt-variant.v1）') { |v| options[:variant] = File.expand_path(v) }
    parser.on('--binary PATH', 'willdeep 二进制，缺省 PATH 里的 willdeep') { |v| options[:binary] = v }
    parser.on('--config PATH', '用户配置文件') { |v| options[:config] = File.expand_path(v) }
    parser.on('--profile NAME', 'willdeep 的 provider profile') { |v| options[:profile] = v }
    parser.on('--timeout SECONDS', Integer, '每个任务的墙钟上限') { |v| options[:timeout] = v }
    parser.on('--jobs N', Integer, '模型评测并发数 1..4；默认 1') { |v| options[:jobs] = v }
    parser.on('--checkpoint-dir DIR', '保存 baseline/candidate 各阶段的逐题断点') { |v| options[:checkpoint_dir] = File.expand_path(v) }
    parser.on('--resume', '从相同出处的逐题断点继续') { options[:resume] = true }
    parser.on('--preflight', '只检查源码、二进制、任务和变体，不请求 Provider') { options[:preflight] = true }
    parser.on('--max-turns N', Integer, '每个任务的模型调用上限') { |v| options[:turns] = v }
    parser.on('--archive DIR', '归档目录，缺省 bench/prompt-rsi') { |v| options[:archive] = File.expand_path(v) }
    parser.on('--no-history', '不向 history.jsonl 追加') { options[:history] = false }
    parser.on('--keep', '保留临时运行目录') { options[:keep] = true }
    parser.on('--rescore REPORT', 'input-suggestion 报告：人工填完 judged 后重算，不请求模型') do |v|
      options[:rescore] = v
    end
  end.parse!
  abort('需要 --model 与 --variant（或 --rescore）') unless options[:rescore] || (options[:model] && options[:variant])
  abort('jobs 必须为 1..4') unless (1..4).cover?(options[:jobs])
  abort('--resume 需要 --checkpoint-dir') if options[:resume] && !options[:checkpoint_dir]
  abort('输入建议套件暂不支持断点；请用模型评测套件') if options[:suite] == 'input-suggestion' && options[:checkpoint_dir]
  abort('--suite 只能是 model-eval 或 input-suggestion') unless %w[model-eval input-suggestion].include?(options[:suite])

  exit(PromptRsiEval.main(options))
end
