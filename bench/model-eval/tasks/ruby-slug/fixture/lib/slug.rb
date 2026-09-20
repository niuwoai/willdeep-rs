# frozen_string_literal: true

# 把标题变成 URL 里能用的 slug：全部小写，连续的非字母数字字符折成一个 `-`，
# 首尾不留 `-`；什么都不剩就返回空串。
module Slug
  def self.of(text)
    raise NotImplementedError, "Slug.of is not implemented (#{text.inspect})"
  end
end
