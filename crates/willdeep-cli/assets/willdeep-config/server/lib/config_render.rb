require "toml_lite"
require "config_schema"

# 把 ConfigSchema.template 生成的中文注释模板与用户 overrides 浅合并。
#
# 静态节的键在模板里是注释占位行（`# max_turns =   # 未设置`），直接就地替换成真实行，
# 这样用户既能保住注释结构，又能得到一份可用的配置。集合节在模板里只有注释占位
# （模板没法猜实例名），所以统一在文本末尾追加真实表头。
module ConfigRender
  class << self
    def merge(template, overrides)
      lines = template.to_s.split("\n", -1)
      lines.pop while !lines.empty? && lines.last == ""

      collections = []
      overrides.each do |raw_section, kv|
        next if kv.nil?
        section = raw_section.to_s.strip
        next if kv.is_a?(Hash) && kv.empty?
        if ConfigSchema.split_collection(section)
          collections << [section, kv]
        else
          patch_static!(lines, section, kv)
        end
      end

      collections.sort_by { |section, _| section }.each do |section, kv|
        lines << "" unless lines.empty? || lines.last == ""
        lines << "[#{section}]"
        kv.each { |key, value| lines << key_line(key.to_s, value) }
      end

      lines << "" if lines.empty? || lines.last != ""
      lines.join("\n")
    end

    def key_line(key, value)
      TomlLite.dump_key(key) + " = " + TomlLite.dump_value(value)
    end

    private

    def patch_static!(lines, section, kv)
      span = section_span(lines, section)
      kv.each do |raw_key, value|
        key = raw_key.to_s
        content = key_line(key, value)
        found = nil
        idx = span[0]
        while idx <= span[1] && idx < lines.length
          if lines[idx] =~ /\A#?[ \t]*#{Regexp.escape(key)}[ \t]*=/
            found = idx
            break
          end
          idx += 1
        end
        if found
          lines[found] = content
        else
          at = [insert_at(lines, span, section), lines.length].min
          lines.insert(at, content)
          span = [span[0], span[1] + 1]
        end
      end
    end

    def header?(line)
      line =~ /\A\[[^\]]+\][ \t]*\z/ ? true : false
    end

    # 节内容所在的行区间（含端点）。根节（section 为 ""）是所有表头之前的顶层键。
    def section_span(lines, section)
      if section.empty?
        stop = lines.length - 1
        lines.each_with_index do |line, i|
          if header?(line)
            stop = i - 1
            break
          end
        end
        return [0, stop]
      end
      start = lines.index { |line| header?(line) && line.strip == "[#{section}]" }
      raise ArgumentError, "模板里没有节 #{section}" if start.nil?
      stop = lines.length - 1
      lines.each_with_index do |line, i|
        next unless i > start
        if header?(line)
          stop = i - 1
          break
        end
      end
      [start + 1, stop]
    end

    # 新键插到节头之后；根节插到开头注释块之后。
    def insert_at(lines, span, section)
      return span[0] unless section.empty?
      idx = span[0]
      while idx <= span[1] && idx < lines.length
        text = lines[idx]
        break unless text.strip.empty? || text.lstrip.start_with?("#")
        idx += 1
      end
      idx
    end
  end
end
