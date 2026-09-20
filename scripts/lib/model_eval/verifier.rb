# frozen_string_literal: true

require 'digest'
require 'fileutils'
require 'find'
require 'tmpdir'

require_relative '../agent_eval_process'

module ModelEval
  # 验收在模型碰不到的干净目录里做：原始 fixture + 模型改过的 editable 文件。
  # 改测试、改 Cargo 别名、加 build script、删测试文件，全都进不了这个目录，
  # 所以绿不了。这和 `agent_reliability_eval.rb` 的 external_verify 是同一条纪律。
  module Verifier
    # 快照与覆盖时都跳过的东西：构建产物、依赖缓存、宿主自己的状态目录。
    # Cargo.lock 也在内——`cargo test` 一跑它就出现，把它算成「改了受保护文件」
    # 等于每个 Rust 任务都被判作弊。
    ARTIFACTS = %w[.git target node_modules __pycache__ .willdeep Cargo.lock].freeze
    VERIFY_SECONDS = 120
    VERIFY_ENV = {
      'CARGO_TERM_COLOR' => 'never',
      'NO_COLOR' => '1',
      'PYTHONDONTWRITEBYTECODE' => '1',
      'NODE_NO_WARNINGS' => '1'
    }.freeze

    module_function

    # 把 fixture 铺成一个正常的 git 仓库：模型的 diff / 检查点工具都默认有仓库。
    def seed(task, workspace)
      copy_tree(task.fixture_dir, workspace)
      git = ->(*args) { system('git', '-C', workspace, *args, out: File::NULL, err: File::NULL) }
      ready = git.call('init', '--quiet') && git.call('add', '.') &&
              git.call('-c', 'user.name=Model Eval', '-c', 'user.email=model-eval@invalid',
                       'commit', '--quiet', '-m', 'fixture')
      raise "#{task.id}: 工作区 git 初始化失败" unless ready

      workspace
    end

    # 逐文件覆盖式拷贝。不用 `cp_r`：目标目录已存在时它会把源目录拷进去变成
    # `lib/lib/slug.rb`，解答和变异就全落空了——自检第一轮 15 个任务全红就是它。
    def copy_tree(source, target)
      FileUtils.mkdir_p(target)
      Dir.children(source).each do |name|
        from = File.join(source, name)
        to = File.join(target, name)
        if File.directory?(from) && !File.symlink?(from)
          copy_tree(from, to)
        else
          FileUtils.rm_rf(to)
          FileUtils.cp(from, to, preserve: true)
        end
      end
    end

    # 受保护文件的指纹：路径 => [sha256, 权限]。editable 与构建产物不在内。
    # 跑前跑后各拍一张，不一样就是模型动了不该动的东西——包括新加文件。
    def snapshot(root, editable)
      files = {}
      Find.find(root) do |path|
        next if path == root

        Find.prune if ARTIFACTS.include?(File.basename(path))
        stat = File.lstat(path)
        next if stat.directory?

        relative = path.delete_prefix(root + File::SEPARATOR)
        next if editable.include?(relative)

        files[relative] = stat.symlink? ? 'symlink' : [Digest::SHA256.file(path).hexdigest, stat.mode & 0o777]
      end
      files
    end

    # 返回一份判定：verifier 是否通过、内容规则是否满足、变异抓住几个、总的过没过。
    #
    # 每个验收目录都从 fixture 重新铺、各自编译，**不共用也不拷贝**构建产物：
    # 变异文件带着仓库里的旧 mtime，一旦目录里已有一份更新的编译缓存，cargo 就
    # 认为源码没变、复用旧二进制，变异「活」了——自检时它就是这么骗过去的。
    # 多花几秒编译，换判定不撒谎。
    def evaluate(task, workspace, seconds: VERIFY_SECONDS)
      Dir.mktmpdir("model-eval-verify-#{task.id}-") do |root|
        clean = File.join(root, 'clean')
        copy_tree(task.fixture_dir, clean)
        overlay_editable(task, workspace, clean)
        violations = content_violations(task, clean)
        verifier_passed = run_verify(task, clean, seconds, root)
        caught = 0
        if verifier_passed && violations.empty?
          task.mutant_dirs.each_with_index do |mutant, index|
            mutated = File.join(root, "mutant-#{index}")
            copy_tree(task.fixture_dir, mutated)
            overlay_editable(task, workspace, mutated)
            copy_tree(mutant, mutated)
            caught += 1 unless run_verify(task, mutated, seconds, root)
          end
        end
        {
          verifier_passed: verifier_passed,
          content_violations: violations,
          mutants_caught: caught,
          mutants_total: task.mutants.size,
          passed: verifier_passed && violations.empty? && caught == task.mutants.size
        }
      end
    end

    # 自检：fixture 原样必须不过（红），fixture + solution 必须过（绿）。
    # 绿着的靶子测不出任何东西；过不了的靶子会把每个模型都判死。
    def self_check(task, **options)
      red = Dir.mktmpdir("model-eval-red-#{task.id}-") do |workspace|
        copy_tree(task.fixture_dir, workspace)
        evaluate(task, workspace, **options)
      end
      green = Dir.mktmpdir("model-eval-green-#{task.id}-") do |workspace|
        copy_tree(task.fixture_dir, workspace)
        copy_tree(task.solution_dir, workspace)
        evaluate(task, workspace, **options)
      end
      { id: task.id, red: !red[:passed], green: green[:passed], ok: !red[:passed] && green[:passed],
        red_detail: red, green_detail: green }
    end

    def overlay_editable(task, workspace, target)
      task.editable.each do |relative|
        source = File.join(workspace, relative)
        destination = File.join(target, relative)
        # 符号链接不跟：模型把 editable 链到工作区外的文件，验收目录里也不该出现那份内容。
        if File.file?(source) && !File.symlink?(source)
          FileUtils.mkdir_p(File.dirname(destination))
          FileUtils.cp(source, destination)
        else
          FileUtils.rm_f(destination)
        end
      end
    end

    def content_violations(task, root)
      violations = []
      task.must_contain.each do |relative, needles|
        text = read_text(File.join(root, relative))
        Array(needles).each { |needle| violations << "#{relative} 缺少 `#{needle}`" unless text.include?(needle) }
      end
      task.must_not_contain.each do |relative, needles|
        text = read_text(File.join(root, relative))
        Array(needles).each { |needle| violations << "#{relative} 不该出现 `#{needle}`" if text.include?(needle) }
      end
      violations
    end

    def read_text(path)
      File.file?(path) ? File.read(path, encoding: 'UTF-8') : ''
    rescue ArgumentError, EncodingError
      ''
    end

    def run_verify(task, dir, seconds, log_root)
      # 构建产物留在验收目录自己的 target/ 里，随临时目录一起消失。
      env = VERIFY_ENV.merge('CARGO_TARGET_DIR' => File.join(dir, 'target'))
      task.verify.all? do |command|
        logs = Dir.mktmpdir('verify-log-', log_root)
        code, timed_out, = AgentEvalProcess.run(command, env, dir, seconds, logs)
        !timed_out && code == 0
      end
    end
  end
end
