# frozen_string_literal: true
require_relative 'agent_eval_observation'

module AgentEvalRecovery
  MARKER_PATH = 'progress.log'
  MARKER = "checkpoint-once\n"
  COMMAND = %q{ruby -e 'File.open("progress.log", "a") { |file| file.write("checkpoint-once\n") }'}.freeze
  UUID = /\A[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}\z/i

  def self.session(home, id)
    return {} unless id.is_a?(String) && id.match?(UUID)
    value = AgentEvalObservation.object(File.join(home, 'sessions', "#{id}.json"))
    value['id'] == id ? value : {}
  end

  def self.marker_call(messages)
    return nil unless messages.is_a?(Array) && messages.all? { |message| message.is_a?(Hash) }
    calls = messages.flat_map { |message| Array(message['tool_calls']) }.select { |call| call.is_a?(Hash) }
    writers = calls.select do |call|
      next false unless %w[run_command create_file edit_file].include?(call['name'])
      call['arguments'].is_a?(String) && call['arguments'].include?(MARKER_PATH)
    end
    return nil unless writers.size == 1 && writers.first['name'] == 'run_command'
    call = writers.first
    args = JSON.parse(call['arguments'])
    return nil unless args.is_a?(Hash) && args['command'] == COMMAND && call['id'].is_a?(String) && !call['id'].empty?
    call_index = messages.index { |message| Array(message['tool_calls']).include?(call) }
    result = messages.drop(call_index + 1).any? { |message| message['role'] == 'tool' && message['tool_call_id'] == call['id'] }
    result ? call['id'] : nil
  rescue JSON::ParserError
    nil
  end

  def self.boundary(home, workspace, id: nil)
    return nil unless AgentEvalObservation.contents(File.join(workspace, MARKER_PATH)) == MARKER
    if id.nil?
      candidates = Dir.glob(File.join(home, 'sessions', '*.json'))
      return nil unless candidates.size == 1
      id = File.basename(candidates.first, '.json')
    end
    value = session(home, id)
    checkpoint = value['execution_checkpoint']
    return nil unless checkpoint.is_a?(Hash) && checkpoint['status'] == 'running' && checkpoint['pending_call_ids'] == []
    call_id = marker_call(value['messages'])
    call_id ? {session_id: id, marker_call_id: call_id} : nil
  end

  def self.recovered?(home, workspace, result, interrupted)
    return false unless interrupted && result['session_id'] == interrupted[:session_id]
    value = session(home, interrupted[:session_id])
    checkpoint = value['execution_checkpoint']
    return false unless checkpoint.is_a?(Hash)
    AgentEvalObservation.contents(File.join(workspace, MARKER_PATH)) == MARKER &&
      checkpoint['status'] == 'completed' &&
      marker_call(value['messages']) == interrupted[:marker_call_id]
  end
end
