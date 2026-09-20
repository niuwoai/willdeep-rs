# frozen_string_literal: true

require "minitest/autorun"
require "row_parser"

class RowParserTest < Minitest::Test
  def test_plain_fields
    assert_equal %w[a b c], RowParser.split("a,b,c")
  end

  def test_quoted_field_keeps_its_comma
    assert_equal ["a", "b,c", "d"], RowParser.split('a,"b,c",d')
  end

  def test_doubled_quote_is_a_literal_quote
    assert_equal ['say "hi"', "x"], RowParser.split('"say ""hi""",x')
  end

  def test_empty_fields_are_preserved
    assert_equal ["a", "", "b"], RowParser.split("a,,b")
    assert_equal ["x", ""], RowParser.split("x,")
  end
end
