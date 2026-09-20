`clamp.js` 已经实现好了，但 `test/clamp.test.js` 只测了落在区间里的值。请补测试：低于下界返回下界；高于上界返回上界；下界大于上界要抛 `RangeError`。只改 `test/clamp.test.js`，不要动 `clamp.js`，加完跑 `node --test`。
