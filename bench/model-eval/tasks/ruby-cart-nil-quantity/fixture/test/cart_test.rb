# frozen_string_literal: true

require "minitest/autorun"
require "cart"

class CartTest < Minitest::Test
  def test_multiplies_price_by_quantity
    assert_equal 10, Cart.new([{ price: 5, quantity: 2 }]).total
  end

  def test_missing_quantity_counts_as_one
    assert_equal 13, Cart.new([{ price: 10, quantity: nil }, { price: 3 }]).total
  end

  def test_empty_cart_is_free
    assert_equal 0, Cart.new.total
  end
end
