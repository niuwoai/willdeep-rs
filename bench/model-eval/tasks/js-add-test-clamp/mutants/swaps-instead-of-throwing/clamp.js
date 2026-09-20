"use strict";

// 变异：下界大于上界时悄悄对调，而不是抛错。
function clamp(value, low, high) {
  if (low > high) [low, high] = [high, low];
  return Math.min(high, Math.max(low, value));
}

module.exports = { clamp };
