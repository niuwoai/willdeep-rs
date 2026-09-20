# frozen_string_literal: true

require "minitest/autorun"
require "leap_year"

class LeapYearTest < Minitest::Test
  def test_ordinary_years
    assert LeapYear.leap?(2024)
    refute LeapYear.leap?(2023)
  end

  def test_century_years_are_not_leap
    refute LeapYear.leap?(1900)
    refute LeapYear.leap?(2100)
  end

  def test_every_four_hundred_years_is_leap
    assert LeapYear.leap?(2000)
    assert LeapYear.leap?(1600)
  end
end
