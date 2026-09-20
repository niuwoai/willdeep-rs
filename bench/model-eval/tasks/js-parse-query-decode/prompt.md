`query.js` 的 `parseQuery` 把百分号编码和 `+` 原样留在结果里了，键和值都该解码。按文件注释里的规则修好，只改 `query.js`，`test/` 不要动，修完跑 `node --test`。
