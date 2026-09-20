"use strict";

// 变异：只夹下界，不夹上界。
function clamp(value, low, high) {
  if (low > high) throw new RangeError("low must not exceed high");
  return Math.max(low, value);
}

module.exports = { clamp };
