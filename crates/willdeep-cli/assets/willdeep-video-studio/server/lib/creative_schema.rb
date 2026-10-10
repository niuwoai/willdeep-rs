# frozen_string_literal: true

require "json"

# The same versioned JSON Schema definitions are consumed by the UI and service.
# This validator implements only the vocabulary used by that checked-in catalog.
module CreativeSchema
  CATALOG = JSON.parse(File.read(File.expand_path("../../schemas/creative-v1.json", __dir__))).freeze

  def self.valid?(kind, value)
    errors(CATALOG.fetch("$defs").fetch(kind), value).empty?
  end

  def self.errors(schema, value, path = "$")
    case schema.fetch("type")
    when "object"
      return [path] unless value.is_a?(Hash)
      result = value.size < schema.fetch("minProperties", 0) ? [path] : []
      Array(schema["required"]).each { |key| result << "#{path}.#{key}" unless value.key?(key) }
      value.each do |key, entry|
        child = schema.fetch("properties", {})[key]
        if child
          result.concat(errors(child, entry, "#{path}.#{key}"))
        elsif schema["additionalProperties"] == false
          result << "#{path}.#{key}"
        end
      end
      result
    when "array"
      return [path] unless value.is_a?(Array)
      result = value.size > schema.fetch("maxItems", Float::INFINITY) ? [path] : []
      value.each_with_index { |entry, index| result.concat(errors(schema["items"], entry, "#{path}[#{index}]")) } if schema["items"]
      result
    when "string"
      return [path] unless value.is_a?(String)
      return [path] if schema.key?("enum") && !schema["enum"].include?(value)
      value.length.between?(schema.fetch("minLength", 0), schema.fetch("maxLength", Float::INFINITY)) ? [] : [path]
    when "number", "integer"
      return [path] unless value.is_a?(Numeric) && value.finite?
      return [path] if schema["type"] == "integer" && value != value.to_i
      return [path] if value < schema.fetch("minimum", -Float::INFINITY) || value > schema.fetch("maximum", Float::INFINITY)
      return [path] if schema.key?("exclusiveMinimum") && value <= schema["exclusiveMinimum"]
      []
    else
      raise ArgumentError, "Unsupported schema type: #{schema['type']}"
    end
  end
end
