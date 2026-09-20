"use strict";

// 把 value 夹到 [low, high] 里；low > high 时抛 RangeError。
function clamp(value, low, high) {
  if (low > high) throw new RangeError("low must not exceed high");
  return Math.min(high, Math.max(low, value));
}

module.exports = { clamp };
