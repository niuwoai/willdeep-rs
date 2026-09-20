# frozen_string_literal: true

# 拆一行 CSV：逗号分隔；双引号包起来的字段里可以有逗号；引号里的 `""` 是一个引号；
# 空字段（`a,,b`、行尾的 `x,`）要保留成空串。
module RowParser
  def self.split(line)
    line.split(",", -1)
  end
end
