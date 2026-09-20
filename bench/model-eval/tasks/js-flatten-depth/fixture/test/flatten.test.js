"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const { flatten } = require("../flatten");

const nested = [1, [2, [3, [4]]]];

test("default depth flattens one level", () => {
  assert.deepEqual(flatten(nested), [1, 2, [3, [4]]]);
});

test("explicit depth", () => {
  assert.deepEqual(flatten(nested, 2), [1, 2, 3, [4]]);
  assert.deepEqual(flatten(nested, Infinity), [1, 2, 3, 4]);
});

test("depth zero is a shallow copy", () => {
  const copy = flatten(nested, 0);
  assert.deepEqual(copy, nested);
  assert.notEqual(copy, nested);
});

test("input is not mutated", () => {
  const input = [[1], [2]];
  flatten(input, Infinity);
  assert.deepEqual(input, [[1], [2]]);
});
