# frozen_string_literal: true
require 'json'
require 'rbconfig'

# The parent never reaps the supervisor until its process group is killed.
# The command's exit is reported through a private pipe while the group leader
# remains alive, so neither normal cleanup nor timeout targets a reused PID.
module AgentEvalProcess
  MAX_LOG_BYTES = 16 * 1024 * 1024
  MAX_INPUT_BYTES = 4096
  MAX_STATUS_BYTES = 1024

  def self.supervise(command)
    status_pipe = IO.for_fd(3)
    owner = IO.for_fd(4)
    Thread.new do
      owner.read
      Process.kill('KILL', -Process.pid)
    end
    begin
      child = Process.spawn([command.fetch(0), command.fetch(0)], *command.drop(1), close_others: true)
      _, status = Process.wait2(child)
      status_pipe.puts(JSON.generate({exit_code: status.exitstatus}))
    rescue SystemCallError
      status_pipe.puts(JSON.generate({exit_code: 127}))
    end
    status_pipe.flush
    sleep
  end

  def self.drain(stream, path)
    Thread.new do
      File.open(path, 'wb', 0o600) do |file|
        kept = 0
        loop do
          chunk = stream.readpartial(8192)
          keep = [chunk.bytesize, MAX_LOG_BYTES - kept].min
          file.write(chunk.byteslice(0, keep)) if keep.positive?
          kept += keep
        end
      rescue EOFError
        nil
      rescue IOError
        raise unless stream.closed?
      end
    end
  end

  def self.run(command, env, root, seconds, log_root, stdin_data: nil, interrupt_when: nil)
    raise 'process timeout must be positive' unless seconds.positive?
    raise 'process input exceeds private bridge limit' if stdin_data && stdin_data.bytesize > MAX_INPUT_BYTES
    started = Process.clock_gettime(Process::CLOCK_MONOTONIC)
    pipes = Array.new(5) { IO.pipe }
    input, output, errors, completion, owner = pipes
    readers = []
    pid = nil
    code = nil
    timed_out = false
    interrupted = false
    begin
      pid = Process.spawn(env, RbConfig.ruby, __FILE__, *command, chdir: root, pgroup: true,
                          in: input[0], out: output[1], err: errors[1], 3 => completion[1], 4 => owner[0], close_others: true)
      [input[0], output[1], errors[1], completion[1], owner[0]].each(&:close)
      readers = [drain(output[0], File.join(log_root, 'stdout.log')), drain(errors[0], File.join(log_root, 'stderr.log'))]
      begin
        input[1].write(stdin_data) if stdin_data
      rescue Errno::EPIPE
        # The command may reject its arguments before consuming bridge input.
      ensure
        input[1].close
      end
      status_bytes = +''
      loop do
        remaining = seconds - (Process.clock_gettime(Process::CLOCK_MONOTONIC) - started)
        if remaining <= 0
          timed_out = true
          break
        end
        if interrupt_when && interrupt_when.call
          interrupted = true
          break
        end
        next unless IO.select([completion[0]], nil, nil, [remaining, 0.05].min)
        chunk = completion[0].read_nonblock(MAX_STATUS_BYTES, exception: false)
        break if chunk.nil?
        next if chunk == :wait_readable
        status_bytes << chunk
        raise 'invalid supervisor status length' if status_bytes.bytesize > MAX_STATUS_BYTES
        next unless status_bytes.end_with?("\n")
        code = JSON.parse(status_bytes).fetch('exit_code')
        break
      end
    ensure
      if pid
        begin
          Process.kill('KILL', -pid)
        rescue Errno::ESRCH
          # A crashed supervisor remains unreaped, preserving the PID identity.
        end
        Process.wait2(pid)
      end
      [output[0], errors[0]].zip(readers).each do |stream, reader|
        next unless reader
        unless reader.join(2)
          timed_out = true
          stream.close unless stream.closed?
        end
      end
      pipes.flatten.each { |io| io.close unless io.closed? }
      readers.each(&:value)
    end
    [code, timed_out, Process.clock_gettime(Process::CLOCK_MONOTONIC) - started, interrupted]
  end
end

AgentEvalProcess.supervise(ARGV) if $PROGRAM_NAME == __FILE__
