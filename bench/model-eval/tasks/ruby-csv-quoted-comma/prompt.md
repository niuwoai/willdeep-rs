`RowParser.split` 现在是简单按逗号切，遇到双引号包着的字段就切错了。按注释里的规则修好 `lib/row_parser.rb`（引号里的逗号不切、`""` 是一个引号、空字段保留），`test/` 不要动，修完跑 `ruby -Ilib test/row_parser_test.rb`。
