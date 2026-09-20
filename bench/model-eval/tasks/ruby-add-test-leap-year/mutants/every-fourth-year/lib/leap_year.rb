# frozen_string_literal: true

# 变异：只看能不能被 4 整除，把 1900 也当闰年。
module LeapYear
  def self.leap?(year)
    (year % 4).zero?
  end
end
