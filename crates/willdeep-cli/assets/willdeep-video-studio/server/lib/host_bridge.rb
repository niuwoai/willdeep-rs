# frozen_string_literal: true

require "json"

# 插件 MCP 进程反向请求宿主（WillDeep 1.378.0-rc1 起）。
#
# 主 Agent 在聊天里调用本插件的工具时，走不到插件页面的 `window.willdeep`，
# 出图却必须用宿主的 some.im 凭据——插件不另配 key。宿主为此允许插件服务在
# 处理 `tools/call` 期间往 stdout 写一条带 id 的 JSON-RPC 请求，宿主处理完把
# 响应写回 stdin。
#
# 两条约定：
# - 宿主只在 initialize 的 `capabilities.extensions["io.willdeep/host-requests"]`
#   里宣告自己支持哪些方法。没宣告就当不支持，直接报错，不发出去干等——旧宿主
#   和 willdeep-rs 不会回这种请求，发了只会卡到工具超时。
# - 请求 id 用字符串：宿主自己的请求 id 是整数，混用同一种会撞号。
#
# stdin 只有一个读者：`start` 起的读线程。反向请求的响应按 id 交给等它的调用方，
# 其余行（宿主发来的请求与通知）进收件箱，由主循环 `next_line` 逐条取。这样
# 本机 HTTP 入口的调用可以在工作线程里向宿主发反向请求，主线程照常应答宿主——
# 宿主对每个请求只等 `startup_timeout_sec`，等不到就结束插件进程。
# stdout 同理只经 `write_line` 加锁写，两个线程的行不会交错。
class HostBridge
  CAPABILITY_KEY = "io.willdeep/host-requests"
  IMAGE_GENERATE = "willdeep/images/generate"
  # 宿主代管 TTS：宿主负责凭据、网络请求与媒体落盘，返回插件媒体目录中的音频引用。
  TTS_GENERATE = "willdeep/audio/synthesize"
  # 问一次模型（WillDeep 1.380.0-rc1 起），形状同页面桥 ai.complete，不流式。
  AI_COMPLETE = "willdeep/ai/complete"
  # 开一场专家圆桌（WillDeep 1.412.0-rc1 起）：插件带着自己的专家席，宿主跑开场、发言、小结、终稿，
  # 再按插件给的契约出一份结构化结论交回。专家席审稿（lib/review_panel.rb）在宿主支持时走这条。
  ROUNDTABLE_RUN = "willdeep/roundtable/run"

  class Unsupported < StandardError; end

  class RequestFailed < StandardError
    attr_reader :code

    def initialize(code, message)
      super(message)
      @code = code
    end
  end

  def initialize(input:, output:)
    @input = input
    @output = output
    @methods = []
    @sequence = 0
    @inbox = Queue.new
    @pending = {}
    @closed = false
    @state = Mutex.new
    @writer = Mutex.new
    @reader = nil
  end

  # 起读线程。幂等；主循环开始前调用。
  def start
    @state.synchronize { @reader ||= Thread.new { read_loop } }
    self
  end

  # 每次 initialize 都重算：宿主重连（比如升级后）宣告的能力可能变了。
  def record_initialize(params)
    extensions = params.is_a?(Hash) ? params.dig("capabilities", "extensions") : nil
    advertised = extensions.is_a?(Hash) ? extensions[CAPABILITY_KEY] : nil
    methods = advertised.is_a?(Hash) ? advertised["methods"] : nil
    @methods = Array(methods).map(&:to_s)
  end

  def supports?(method)
    @methods.include?(method)
  end

  # 主循环取宿主发来的下一行；stdin 关闭后返回 nil。
  def next_line
    start
    @inbox.pop
  end

  # 往宿主写一行 JSON-RPC。响应、反向请求都走这里。
  def write_line(text)
    @writer.synchronize do
      @output.write("#{text}\n")
      @output.flush
    end
  end

  # 本线程之后发出的反向请求最多等多少秒（后台任务用，0.36.0-rc1）。前台工具调用不设：
  # 宿主自己对单个反向请求有 10 分钟上限。后台任务没有调用方在等，宿主万一不回，
  # 工作线程不能永远卡在这里。
  TIMEOUT_KEY = :video_studio_host_request_timeout
  TIMED_OUT = -32_004

  def self.with_timeout(seconds)
    previous = Thread.current[TIMEOUT_KEY]
    Thread.current[TIMEOUT_KEY] = seconds
    yield
  ensure
    Thread.current[TIMEOUT_KEY] = previous
  end

  # 一次性的应答信箱：读线程放入，发起方取；取的时候可以限时（Ruby 2.6 的 Queue#pop 不能）。
  class Waiter
    def initialize
      @lock = Mutex.new
      @ready = ConditionVariable.new
      @filled = false
      @value = nil
    end

    def <<(value)
      @lock.synchronize do
        @value = value
        @filled = true
        @ready.broadcast
      end
      self
    end

    # 超时返回 :timeout。
    def pop(timeout = nil)
      deadline = timeout && Process.clock_gettime(Process::CLOCK_MONOTONIC) + timeout
      @lock.synchronize do
        until @filled
          if deadline
            remaining = deadline - Process.clock_gettime(Process::CLOCK_MONOTONIC)
            return :timeout if remaining <= 0

            @ready.wait(@lock, remaining)
          else
            @ready.wait(@lock)
          end
        end
        @value
      end
    end
  end

  def request(method, params)
    raise Unsupported, method unless supports?(method)

    start
    waiter = Waiter.new
    id = @state.synchronize do
      raise RequestFailed.new(-32_003, "Host closed the connection before answering #{method}.") if @closed

      @sequence += 1
      "video-studio-host-#{Process.pid}-#{@sequence}".tap { |key| @pending[key] = waiter }
    end
    write_line(JSON.generate(jsonrpc: "2.0", id: id, method: method, params: params))
    timeout = Thread.current[TIMEOUT_KEY]
    message = waiter.pop(timeout)
    if message == :timeout
      @state.synchronize { @pending.delete(id) }
      raise RequestFailed.new(TIMED_OUT, "Host did not answer #{method} within #{timeout.round} seconds.")
    end
    raise RequestFailed.new(-32_003, "Host closed the connection before answering #{method}.") if message.nil?

    error = message["error"]
    raise RequestFailed.new(error["code"].to_i, error["message"].to_s) if error.is_a?(Hash)

    message["result"]
  end

  private

  def read_loop
    while (line = @input.gets)
      message = begin
        JSON.parse(line)
      rescue JSON::ParserError
        nil
      end
      response = message.is_a?(Hash) && message["id"].is_a?(String) && !message.key?("method")
      waiter = response ? @state.synchronize { @pending.delete(message["id"]) } : nil
      if waiter
        waiter << message
      elsif response && message["id"].start_with?("video-studio-host-")
        # 发起方已经超时放弃的那条请求，宿主后来才回。不进收件箱：主循环会把它当成
        # 一条没有 method 的请求，再给宿主回一条莫名其妙的错误。
        warn "video-studio: dropped a late host response (#{message['id']})"
      else
        @inbox << line
      end
    end
  rescue IOError, SystemCallError => error
    warn "video-studio: host stdin closed (#{error.class})"
  ensure
    orphans = @state.synchronize do
      @closed = true
      @pending.values.tap { @pending.clear }
    end
    orphans.each { |waiter| waiter << nil }
    @inbox << nil
  end
end
