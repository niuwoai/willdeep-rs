# frozen_string_literal: true

# 拆一行 CSV：逗号分隔；双引号包起来的字段里可以有逗号；引号里的 `""` 是一个引号；
# 空字段（`a,,b`、行尾的 `x,`）要保留成空串。
module RowParser
  def self.split(line)
    fields = []
    field = +""
    quoted = false
    chars = line.chars
    index = 0
    while index < chars.length
      char = chars[index]
      if quoted
        if char == '"'
          if chars[index + 1] == '"'
            field << '"'
            index += 1
          else
            quoted = false
          end
        else
          field << char
        end
      elsif char == '"'
        quoted = true
      elsif char == ","
        fields << field
        field = +""
      else
        field << char
      end
      index += 1
    end
    fields << field
  end
end
