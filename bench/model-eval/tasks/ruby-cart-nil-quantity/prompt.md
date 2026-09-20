`Cart#total` 碰到没写数量的商品会抛 NoMethodError。按注释里的约定，数量缺省按 1 件算。修 `lib/cart.rb`，`test/` 不要动，修完跑 `ruby -Ilib test/cart_test.rb`。
