# encoding: utf-8
# TOML 子集解析器：只覆盖 willdeep-config 用得到的构造，零外部依赖。
module TomlLite
  class ParseError < StandardError; end

  # 一次键值赋值的原文位置，供保注释改写使用。
  # 列号按字符计，且 value_start_col...value_end_col 是行内**原始 value token** 区间
  # （含引号等字面符号），不是解析后的值，方便整段替换。
  Assignment = Struct.new(:section, :section_parts, :key, :value,
                          :line, :end_line, :value_start_col, :value_end_col)
  # 一个 [a.b] / [[a.b]] 表头
  Header = Struct.new(:name, :parts, :line, :aot)

  BARE_KEY = /\A[A-Za-z0-9_-]+\z/
  BARE_CHAR = /[A-Za-z0-9_-]/
  TOKEN_CHAR = /[0-9A-Za-z_+\-.]/

  class << self
    def parse(text)
      parse_document(text)[:root]
    end

    # 返回 { root:, entries: [Assignment], headers: [Header] }
    def parse_document(text)
      Parser.new(coerce(text)).run
    end

    # 所有对外返回的字符串都固定成 UTF-8：宿主会直接 JSON.generate
    def utf8(str)
      str.dup.force_encoding(Encoding::UTF_8)
    end

    def coerce(text)
      raise ParseError, "输入必须是字符串（得到 #{text.class}）" unless text.is_a?(String)
      s = text.dup
      s.force_encoding(Encoding::UTF_8)
      raise ParseError, "输入不是合法的 UTF-8 字节序列" unless s.valid_encoding?
      s
    end

    def dump_string(str)
      raise ArgumentError, "需要 String（得到 #{str.class}）" unless str.is_a?(String)
      s = str.dup.force_encoding(Encoding::UTF_8)
      raise ArgumentError, "字符串不是合法的 UTF-8 字节序列" unless s.valid_encoding?
      out = ""
      s.each_char do |ch|
        case ch
        when "\"" then out << "\\\""
        when "\\" then out << "\\\\"
        when "\n" then out << "\\n"
        when "\t" then out << "\\t"
        when "\r" then out << "\\r"
        when "\b" then out << "\\b"
        when "\f" then out << "\\f"
        else
          code = ch.ord
          if code < 0x20 || code == 0x7f
            out << format("\\u%04X", code)
          else
            out << ch
          end
        end
      end
      utf8("\"" + out + "\"")
    end

    def dump_value(value)
      case value
      when String then dump_string(value)
      when TrueClass, FalseClass then value ? "true" : "false"
      when Integer then utf8(value.to_s)
      when Float
        return "inf" if value.infinite? == 1
        return "-inf" if value.infinite? == -1
        return "nan" if value.nan?
        s = value.to_s
        utf8(s =~ /[.eE]/ ? s : s + ".0")
      when Array
        return "[]" if value.empty?
        utf8("[ " + value.map { |v| dump_value(v) }.join(", ") + " ]")
      when NilClass
        raise ArgumentError, "TOML 没有 null，删除键请用 unset"
      else
        raise ArgumentError, "无法写成 TOML 值：#{value.class}"
      end
    end

    def dump_key(key)
      s = validate_key_string(key)
      utf8(s =~ BARE_KEY ? s : dump_string(s))
    end

    def format_section(parts)
      utf8(parts.map { |p| dump_key(p) }.join("."))
    end

    # 把 "a.b" / 'a."b.c"' 拆成 ["a", "b.c"]
    def split_section_name(name)
      s = name.to_s
      return [] if s.empty?
      parts = []
      buf = String.new
      i = 0
      while i < s.length
        c = s[i]
        if c == "\"" || c == "'"
          i = read_quoted_part(s, i, c, buf)
        elsif c == "."
          parts << buf
          buf = String.new
          i += 1
        else
          buf << c
          i += 1
        end
      end
      parts << buf
      parts
    end

    def validate_key_string(key)
      raise ArgumentError, "键名必须是字符串（得到 #{key.class}）" unless key.is_a?(String)
      s = key.dup.force_encoding(Encoding::UTF_8)
      raise ArgumentError, "键名不能为空" if s.empty?
      raise ArgumentError, "键名不是合法的 UTF-8 字节序列" unless s.valid_encoding?
      raise ArgumentError, "键名不能包含换行或回车" if s =~ /[\n\r]/
      s
    end

    private

    def read_quoted_part(s, i, quote, buf)
      i += 1
      closed = false
      while i < s.length
        ch = s[i]
        if quote == "\"" && ch == "\\"
          i += 1
          raise ParseError, "节名中转义不完整：#{s.inspect}" if i >= s.length
          esc = s[i]
          buf << case esc
                 when "n" then "\n"
                 when "t" then "\t"
                 else esc
                 end
          i += 1
          next
        end
        if ch == quote
          closed = true
          i += 1
          break
        end
        buf << ch
        i += 1
      end
      raise ParseError, "节名中引号未闭合：#{s.inspect}" unless closed
      i
    end
  end

  class Parser
    def initialize(text)
      @text = text
      @len = text.length
      @pos = 0
      @line = 0
      @line_start = 0
      @root = {}
      @current = @root
      @current_name = ""
      @current_parts = []
      @entries = []
      @headers = []
    end

    def run
      loop do
        skip_junk
        break if eof?
        if peek == "["
          parse_header
        else
          parse_assignment
        end
      end
      { root: @root, entries: @entries, headers: @headers }
    end

    private

    def eof?
      @pos >= @len
    end

    def peek(offset = 0)
      @text[@pos + offset]
    end

    def advance
      ch = @text[@pos]
      @pos += 1
      if ch == "\n"
        @line += 1
        @line_start = @pos
      end
      ch
    end

    def line_no
      @line + 1
    end

    def col
      @pos - @line_start
    end

    def error(msg)
      raise ParseError, "#{msg}（第 #{line_no} 行）"
    end

    def skip_junk
      loop do
        break if eof?
        c = peek
        if c == " " || c == "\t" || c == "\r" || c == "\n"
          advance
        elsif c == "#"
          skip_comment
        else
          break
        end
      end
    end

    def skip_comment
      advance
      advance while !eof? && peek != "\n"
    end

    def skip_inline_ws
      advance while !eof? && (peek == " " || peek == "\t" || peek == "\r")
    end

    def skip_array_ws
      loop do
        break if eof?
        c = peek
        if c == " " || c == "\t" || c == "\r" || c == "\n"
          advance
        elsif c == "#"
          skip_comment
        else
          break
        end
      end
    end

    def parse_header
      start_line = @line
      advance
      aot = false
      if peek == "["
        aot = true
        advance
      end
      skip_inline_ws
      parts = parse_key_path
      skip_inline_ws
      error("表头缺少 ]") unless peek == "]"
      advance
      if aot
        error("数组表头缺少 ]]") unless peek == "]"
        advance
      end
      skip_inline_ws
      skip_comment if peek == "#"
      error("表头后有多余内容") unless eof? || peek == "\n"
      name = TomlLite.format_section(parts)
      @headers << Header.new(name, parts, start_line, aot)
      @current = resolve_table(parts, aot)
      @current_parts = parts
      @current_name = name
    end

    def resolve_table(parts, aot)
      node = @root
      parts.each_with_index do |p, idx|
        last = (idx == parts.length - 1)
        if last && aot
          node[p] = [] unless node.key?(p)
          error("数组表 #{TomlLite.format_section(parts)} 与已有键冲突") unless node[p].is_a?(Array)
          elem = {}
          node[p] << elem
          return elem
        end
        if node.key?(p)
          cur = node[p]
          if cur.is_a?(Array)
            error("表 #{p} 是数组表，不能这样嵌套") if cur.empty?
            node = cur.last
          elsif cur.is_a?(Hash)
            node = cur
          else
            error("键 #{p} 已存在且不是表，无法定义表 #{TomlLite.format_section(parts)}")
          end
        else
          created = {}
          node[p] = created
          node = created
        end
      end
      node
    end

    def parse_assignment
      start_line = @line
      parts = parse_key_path
      key = parts.length == 1 ? parts[0] : TomlLite.format_section(parts)
      skip_inline_ws
      error("键 #{key} 后缺少 =") unless peek == "="
      advance
      skip_inline_ws
      value_start_col = col
      value = parse_value
      end_line = @line
      value_end_col = col
      skip_inline_ws
      skip_comment if peek == "#"
      error("值后有多余内容") unless eof? || peek == "\n"
      assign(parts, value, start_line, end_line, value_start_col, value_end_col)
    end

    def assign(parts, value, start_line, end_line, value_start_col, value_end_col)
      target = @current
      prefix = parts[0...-1]
      target = resolve_inline_dotted(prefix) unless prefix.empty?
      leaf = parts[-1]
      error("键 #{leaf} 重复定义") if target.key?(leaf)
      target[leaf] = value
      section_parts = @current_parts + prefix
      @entries << Assignment.new(TomlLite.format_section(section_parts), section_parts, leaf, value,
                                 start_line, end_line, value_start_col, value_end_col)
    end

    def resolve_inline_dotted(parts)
      node = @current
      parts.each do |p|
        if node.key?(p)
          cur = node[p]
          error("键 #{p} 不是表") unless cur.is_a?(Hash)
          node = cur
        else
          created = {}
          node[p] = created
          node = created
        end
      end
      node
    end

    def parse_key_path
      parts = []
      loop do
        skip_inline_ws
        parts << parse_key_part
        skip_inline_ws
        break unless peek == "."
        advance
      end
      parts
    end

    def parse_key_part
      c = peek
      if c == "\""
        parse_basic_string
      elsif c == "'"
        parse_literal_string
      elsif c && c =~ BARE_CHAR
        start = @pos
        advance while !eof? && peek =~ BARE_CHAR
        @text[start...@pos]
      else
        error("无法解析键名")
      end
    end

    def parse_value
      c = peek
      error("缺少值") if c.nil?
      case c
      when "\"" then parse_basic_string
      when "'" then parse_literal_string
      when "[" then parse_array
      when "{" then error("不支持 inline table")
      else
        if c =~ /[0-9+\-A-Za-z]/
          parse_scalar_word
        else
          error("无法识别的值 #{c.inspect}")
        end
      end
    end

    def parse_basic_string
      error("不支持多行基本字符串 \"\"\"") if peek(1) == "\"" && peek(2) == "\""
      advance
      buf = String.new
      loop do
        error("基本字符串未闭合") if eof?
        c = peek
        if c == "\""
          advance
          return buf
        elsif c == "\\"
          buf << parse_escape
        elsif c == "\n"
          error("基本字符串中不能有裸换行")
        else
          buf << c
          advance
        end
      end
    end

    def parse_literal_string
      error("不支持多行字面串 '''") if peek(1) == "'" && peek(2) == "'"
      advance
      buf = String.new
      loop do
        error("字面串未闭合") if eof?
        c = peek
        if c == "'"
          advance
          return buf
        elsif c == "\n"
          error("字面串中不能有换行")
        else
          buf << c
          advance
        end
      end
    end

    def parse_escape
      advance
      error("转义符后就是文件结尾") if eof?
      c = advance
      case c
      when "\"" then "\""
      when "\\" then "\\"
      when "n" then "\n"
      when "t" then "\t"
      when "r" then "\r"
      when "b" then "\b"
      when "f" then "\f"
      when "u" then parse_unicode(4)
      when "U" then parse_unicode(8)
      else
        error("不支持的转义 \\#{c}")
      end
    end

    def parse_unicode(len)
      hex = String.new
      len.times do
        error("\\u 转义不完整") if eof?
        c = peek
        error("\\u 转义含非法字符 #{c.inspect}") unless c =~ /[0-9A-Fa-f]/
        hex << advance
      end
      code = hex.to_i(16)
      error("非法 Unicode 码点 U+#{hex}") if code > 0x10FFFF || (code >= 0xD800 && code <= 0xDFFF)
      [code].pack("U")
    end

    def parse_array
      advance
      arr = []
      loop do
        skip_array_ws
        error("数组未闭合") if eof?
        if peek == "]"
          advance
          return arr
        end
        arr << parse_value
        skip_array_ws
        error("数组未闭合") if eof?
        if peek == ","
          advance
        elsif peek == "]"
          advance
          return arr
        else
          error("数组元素后需要 , 或 ]")
        end
      end
    end

    def parse_scalar_word
      start = @pos
      advance while !eof? && peek =~ TOKEN_CHAR
      token = @text[start...@pos]
      case token
      when "true" then true
      when "false" then false
      when "inf", "+inf" then Float::INFINITY
      when "-inf" then -Float::INFINITY
      when "nan", "+nan", "-nan" then Float::NAN
      else parse_number_token(token)
      end
    end

    def parse_number_token(token)
      body = token
      sign = 1
      if body.start_with?("-")
        sign = -1
        body = body[1..-1]
      elsif body.start_with?("+")
        body = body[1..-1]
      end
      if body =~ /\A0x[0-9A-Fa-f](_?[0-9A-Fa-f])*\z/
        sign * body.delete("_")[2..-1].to_i(16)
      elsif body =~ /\A0o[0-7](_?[0-7])*\z/
        sign * body.delete("_")[2..-1].to_i(8)
      elsif body =~ /\A0b[01](_?[01])*\z/
        sign * body.delete("_")[2..-1].to_i(2)
      elsif body =~ /\A(0|[1-9](_?[0-9])*)\z/
        sign * body.delete("_").to_i(10)
      elsif body =~ /\A(0|[1-9](_?[0-9])*)\.[0-9](_?[0-9])*([eE][+\-]?[0-9](_?[0-9])*)?\z/ ||
            body =~ /\A(0|[1-9](_?[0-9])*)[eE][+\-]?[0-9](_?[0-9])*\z/
        sign * body.delete("_").to_f
      else
        error("无法解析数值 #{token.inspect}")
      end
    end
  end
end
