# frozen_string_literal: true
require 'json'
require 'digest'
require 'English'

module RuntimeParameters
  KEYS = %w[schema max_turns token_budget goal_token_budget goal_wall_clock_minutes
            goal_max_continuations input_suggestions small_model_routing auto_dispatch_read_only
            max_deep_calls_per_harness].freeze
  RANGES = { 'max_turns' => 1..1000, 'token_budget' => 1000..10_000_000,
             'goal_token_budget' => 1000..10_000_000, 'goal_wall_clock_minutes' => 1..10_080,
             'goal_max_continuations' => 1..10_000, 'max_deep_calls_per_harness' => 0..16 }.freeze
  module_function

  def canonical(value)
    raise 'invalid runtime parameters' unless value.is_a?(Hash) && value.keys.sort == KEYS.sort &&
      value['schema'] == 'willdeep.runtime-parameters.v1'
    RANGES.each do |key, range|
      item = value[key]
      next if %w[token_budget goal_token_budget].include?(key) && item.nil?
      raise 'invalid runtime parameters' unless item.is_a?(Integer) && range.cover?(item)
    end
    %w[input_suggestions small_model_routing auto_dispatch_read_only].each do |key|
      raise 'invalid runtime parameters' unless [true, false].include?(value[key])
    end
    JSON.generate(KEYS.to_h { |key| [key, value[key]] })
  end

  def fingerprint(value)
    Digest::SHA256.hexdigest(canonical(value))
  end

  def parse(text)
    raise 'invalid runtime parameters' unless text.bytesize <= 65_536 &&
      text.scan(/"(?:[^"\\]|\\.)*"\s*:/).size == KEYS.size
    value = JSON.parse(text)
    canonical(value)
    value
  end

  def query(binary, config: nil, turns:)
    command = [binary]
    command += ['--config', config] if config
    command += ['config', 'runtime', '--max-turns', turns.to_s]
    output = IO.popen(command, err: File::NULL, &:read)
    raise 'cannot resolve effective runtime parameters' unless $CHILD_STATUS&.success?
    parse(output)
  end
end
