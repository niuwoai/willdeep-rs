"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const { clamp } = require("../clamp");

test("values inside the range are unchanged", () => {
  assert.equal(clamp(5, 0, 10), 5);
});
