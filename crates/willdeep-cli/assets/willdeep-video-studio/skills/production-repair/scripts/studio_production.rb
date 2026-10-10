# frozen_string_literal: true

require "json"
require "net/http"
require "uri"
require "digest"
require "fileutils"
require "open3"
require "optparse"
require "time"

# Host-side controller. It never launches a second stdio plugin or edits registries.
module StudioProduction
  ID = "willdeep-video-studio"
  RUNTIME_PATHS = %w[.codex-plugin .willdeep-plugin schemas mcp.json server skills ui/dist README.md CHANGELOG.md].freeze
  class Refused < StandardError; end

  def self.fingerprint(root, paths = RUNTIME_PATHS)
    entries = paths.flat_map do |relative|
      path = File.join(root, relative)
      raise Refused, "missing runtime resource: #{relative}" unless File.exist?(path)
      File.directory?(path) ? Dir.glob(File.join(path, "**", "*"), File::FNM_DOTMATCH) : [path]
    end
    files = entries.select { |path| File.file?(path) }.sort
    raise Refused, "symbolic links are not allowed" if entries.any? { |path| File.symlink?(path) }
    Digest::SHA256.hexdigest(files.map { |path| "#{path.delete_prefix(root + '/')}\0#{Digest::SHA256.file(path).hexdigest}\n" }.join)
  end

  class Client
    def initialize(home)
      discovery = JSON.parse(File.read(File.join(home, "mcp-gateway.json")))
      raise Refused, "gateway belongs to another host" unless discovery["host"] == "willdeep-rs"
      entry = Array(discovery["servers"]).find { |item| item["pluginID"] == ID && item["server"] == "video-studio" }
      raise Refused, "short-drama gateway is unavailable" unless entry
      @uri = URI(entry.fetch("url"))
      raise Refused, "gateway must be loopback HTTP" unless @uri.scheme == "http" && @uri.host == "127.0.0.1" && @uri.userinfo.nil? && @uri.query.nil?
      raise Refused, "unexpected gateway route" unless @uri.path == "/plugins/#{ID}/video-studio/mcp"
      @token = discovery.fetch("token")
    end

    def rpc(method, params = {})
      request = Net::HTTP::Post.new(@uri)
      request["Authorization"] = "Bearer #{@token}"
      request["Content-Type"] = "application/json"
      request["Accept"] = "application/json, text/event-stream"
      request.body = JSON.generate("jsonrpc" => "2.0", "id" => 1, "method" => method, "params" => params)
      http = Net::HTTP.new(@uri.host, @uri.port, nil)
      http.open_timeout = 5
      http.read_timeout = 60
      response = http.request(request)
      raise Refused, "gateway HTTP #{response.code}" unless response.code == "200"
      body = JSON.parse(response.body)
      raise Refused, "gateway RPC failure #{body.dig('error', 'code')}" if body["error"]
      body.fetch("result")
    end

    def call(name, arguments = {})
      result = rpc("tools/call", "name" => name, "arguments" => arguments)
      text = Array(result["content"]).find { |item| item["type"] == "text" }
      value = text && JSON.parse(text.fetch("text"))
      raise Refused, "tool failed: #{value && value.dig('error', 'code')}" unless value.is_a?(Hash) && value["ok"] == true && !result["isError"]
      value
    end

    def idle!
      value = call("jobs.status", "state" => "active", "limit" => 1, "includeItems" => false)
      raise Refused, "background work is active or unknown" unless value["total"] == 0 && value["jobs"] == []
      # Submitted external work survives process exit and must be reconciled first.
      videos = call("video.list", "status" => "active")
      jobs = videos["jobs"]
      raise Refused, "video ledger did not report jobs" unless jobs.is_a?(Array)
      active = jobs.select { |job| %w[submitting queued in_progress].include?(job["state"]) }
      raise Refused, "external video work remains active" unless active.empty?
      true
    end
  end

  def self.protected_files(root)
    paths = Dir.glob(File.join(root, "scripts", "**", "*"), File::FNM_DOTMATCH).select { |path| File.file?(path) }
    paths += [File.join(root, "skills/production-repair/scripts/studio_production.rb")]
    paths.to_h { |path| [path.delete_prefix(root + "/"), Digest::SHA256.file(path).hexdigest] }
  end

  def self.check_candidate!(source, policy)
    raise Refused, "source differs from the authorized repository" unless File.realpath(source) == policy.fetch("source")
    policy.fetch("protected").each do |relative, hash|
      path = File.join(source, relative)
      raise Refused, "protected verifier changed: #{relative}" unless File.file?(path) && Digest::SHA256.file(path).hexdigest == hash
    end
    manifest = JSON.parse(File.read(File.join(source, ".codex-plugin/plugin.json")))
    raise Refused, "plugin identity changed" unless manifest["name"] == ID
    permissions = JSON.parse(File.read(File.join(source, ".willdeep-plugin/plugin.json"))).fetch("permissions").sort
    raise Refused, "plugin permissions changed" unless permissions == policy.fetch("permissions")
    manifest.fetch("version")
  end

  def self.private_json(path, value)
    FileUtils.mkdir_p(File.dirname(path))
    temporary = "#{path}.#{Process.pid}.tmp"
    File.open(temporary, File::WRONLY | File::CREAT | File::EXCL, 0o600) { |file| file.write(JSON.pretty_generate(value) + "\n") }
    File.rename(temporary, path)
  end

  def self.main(argv)
    options = { "home" => File.expand_path("~/.willdeep"), "binary" => "willdeep" }
    parser = OptionParser.new do |opt|
      opt.on("--home PATH") { |value| options["home"] = File.expand_path(value) }
      opt.on("--source PATH") { |value| options["source"] = File.realpath(value) }
      opt.on("--binary PATH") { |value| options["binary"] = value }
      opt.on("--drama ID") { |value| options["drama"] = value }
      opt.on("--tool NAME") { |value| options["tool"] = value }
      opt.on("--arguments JSON") { |value| options["arguments"] = JSON.parse(value) }
      opt.on("--report PATH") { |value| options["report"] = File.expand_path(value) }
    end
    command = argv.shift
    parser.parse!(argv)
    raise Refused, "unexpected arguments" unless argv.empty?
    directory = File.join(options["home"], "production-loop", ID)
    FileUtils.mkdir_p(directory)
    File.open(File.join(directory, "controller.lock"), File::RDWR | File::CREAT, 0o600) do |lock|
      raise Refused, "another production controller is active" unless lock.flock(File::LOCK_EX | File::LOCK_NB)
      policy_path = File.join(directory, "policy.json")
      case command
      when "authorize"
        raise Refused, "policy already exists; preserve its verifier baseline" if File.exist?(policy_path)
        source = options.fetch("source")
        value = { "schema" => "willdeep.production-policy.v1", "source" => source, "pluginID" => ID,
                  "dramaID" => options.fetch("drama"), "host" => "willdeep-rs", "maxRepairCandidates" => 2,
                  "protected" => protected_files(source), "permissions" => JSON.parse(File.read(File.join(source, ".willdeep-plugin/plugin.json"))).fetch("permissions").sort }
        private_json(policy_path, value)
        puts JSON.generate("ok" => true, "policy" => policy_path)
      when "inspect", "checkpoint", "call"
        client = Client.new(options["home"])
        value = if command == "call"
                  client.call(options.fetch("tool"), options.fetch("arguments", {}))
                elsif command == "inspect"
                  client.call("system.status")
                else
                  client.call("drama.get_progress", "dramaID" => options.fetch("drama"))
                end
        if command == "checkpoint"
          private_json(File.join(directory, "checkpoint.json"), { "at" => Time.now.utc.iso8601, "host" => "willdeep-rs", "progress" => value })
        end
        puts JSON.generate(value)
      when "verify"
        source = options.fetch("source")
        policy = JSON.parse(File.read(policy_path))
        version = check_candidate!(source, policy)
        report = options.fetch("report")
        FileUtils.mkdir_p(report)
        tests = policy.fetch("protected").keys.select { |path| path.match?(%r{\Ascripts/[^/]+_test\.rb\z}) }.sort
        commands = tests.map { |path| ["ruby", path] } + [["yarn", "--cwd", "ui", "test"], ["yarn", "--cwd", "ui", "build"]]
        results = commands.map.with_index do |args, index|
          stdout, stderr, status = Open3.capture3(*args, chdir: source)
          File.open(File.join(report, "check-#{index}.log"), "w", 0o600) { |file| file.write(stdout + stderr) }
          result = { "command" => args, "exitCode" => status.exitstatus }
          raise Refused, "verification failed: #{args.join(' ')}; see #{report}" unless status.success? && !stdout.match?(/\"status\"\s*:\s*\"skipped\"/)
          result
        end
        receipt = { "schema" => "willdeep.production-verification.v1", "version" => version, "source" => source,
                    "runtimeHash" => fingerprint(source), "policyHash" => Digest::SHA256.file(policy_path).hexdigest,
                    "checks" => results, "at" => Time.now.utc.iso8601 }
        private_json(File.join(report, "receipt.json"), receipt)
        File.write(File.join(report, "report.md"), "# 插件验证\n\n版本：#{version}\n\n#{results.length} 项检查通过，无跳过。\n\n包内容 SHA-256：#{receipt['runtimeHash']}\n")
        puts JSON.generate("ok" => true, "receipt" => File.join(report, "receipt.json"), "checks" => results.length)
      when "install"
        source = options.fetch("source")
        policy = JSON.parse(File.read(policy_path))
        version = check_candidate!(source, policy)
        receipt = JSON.parse(File.read(File.join(options.fetch("report"), "receipt.json")))
        raise Refused, "verification receipt is stale" unless receipt["source"] == source && receipt["version"] == version && receipt["runtimeHash"] == fingerprint(source) && receipt["policyHash"] == Digest::SHA256.file(policy_path).hexdigest
        Client.new(options["home"]).idle!
        stage = File.join(directory, "staging", version)
        raise Refused, "staging directory already exists" if File.exist?(stage)
        FileUtils.mkdir_p(stage)
        RUNTIME_PATHS.each { |relative| target = File.join(stage, relative); FileUtils.mkdir_p(File.dirname(target)); FileUtils.cp_r(File.join(source, relative), target) }
        raise Refused, "staged package differs from verified content" unless fingerprint(stage) == receipt["runtimeHash"]
        stdout, stderr, status = Open3.capture3({ "WILLDEEP_HOME" => options["home"] }, options["binary"], "plugin", "install", stage, "--enable")
        File.open(File.join(directory, "install.log"), "w", 0o600) { |file| file.write(stdout + stderr) }
        raise Refused, "official installer failed; see private install.log" unless status.success?
        private_json(File.join(directory, "installation.json"), receipt.merge("state" => "installed_pending_runtime_readback"))
        puts JSON.generate("ok" => true, "version" => version, "state" => "installed_pending_runtime_readback", "restartRequired" => true)
      when "readback"
        installation = JSON.parse(File.read(File.join(directory, "installation.json")))
        status = Client.new(options["home"]).call("system.status")
        expected_root = File.join(options["home"], "plugins", ID, installation.fetch("version"))
        raise Refused, "old plugin process is still running" unless status["version"] == installation["version"] && status["packageRoot"] == expected_root
        raise Refused, "installed content differs from verified package" unless fingerprint(expected_root) == installation["runtimeHash"]
        installation["state"] = "runtime_verified"
        installation["runtime"] = status
        private_json(File.join(directory, "installation.json"), installation)
        puts JSON.generate("ok" => true, "state" => installation["state"], "version" => installation["version"])
      else
        raise Refused, "use authorize, inspect, checkpoint, call, verify, install or readback"
      end
    end
  end
end

if $PROGRAM_NAME == __FILE__
  begin
    StudioProduction.main(ARGV)
  rescue StandardError => error
    warn JSON.generate("ok" => false, "error" => error.class.name, "message" => error.message)
    exit 1
  end
end
