import { test } from "node:test";
import assert from "node:assert/strict";
import { Cart } from "./cart.ts";

test("total applies discount", () => {
  const c = new Cart();
  c.add(1000);
  c.add(500);
  assert.equal(c.total(10), 1350);
});
