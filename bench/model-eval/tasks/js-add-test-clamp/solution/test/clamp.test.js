"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const { clamp } = require("../clamp");

test("values inside the range are unchanged", () => {
  assert.equal(clamp(5, 0, 10), 5);
});

test("values below the range snap to the low bound", () => {
  assert.equal(clamp(-3, 0, 10), 0);
});

test("values above the range snap to the high bound", () => {
  assert.equal(clamp(15, 0, 10), 10);
});

test("an inverted range is rejected", () => {
  assert.throws(() => clamp(5, 10, 0), RangeError);
});
