# frozen_string_literal: true

# 变异：整百年一律不算闰年，把 2000 也排除了。
module LeapYear
  def self.leap?(year)
    (year % 4).zero? && !(year % 100).zero?
  end
end
