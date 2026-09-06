# frozen_string_literal: true
require 'json'
require 'digest'
require 'find'

module AgentEvalObservation
  MAX_BYTES = 16 * 1024 * 1024
  READ_ONLY_PROFILE = 'reviewer'

  def self.contents(path, limit = MAX_BYTES)
    return nil unless File.lstat(path).file?
    File.open(path, 'rb') do |file|
      value = file.read(limit + 1) || ''.b
      value.bytesize <= limit ? value : nil
    end
  rescue SystemCallError, IOError
    nil
  end

  def self.object(path)
    value = contents(path)
    return {} unless value
    parsed = JSON.parse(value)
    parsed.is_a?(Hash) ? parsed : {}
  rescue JSON::ParserError, EncodingError
    {}
  end

  def self.checkpoint(home, result)
    id = result['session_id']
    return {} unless id.is_a?(String) && id.match?(/\A[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}\z/i)
    session = object(File.join(home, 'sessions', "#{id}.json"))
    return {} unless session['id'] == id && session['execution_checkpoint'].is_a?(Hash)
    session['execution_checkpoint'].slice('input_tokens', 'output_tokens').transform_values do |value|
      value.is_a?(Integer) && value >= 0 ? value : nil
    end
  end

  def self.cli_output(path, ndjson: false)
    return [object(path), []] unless ndjson
    raw = contents(path)
    return [{}, []] unless raw
    events = raw.lines.reject { |line| line.strip.empty? }.map { |line| JSON.parse(line) }
    return [{}, []] unless events.all? { |event| event.is_a?(Hash) }
    final = events.last
    return [{}, []] unless final && %w[completed partial error].include?(final['type'])
    [final, events]
  rescue JSON::ParserError, EncodingError
    [{}, []]
  end

  def self.completed_read_only_child?(events)
    events.each_with_index.any? do |event, index|
      next false unless event['type'] == 'subagent_started' && event['profile'] == READ_ONLY_PROFILE && event['background'] == false
      id = event['id']
      next false unless id.is_a?(String) && id.match?(/\A[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}\z/i)
      terminal = events.drop(index + 1).find { |following| following['type'] == 'subagent_completed' && following['id'] == id }
      terminal && terminal['status'] == 'completed'
    end
  end

  # Compare actual directory entries, including untracked/ignored additions.
  # Git config and model-edited ignore rules cannot hide a changed test runner.
  def self.fixture_files(root, mutable: [])
    files = {}
    Find.find(root) do |path|
      next if path == root
      relative = path.delete_prefix(root + File::SEPARATOR)
      Find.prune if %w[.git target].include?(relative)
      stat = File.lstat(path)
      if stat.symlink?
        files[relative] = 'symlink'
      elsif stat.directory?
        files[relative + '/'] = 'directory'
      elsif !mutable.include?(relative)
        content = contents(path)
        files[relative] = content ? [Digest::SHA256.hexdigest(content), stat.mode & 0o777] : 'unreadable'
      end
    end
    files
  rescue SystemCallError, IOError
    nil
  end
end
