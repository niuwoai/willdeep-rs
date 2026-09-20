"use strict";

// 把嵌套数组拍平 depth 层：depth 省略时为 1，Infinity 表示拍到底，0 表示原样浅拷贝。
// 返回的永远是新数组，不改动入参。不许用 Array.prototype.flat。
function flatten(list, depth = 1) {
  throw new Error(`flatten is not implemented (${list.length} items, depth ${depth})`);
}

module.exports = { flatten };
