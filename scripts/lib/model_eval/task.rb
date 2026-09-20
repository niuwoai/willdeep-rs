# frozen_string_literal: true

require 'json'
require 'find'

module ModelEval
  KINDS = %w[fix feature test lint].freeze

  # 一个任务就是一个目录：
  #
  #   task.json     说明书：改哪些文件、怎么验收、依赖哪些可执行文件
  #   prompt.md     给模型的话，原样喂给 `willdeep run --input`
  #   fixture/      模型看到的起始工作区
  #   solution/     参考答案，只放 editable 里的文件；自检用，模型看不到
  #   mutants/<n>/  变异实现，只放 editable 之外的文件；kind=test 的任务用它证明
  #                 模型加的测试真能抓住错——不然把测试文件写空也能「通过」
  #
  # 说明书里写死的几条约束在这里就校验掉：错的任务集比没有任务集更糟，它会
  # 一夜一夜地产出看着像成绩的数字。
  class Task
    attr_reader :dir, :id, :title, :kind, :language, :requires, :editable, :verify,
                :must_contain, :must_not_contain, :mutants

    def self.load_all(root)
      Dir[File.join(root, 'tasks', '*', 'task.json')].sort.map { |path| load(File.dirname(path)) }
    end

    def self.load(dir)
      spec = JSON.parse(File.read(File.join(dir, 'task.json'), encoding: 'UTF-8'))
      new(dir, spec)
    end

    def initialize(dir, spec)
      @dir = dir
      @id = spec.fetch('id')
      raise ArgumentError, "#{dir}: id `#{@id}` 与目录名不一致" unless @id == File.basename(dir)

      @title = spec.fetch('title')
      @kind = spec.fetch('kind')
      raise ArgumentError, "#{@id}: kind 只能是 #{KINDS.join(' / ')}" unless KINDS.include?(@kind)

      @language = spec.fetch('language')
      @requires = Array(spec['requires'])
      @editable = Array(spec.fetch('editable'))
      raise ArgumentError, "#{@id}: editable 不能为空" if @editable.empty?

      @verify = spec.fetch('verify')
      unless @verify.is_a?(Array) && !@verify.empty? &&
             @verify.all? { |command| command.is_a?(Array) && !command.empty? && command.all?(String) }
        raise ArgumentError, "#{@id}: verify 必须是非空的命令数组（每条命令是字符串数组）"
      end

      @must_contain = spec['must_contain'] || {}
      @must_not_contain = spec['must_not_contain'] || {}
      @mutants = Array(spec['mutants'])
      raise ArgumentError, "#{@id}: kind=test 必须带 mutants，否则测不出测试有没有用" if @kind == 'test' && @mutants.empty?

      validate_layout
    end

    def prompt_path
      File.join(dir, 'prompt.md')
    end

    def fixture_dir
      File.join(dir, 'fixture')
    end

    def solution_dir
      File.join(dir, 'solution')
    end

    def mutant_dirs
      mutants.map { |relative| File.join(dir, relative) }
    end

    # 缺哪些可执行文件。缺了任务跳过，不算失败：「没跑」和「没过」是两件事。
    def missing_requirements
      requires.reject { |name| self.class.executable_in_path?(name) }
    end

    def self.executable_in_path?(name)
      return File.executable?(name) if name.include?(File::SEPARATOR)

      ENV.fetch('PATH', '').split(File::PATH_SEPARATOR).any? do |directory|
        candidate = File.join(directory, name)
        File.file?(candidate) && File.executable?(candidate)
      end
    end

    private

    def validate_layout
      [prompt_path, fixture_dir, solution_dir].each do |path|
        raise ArgumentError, "#{id}: 缺 #{File.basename(path)}" unless File.exist?(path)
      end
      # 参考答案只许碰 editable：答案改了受保护文件，自检会因为覆盖时被丢掉而
      # 变红，但那种红看不出原因。这里直接点名。
      stray = relative_files(solution_dir) - editable
      raise ArgumentError, "#{id}: solution/ 里有 editable 之外的文件：#{stray.join(', ')}" unless stray.empty?

      mutant_dirs.each do |mutant|
        raise ArgumentError, "#{id}: 缺变异目录 #{mutant}" unless File.directory?(mutant)

        overlap = relative_files(mutant) & editable
        raise ArgumentError, "#{id}: 变异不能改 editable 文件（#{overlap.join(', ')}），那是模型的地盘" unless overlap.empty?
      end
    end

    def relative_files(root)
      files = []
      Find.find(root) do |path|
        files << path.delete_prefix(root + File::SEPARATOR) if File.file?(path)
      end
      files
    end
  end
end
