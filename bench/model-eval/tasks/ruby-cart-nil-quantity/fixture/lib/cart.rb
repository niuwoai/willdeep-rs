# frozen_string_literal: true

# 购物车：每项是 { price:, quantity: }。quantity 缺省（nil）按 1 件算。
class Cart
  def initialize(items = [])
    @items = items
  end

  def total
    @items.sum { |item| item[:price] * item[:quantity] }
  end
end
