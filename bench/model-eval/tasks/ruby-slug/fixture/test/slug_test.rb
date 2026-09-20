# frozen_string_literal: true

require "minitest/autorun"
require "slug"

class SlugTest < Minitest::Test
  def test_lowercases_and_joins_words_with_dashes
    assert_equal "hello-world", Slug.of("Hello, World!")
  end

  def test_collapses_runs_of_separators
    assert_equal "rust-ruby", Slug.of("  Rust & Ruby  ")
    assert_equal "a-b", Slug.of("--a--b--")
  end

  def test_keeps_digits
    assert_equal "v0-78-0-rc21", Slug.of("v0.78.0-rc21")
  end

  def test_nothing_left_gives_empty_slug
    assert_equal "", Slug.of("")
    assert_equal "", Slug.of("!!!")
  end
end
