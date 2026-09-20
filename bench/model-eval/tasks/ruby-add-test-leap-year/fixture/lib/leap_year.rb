# frozen_string_literal: true

# 格里高利历闰年：能被 4 整除，但整百年要能被 400 整除。
module LeapYear
  def self.leap?(year)
    (year % 4).zero? && (!(year % 100).zero? || (year % 400).zero?)
  end
end
