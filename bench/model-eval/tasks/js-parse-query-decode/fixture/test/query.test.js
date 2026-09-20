"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const { parseQuery } = require("../query");

test("plain pairs", () => {
  assert.deepEqual(parseQuery("a=1&b=2"), { a: "1", b: "2" });
  assert.deepEqual(parseQuery("?a=1"), { a: "1" });
});

test("decodes percent escapes and plus signs", () => {
  assert.deepEqual(parseQuery("name=hello%20world&plus=a+b"), { name: "hello world", plus: "a b" });
  assert.deepEqual(parseQuery("k%3Dey=v%26"), { "k=ey": "v&" });
});

test("bare keys and empty input", () => {
  assert.deepEqual(parseQuery("flag"), { flag: "" });
  assert.deepEqual(parseQuery(""), {});
});
