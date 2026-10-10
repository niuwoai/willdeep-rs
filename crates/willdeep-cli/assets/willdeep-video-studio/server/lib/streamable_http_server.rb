# frozen_string_literal: true

require "json"
require "fileutils"
require "securerandom"
require "socket"
require "timeout"

# A small, dependency-free Streamable HTTP transport for the already-running
# Video Studio MCP process. The acceptor never executes a tool: it queues the
# JSON-RPC request for one worker thread (`pop_request`), so HTTP calls run one
# at a time and never block the stdio loop that answers the host. HostBridge
# stays the only reader of stdin and routes reverse-request responses by id.
class StreamableHTTPServer
  MAX_CONCURRENT_CLIENTS = 32
  REQUEST_READ_TIMEOUT_SECONDS = 15
  RESPONSE_WAIT_TIMEOUT_SECONDS = 900

  Request = Struct.new(:method, :path, :headers, :body, :response, keyword_init: true)

  attr_reader :port, :token

  # metadata_path 给了就在 start 时写连接文件；不给则等调用方知道该写到哪里再
  # 调 publish（插件要等 stdio 宿主的 initialize 才知道自己被谁拉起）。
  def initialize(host: "127.0.0.1", port: 0, token: nil, metadata_path: nil)
    @host = host
    @requested_port = Integer(port)
    @token = token.to_s.empty? ? SecureRandom.hex(32) : token.to_s
    @metadata_path = metadata_path
    @metadata_extra = {}
    @queue = Queue.new
    @mutex = Mutex.new
    @sessions = {}
    @client_slots = Queue.new
    MAX_CONCURRENT_CLIENTS.times { @client_slots << true }
  end

  def start
    @server = TCPServer.new(@host, @requested_port)
    @port = @server.addr[1]
    write_metadata
    @thread = Thread.new { accept_loop }
    self
  end

  # 把连接信息写到 path（extra 是附加字段，如 host / pid）。换了位置时先删掉
  # 旧位置上仍属于自己的那份，免得留下一份指向本进程、却没人该读的文件。
  def publish(path, extra = {})
    @mutex.synchronize do
      delete_own_metadata if @metadata_path && @metadata_path != path
      @metadata_path = path
      @metadata_extra = extra
      write_metadata
    end
  end

  def url
    "http://#{@host}:#{@port}/mcp"
  end

  # Blocks until the next request; nil once the server is closed.
  def pop_request
    @queue.pop
  end

  def complete(request, response, session_id: nil)
    register_session(session_id) if session_id
    request.response << response
  end

  def close
    @server&.close
    @queue << nil
    @thread&.kill
    delete_own_metadata
  end

  private

  def accept_loop
    loop do
      socket = @server.accept
      @client_slots.pop
      Thread.new(socket) { |client| serve(client) }
    rescue IOError, Errno::EBADF
      break
    rescue StandardError => error
      warn "video-studio: streamable HTTP accept failed: #{error.class}: #{error.message}"
    end
  end

  def serve(socket)
    request = Timeout.timeout(REQUEST_READ_TIMEOUT_SECONDS) { read_request(socket) }
    return write_response(socket, 400, { "error" => "Invalid HTTP request." }) unless request
    return write_response(socket, 404, { "error" => "Not found." }) unless request.path == "/mcp"
    return write_response(socket, 401, { "error" => "Unauthorized." }) unless authorized?(request)

    if request.method == "GET"
      return write_response(socket, 405, { "error" => "Use POST for MCP JSON-RPC." }, { "Allow" => "POST" })
    end
    return write_response(socket, 405, { "error" => "Use POST for MCP JSON-RPC." }, { "Allow" => "POST" }) unless request.method == "POST"

    session_id = request.headers["mcp-session-id"]
    if session_id && !session_known?(session_id)
      return write_response(socket, 404, { "error" => "Unknown MCP session." })
    end

    request.response = Queue.new
    @queue << request
    response = Timeout.timeout(RESPONSE_WAIT_TIMEOUT_SECONDS) { request.response.pop }
    write_response(socket, response.fetch(:status), response.fetch(:body), response.fetch(:headers, {}))
  rescue Timeout::Error
    write_response(socket, 504, { "error" => "Video Studio request timed out." }) rescue nil
  rescue StandardError => error
    warn "video-studio: streamable HTTP request failed: #{error.class}: #{error.message}"
    write_response(socket, 500, { "error" => "Video Studio internal error." }) rescue nil
  ensure
    socket.close rescue nil
    @client_slots << true
  end

  def read_request(socket)
    headers = {}
    request_line = socket.gets("\r\n")
    return nil unless request_line
    method, path, version = request_line.strip.split(" ", 3)
    return nil unless version == "HTTP/1.1"
    while (line = socket.gets("\r\n"))
      line = line.strip
      break if line.empty?
      key, value = line.split(":", 2)
      headers[key.to_s.downcase] = value.to_s.strip
    end
    length = Integer(headers.fetch("content-length", "0"))
    return nil if length.negative? || length > 10 * 1024 * 1024
    body = length.zero? ? "" : socket.read(length)
    Request.new(method: method, path: path, headers: headers, body: body)
  rescue ArgumentError
    nil
  end

  def authorized?(request)
    request.headers["authorization"] == "Bearer #{@token}"
  end

  def session_known?(session_id)
    @mutex.synchronize { @sessions.key?(session_id) }
  end

  def register_session(session_id)
    @mutex.synchronize { @sessions[session_id] = true }
  end

  def write_response(socket, status, body, extra_headers = {})
    payload = JSON.generate(body)
    headers = {
      "Content-Type" => "application/json",
      "Content-Length" => payload.bytesize.to_s,
      "Connection" => "close"
    }.merge(extra_headers)
    socket.write("HTTP/1.1 #{status} #{status_text(status)}\r\n")
    headers.each { |key, value| socket.write("#{key}: #{value}\r\n") }
    socket.write("\r\n#{payload}")
  end

  def status_text(status)
    { 200 => "OK", 202 => "Accepted", 400 => "Bad Request", 401 => "Unauthorized", 404 => "Not Found", 405 => "Method Not Allowed", 500 => "Internal Server Error", 504 => "Gateway Timeout" }.fetch(status, "Error")
  end

  # 宿主超时结束旧进程时，新进程往往已经拉起并写好了自己的连接文件；旧进程退出
  # 时只删还是自己写的那份，否则新进程开着端口却没人知道怎么连。
  def own_metadata?
    return false unless @metadata_path && File.file?(@metadata_path)

    JSON.parse(File.read(@metadata_path))["token"] == @token
  rescue JSON::ParserError
    false
  end

  def delete_own_metadata
    File.delete(@metadata_path) if own_metadata?
  rescue SystemCallError
    nil
  end

  # 先写同目录临时文件（建时即 0600）再 rename：网关随时可能来读，不能读到半份，
  # 也不能有一瞬间按 umask 对别人可读。
  def write_metadata
    return unless @metadata_path

    FileUtils.mkdir_p(File.dirname(@metadata_path))
    temporary = "#{@metadata_path}.#{Process.pid}.tmp"
    File.open(temporary, File::WRONLY | File::CREAT | File::TRUNC, 0o600) do |file|
      file.write(JSON.pretty_generate({ url: url, token: @token }.merge(@metadata_extra)))
    end
    File.chmod(0o600, temporary)
    File.rename(temporary, @metadata_path)
  ensure
    File.delete(temporary) if temporary && File.exist?(temporary)
  end
end
