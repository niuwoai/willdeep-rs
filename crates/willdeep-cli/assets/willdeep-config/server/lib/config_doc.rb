require "toml_lite"

# 文档模型：只改动的行会被重写，其余行按原字节保留。
class ConfigDoc
  class ParseError < StandardError; end
  class RenderError < StandardError; end

  CONTEXT_LINES = 3

  attr_reader :path

  class << self
    def load(path)
      new(path, File.binread(path))
    end

    def load_if_exists(path)
      return nil unless File.file?(path)
      load(path)
    end

    # 返回 [行数组（不含行尾符）, 行尾风格, 末尾是否有换行]
    def split_text(raw)
      bin = raw.dup.force_encoding(Encoding::BINARY)
      nl = bin.index("\n")
      eol = (nl.nil? || nl.zero? || bin.getbyte(nl - 1) != 13) ? "\n" : "\r\n"
      segs = bin.split("\n", -1)
      trailing = false
      if segs.last == ""
        segs.pop
        trailing = true
      end
      lines = segs.map do |s|
        s = s.dup.force_encoding(Encoding::UTF_8)
        s = s[0...-1] if s.end_with?("\r")
        s
      end
      [lines, eol, trailing]
    end

    def canon_section(section)
      return "" if section.nil?
      raise ArgumentError, "节名必须是字符串（得到 #{section.class}）" unless section.is_a?(String)
      s = section.strip
      return "" if s.empty?
      TomlLite.format_section(TomlLite.split_section_name(s))
    end

    def validate_value!(value)
      case value
      when String
        raise ArgumentError, "字符串不是合法的 UTF-8 字节序列" unless value.dup.force_encoding(Encoding::UTF_8).valid_encoding?
      when Integer, Float, TrueClass, FalseClass
        nil
      when Array
        value.each { |v| validate_scalar!(v) }
      else
        raise ArgumentError, "不支持的值类型：#{value.class}"
      end
      nil
    end

    def value_eql?(a, b)
      return true if a.is_a?(Float) && b.is_a?(Float) && a.nan? && b.nan?
      a.eql?(b)
    end

    # 按点号路径取值；路径上遇到数组表时取最后一个元素
    def lookup(root, parts)
      cur = root
      parts.each do |p|
        cur = cur.last if cur.is_a?(Array) && !cur.empty?
        return [false, nil] unless cur.is_a?(Hash)
        return [false, nil] unless cur.key?(p)
        cur = cur[p]
      end
      [true, cur]
    end

    def unified_diff(a_lines, b_lines, context = CONTEXT_LINES)
      ops = line_ops(a_lines, b_lines)
      changed = []
      ops.each_with_index { |op, i| changed << i unless op[0] == :eq }
      return "" if changed.empty?

      groups = []
      cur = [changed.first, changed.first]
      changed.drop(1).each do |i|
        if i - cur[1] <= 2 * context + 1
          cur[1] = i
        else
          groups << cur
          cur = [i, i]
        end
      end
      groups << cur

      out = ["--- a/config.toml", "+++ b/config.toml"]
      groups.each do |range|
        lo = [range[0] - context, 0].max
        hi = [range[1] + context, ops.length - 1].min
        old_before = 0
        new_before = 0
        ops.each_with_index do |op, i|
          next if i >= lo
          old_before += 1 unless op[0] == :add
          new_before += 1 unless op[0] == :del
        end
        old_count = 0
        new_count = 0
        body = []
        ops[lo..hi].each do |op|
          case op[0]
          when :eq
            old_count += 1
            new_count += 1
            body << " " + op[1]
          when :del
            old_count += 1
            body << "-" + op[1]
          when :add
            new_count += 1
            body << "+" + op[1]
          end
        end
        out << "@@ -#{old_before + 1},#{old_count} +#{new_before + 1},#{new_count} @@"
        out.concat(body)
      end
      out.join("\n") + "\n"
    end

    def line_ops(a, b)
      n = a.length
      m = b.length
      dp = Array.new(n + 1) { Array.new(m + 1, 0) }
      (n - 1).downto(0) do |i|
        (m - 1).downto(0) do |j|
          dp[i][j] = if a[i] == b[j]
                       dp[i + 1][j + 1] + 1
                     else
                       l = dp[i + 1][j]
                       r = dp[i][j + 1]
                       l >= r ? l : r
                     end
        end
      end
      ops = []
      i = 0
      j = 0
      while i < n && j < m
        if a[i] == b[j]
          ops << [:eq, a[i]]
          i += 1
          j += 1
        elsif dp[i + 1][j] >= dp[i][j + 1]
          ops << [:del, a[i]]
          i += 1
        else
          ops << [:add, b[j]]
          j += 1
        end
      end
      while i < n
        ops << [:del, a[i]]
        i += 1
      end
      while j < m
        ops << [:add, b[j]]
        j += 1
      end
      ops
    end

    private

    def validate_scalar!(value)
      case value
      when String
        raise ArgumentError, "字符串不是合法的 UTF-8 字节序列" unless value.dup.force_encoding(Encoding::UTF_8).valid_encoding?
      when Integer, Float, TrueClass, FalseClass
        nil
      else
        raise ArgumentError, "数组元素只支持字符串/数字/布尔（得到 #{value.class}），且不支持嵌套数组"
      end
    end
  end

  def initialize(path, raw)
    @path = path
    @raw = raw.dup.force_encoding(Encoding::BINARY)
    @text = raw.dup.force_encoding(Encoding::UTF_8)
    @ops = []
    analyze
  end

  def get(section, key)
    ensure_parsed!
    sec = ConfigDoc.canon_section(section)
    ks = TomlLite.validate_key_string(key)
    op = pending_op(sec, ks)
    return op[:op] == :set ? op[:value] : nil if op
    block = @base_values[sec]
    block ? block[ks] : nil
  end

  def key?(section, key)
    ensure_parsed!
    sec = ConfigDoc.canon_section(section)
    ks = TomlLite.validate_key_string(key)
    op = pending_op(sec, ks)
    return op[:op] == :set if op
    block = @base_values[sec]
    !!(block && block.key?(ks))
  end

  def sections
    ensure_parsed!
    seen = {}
    out = []
    @header_names.each do |name|
      next if seen[name]
      seen[name] = true
      out << name
    end
    out
  end

  def set(section, key, value)
    sec = ConfigDoc.canon_section(section)
    ks = TomlLite.validate_key_string(key)
    return unset(section, ks) if value.nil?
    ConfigDoc.validate_value!(value)
    @ops.reject! { |o| o[:section] == sec && o[:key] == ks }
    block = @base_values[sec]
    current = block ? block[ks] : nil
    @ops << { op: :set, section: sec, key: ks, value: value } unless ConfigDoc.value_eql?(current, value)
    value
  end

  def unset(section, key)
    sec = ConfigDoc.canon_section(section)
    ks = TomlLite.validate_key_string(key)
    @ops.reject! { |o| o[:section] == sec && o[:key] == ks }
    block = @base_values[sec]
    @ops << { op: :unset, section: sec, key: ks } if block && block.key?(ks)
    nil
  end

  # 删除整个节：表头与节内所有键。只删到本节最后一个键为止，节尾的注释与空行
  # 保留——它们通常是写给下一节的引子。节不存在时什么都不做。
  def remove_section(section)
    ensure_parsed!
    sec = ConfigDoc.canon_section(section)
    raise ArgumentError, "不能删除顶层键所在的根节" if sec.empty?
    @ops.reject! { |o| o[:section] == sec }
    @ops << { op: :remove_section, section: sec } if section_present?(sec)
    nil
  end

  def section_present?(section)
    ensure_parsed!
    sec = ConfigDoc.canon_section(section)
    @header_names.include?(sec) || @base_values.key?(sec)
  end

  def dirty?
    !@ops.empty?
  end

  def render
    ensure_parsed!
    return @text.dup if @ops.empty?
    blocks = build_blocks
    @ops.each { |op| apply_op(blocks, op) }
    begin
      lines = serialize(blocks)
    rescue ArgumentError => e
      raise RenderError, "渲染失败：#{e.message}"
    end
    out = lines.join(@eol)
    out += @eol if @trailing_newline || blocks.any? { |b| b[:new] }
    self_check(out)
    out
  end

  def diff
    ensure_parsed!
    rendered = render
    b_lines = ConfigDoc.split_text(rendered)[0]
    return "" if @lines == b_lines
    ConfigDoc.unified_diff(@lines, b_lines)
  end

  private

  def analyze
    @lines, @eol, @trailing_newline = ConfigDoc.split_text(@raw)
    @header_names = []
    @entries = []
    @base_values = {}
    @doc = nil
    @parse_error = nil
    if @text.valid_encoding?
      begin
        @doc = TomlLite.parse_document(@text)
      rescue TomlLite::ParseError => e
        @parse_error = e
      end
    else
      @parse_error = TomlLite::ParseError.new("文件不是合法的 UTF-8 字节序列")
    end
    return unless @doc
    @header_names = @doc[:headers].map { |h| h.name }
    @entries = @doc[:entries]
    @entries.each do |e|
      (@base_values[e.section] ||= {})[e.key] = e.value
    end
  end

  def ensure_parsed!
    return if @doc
    raise ParseError, "无法解析 #{@path}：#{@parse_error.message}"
  end

  # 整节删除对节内每个键都等价于一次 unset；之后对同一键的 set 仍然生效。
  def pending_op(section, key)
    op = nil
    @ops.each do |o|
      next unless o[:section] == section
      if o[:op] == :remove_section
        op = { op: :unset, section: section, key: key }
      elsif o[:key] == key
        op = o
      end
    end
    op
  end

  # 把原文切成块：前导块 + 每个表头一块，块内是按键行/原样行
  def build_blocks
    blocks = []
    entry_at = {}
    @entries.each { |e| entry_at[e.line] = e }
    header_at = {}
    @doc[:headers].each { |h| header_at[h.line] = h }
    current = { name: "", header_lines: [], items: [] }
    blocks << current
    i = 0
    while i < @lines.length
      h = header_at[i]
      if h
        current = { name: h.name, header_lines: [@lines[i]], items: [] }
        blocks << current
        i += 1
        next
      end
      e = entry_at[i]
      if e
        current[:items] << { kind: :key, key: e.key, entry: e, lines: @lines[i..e.end_line] }
        i = e.end_line + 1
        next
      end
      current[:items] << { kind: :raw, text: @lines[i] }
      i += 1
    end
    blocks
  end

  def apply_op(blocks, op)
    return remove_block(blocks, op[:section]) if op[:op] == :remove_section
    block = nil
    blocks.each { |b| block = b if b[:name] == op[:section] }
    if op[:op] == :unset
      return unless block
      block[:items].reject! { |it| it[:kind] == :key && it[:key] == op[:key] }
      return
    end
    unless block
      block = { name: op[:section], header_lines: ["[#{op[:section]}]"], items: [], new: true }
      blocks << block
    end
    item = nil
    block[:items].each { |it| item = it if it[:kind] == :key && it[:key] == op[:key] }
    if item
      if item[:entry]
        item[:lines] = replacement_lines(item[:entry], op[:value])
      else
        item[:lines] = [key_line(op[:key], op[:value], item[:indent])]
      end
    else
      indent = section_indent(block)
      block[:items].insert(insert_index(block),
                           { kind: :key, key: op[:key], entry: nil, indent: indent,
                             lines: [key_line(op[:key], op[:value], indent)] })
    end
  end

  # 块留在原位但失去名字与表头：之后的 set 不会误把键写进一个没有表头的块。
  def remove_block(blocks, section)
    child = blocks.find { |b| b[:name] && b[:name].start_with?(section + ".") }
    raise RenderError, "节 [#{section}] 下还有子表 [#{child[:name]}]，拒绝整节删除" if child
    blocks.each do |b|
      next unless b[:name] == section
      last_key = nil
      b[:items].each_with_index { |it, i| last_key = i if it[:kind] == :key }
      b[:items] = last_key ? b[:items][(last_key + 1)..-1] : []
      b[:header_lines] = []
      b[:name] = nil
      b[:removed] = true
    end
  end

  def key_line(key, value, indent)
    indent + TomlLite.dump_key(key) + " = " + TomlLite.dump_value(value)
  end

  def replacement_lines(entry, value)
    first = @lines[entry.line]
    last = @lines[entry.end_line]
    prefix = first[0...entry.value_start_col] || ""
    suffix = last[entry.value_end_col..-1] || ""
    [prefix + TomlLite.dump_value(value) + suffix]
  end

  def section_indent(block)
    block[:items].each do |it|
      next unless it[:kind] == :key
      text = it[:lines][0]
      m = text.match(/\A[ \t]*/)
      return m[0]
    end
    ""
  end

  # 插到该节最后一行内容之后；本节没有键时插到开头注释之后
  def insert_index(block)
    last_key = nil
    block[:items].each_with_index { |it, i| last_key = i if it[:kind] == :key }
    return last_key + 1 if last_key
    idx = 0
    while idx < block[:items].length &&
          block[:items][idx][:kind] == :raw &&
          block[:items][idx][:text].lstrip.start_with?("#")
      idx += 1
    end
    idx
  end

  def serialize(blocks)
    out = []
    blocks.each do |block|
      out << "" if block[:new] && !out.empty? && !out.last.strip.empty?
      out.concat(block[:header_lines])
      block[:items].each_with_index do |it, i|
        # 删掉的节只剩尾部注释/空行；与上一节接缝处不留连续空行
        next if block[:removed] && i.zero? && it[:kind] == :raw && it[:text].strip.empty? &&
                (out.empty? || out.last.strip.empty?)
        if it[:kind] == :key
          out.concat(it[:lines])
        else
          out << it[:text]
        end
      end
    end
    # 删掉末尾的节后，上一节留下的分隔空行会悬在文件尾
    out.pop while blocks.any? { |b| b[:removed] } && out.length > 1 && out.last.strip.empty?
    out
  end

  def self_check(rendered)
    root = begin
      TomlLite.parse(rendered)
    rescue TomlLite::ParseError => e
      raise RenderError, "自检失败：改写后的内容无法解析（#{e.message}）"
    end
    removed = @ops.select { |o| o[:op] == :remove_section }.map { |o| o[:section] }
    removed.each do |sec|
      next if @ops.any? { |o| o[:op] == :set && o[:section] == sec }
      found, = ConfigDoc.lookup(root, TomlLite.split_section_name(sec))
      raise RenderError, "自检失败：节 [#{sec}] 未被删除" if found
    end

    @ops.each do |op|
      next if op[:op] == :remove_section
      parts = TomlLite.split_section_name(op[:section]) + [op[:key]]
      found, value = ConfigDoc.lookup(root, parts)
      label = op[:section].empty? ? op[:key] : "#{op[:section]}.#{op[:key]}"
      if op[:op] == :set
        unless found && ConfigDoc.value_eql?(value, op[:value])
          raise RenderError, "自检失败：#{label} 期望 #{op[:value].inspect}，" \
                             "实际 #{found ? value.inspect : "缺失"}"
        end
      elsif found
        raise RenderError, "自检失败：#{label} 未被删除"
      end
    end

    touched = {}
    @ops.each { |op| touched[[op[:section], op[:key]]] = true }
    seen = {}
    @entries.each do |e|
      pair = [e.section, e.key]
      next if touched[pair] || seen[pair]
      next if removed.any? { |sec| e.section == sec || e.section.start_with?(sec + ".") }
      seen[pair] = true
      parts = e.section_parts + [e.key]
      before_found, before_value = ConfigDoc.lookup(@doc[:root], parts)
      after_found, after_value = ConfigDoc.lookup(root, parts)
      unless before_found == after_found && ConfigDoc.value_eql?(before_value, after_value)
        label = e.section.empty? ? e.key : "#{e.section}.#{e.key}"
        raise RenderError, "自检失败：未改动的键 #{label} 值发生了变化"
      end
    end
    true
  end
end
