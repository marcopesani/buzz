import assert from "node:assert/strict";
import { test } from "node:test";

import { selectWinningReceipt } from "./selectWinningReceipt.ts";

test("selectWinningReceipt: lowest created_at wins", () => {
  const winner = selectWinningReceipt([
    { id: "bbb", createdAt: 200 },
    { id: "aaa", createdAt: 100 },
    { id: "ccc", createdAt: 150 },
  ]);
  assert.deepEqual(winner, { id: "aaa", createdAt: 100 });
});

test("selectWinningReceipt: event-id tiebreak is deterministic", () => {
  const winner = selectWinningReceipt([
    { id: "fff", createdAt: 50 },
    { id: "aaa", createdAt: 50 },
    { id: "mmm", createdAt: 50 },
  ]);
  assert.deepEqual(winner, { id: "aaa", createdAt: 50 });
});

test("selectWinningReceipt: input order does not matter", () => {
  const a = [
    { id: "z", createdAt: 10 },
    { id: "a", createdAt: 10 },
  ];
  const b = [
    { id: "a", createdAt: 10 },
    { id: "z", createdAt: 10 },
  ];
  assert.deepEqual(selectWinningReceipt(a), selectWinningReceipt(b));
});

test("selectWinningReceipt: empty → null", () => {
  assert.equal(selectWinningReceipt([]), null);
});
