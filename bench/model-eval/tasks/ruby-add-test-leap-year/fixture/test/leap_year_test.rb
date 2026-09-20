# frozen_string_literal: true

require "minitest/autorun"
require "leap_year"

class LeapYearTest < Minitest::Test
  def test_ordinary_years
    assert LeapYear.leap?(2024)
    refute LeapYear.leap?(2023)
  end
end
