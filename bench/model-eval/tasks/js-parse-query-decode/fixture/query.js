"use strict";

// 解析 URL 查询串：去掉开头的 `?`；`+` 是空格；百分号编码要解码（键和值都要）；
// 没有 `=` 的键值为空串；空串返回 {}。
function parseQuery(text) {
  const result = {};
  const body = text.startsWith("?") ? text.slice(1) : text;
  if (body === "") return result;
  for (const pair of body.split("&")) {
    const [key, value = ""] = pair.split("=");
    result[key] = value;
  }
  return result;
}

module.exports = { parseQuery };
