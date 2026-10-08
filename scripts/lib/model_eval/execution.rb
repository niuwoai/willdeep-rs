# frozen_string_literal: true

require 'digest'
require 'json'
require 'fileutils'
require 'thread'

module ModelEval
  # 每题独立运行；结果始终按任务清单排序，不按线程完成顺序排序。
  module Execution
    MAX_JOBS = 4
    module_function

    def run(items, jobs:, &work)
      raise "jobs 必须为 1..#{MAX_JOBS}" unless (1..MAX_JOBS).cover?(jobs)

      queue = Queue.new
      items.each_with_index { |item, index| queue << [item, index] }
      results = Array.new(items.size)
      workers = [jobs, items.size].min.times.map do
        Thread.new do
          loop do
            pair = begin
              queue.pop(true)
            rescue ThreadError
              break
            end
            item, index = pair
            results[index] = work.call(item)
          end
        end
      end
      # value 会传播线程异常；调用方不会把部分结果误归档为完整运行。
      workers.each(&:value)
      results
    ensure
      workers&.each { |worker| worker.kill if worker.alive? }
      workers&.each(&:join)
    end
  end

  # 不保存配置正文。锁覆盖整个运行；原子替换避免中断留下半条 JSON。
  class Checkpoint
    SCHEMA = 'willdeep.model-eval-checkpoint.v1'
    REUSABLE = %w[passed failed cheated timeout].freeze

    def initialize(path, manifest, resume: false)
      @path = File.expand_path(path)
      FileUtils.mkdir_p(File.dirname(@path), mode: 0o700)
      @lock = File.open("#{@path}.lock", File::RDWR | File::CREAT, 0o600)
      raise '另一个评测正在使用这个 checkpoint' unless @lock.flock(File::LOCK_EX | File::LOCK_NB)

      @mutex = Mutex.new
      @state = { 'schema' => SCHEMA, 'manifest' => manifest, 'rows' => {} }
      if resume
        prior = JSON.parse(File.read(@path))
        raise 'checkpoint 出处不同，不能复用；请使用新路径' unless prior['schema'] == SCHEMA && prior['manifest'] == manifest
        raise 'checkpoint 结果格式不合法' unless prior['rows'].is_a?(Hash)

        @state = prior
      elsif File.exist?(@path)
        raise 'checkpoint 已存在；请用 --resume 或换路径'
      end
    rescue StandardError
      close
      raise
    end

    def fetch(model, task)
      row = @state['rows'][key(model, task.id)]
      return nil unless row && REUSABLE.include?(row['status'])
      raise 'checkpoint 任务内容不匹配' unless row['task'] == task.id && row['task_sha256'] == task.content_sha256

      row.transform_keys(&:to_sym)
    end

    def store(model, row)
      @mutex.synchronize do
        @state['rows'][key(model, row[:task])] = row
        temporary = "#{@path}.#{Process.pid}.tmp"
        File.open(temporary, 'w', 0o600) { |file| file.write(JSON.generate(@state)); file.flush; file.fsync }
        File.rename(temporary, @path)
      ensure
        FileUtils.rm_f(temporary) if temporary
      end
    end

    def close
      @lock&.close unless @lock&.closed?
    end

    private

    def key(model, task)
      JSON.generate([model, task])
    end
  end
end
