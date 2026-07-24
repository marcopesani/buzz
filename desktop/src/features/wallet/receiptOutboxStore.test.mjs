import assert from "node:assert/strict";
import { afterEach, describe, it } from "node:test";

import { emptyReceiptOutbox, enqueueReceipt } from "./receiptOutbox.ts";
import {
  getReceiptOutboxState,
  initReceiptOutbox,
  peekReceiptOutboxForCommunity,
  resetReceiptOutbox,
  setReceiptOutboxState,
  writeReceiptOutboxForCommunity,
} from "./receiptOutboxStore.ts";

function makeLocalStorage() {
  const map = new Map();
  return {
    getItem: (k) => (map.has(k) ? map.get(k) : null),
    setItem: (k, v) => {
      map.set(k, String(v));
    },
    removeItem: (k) => {
      map.delete(k);
    },
    clear: () => map.clear(),
    key: (i) => [...map.keys()][i] ?? null,
    get length() {
      return map.size;
    },
  };
}

function installLocalStorage() {
  const ls = makeLocalStorage();
  globalThis.localStorage = ls;
  if (typeof globalThis.window === "undefined") {
    globalThis.window = { localStorage: ls };
  } else {
    globalThis.window.localStorage = ls;
  }
  // Active community id used by ensureBound().
  ls.setItem("buzz-active-community-id", "community-a");
  return ls;
}

afterEach(() => {
  resetReceiptOutbox();
});

describe("receiptOutboxStore community isolation", () => {
  it("persists per community and does not leak across reset+init", () => {
    installLocalStorage();
    initReceiptOutbox("community-a");
    const state = enqueueReceipt(emptyReceiptOutbox(), {
      requestEventId: "aa".repeat(32),
      paymentHash: "bb".repeat(32),
      preimage: "cc".repeat(32),
      amountMsat: 1000,
      channelId: "ch-a",
      nowUnix: 1,
    });
    setReceiptOutboxState(state);
    assert.equal(getReceiptOutboxState().pending.length, 1);

    resetReceiptOutbox();
    // In-memory cleared; durable for A still present.
    assert.equal(
      peekReceiptOutboxForCommunity("community-a").pending.length,
      1,
    );

    // Community B starts empty.
    initReceiptOutbox("community-b");
    assert.equal(getReceiptOutboxState().pending.length, 0);
    setReceiptOutboxState(
      enqueueReceipt(emptyReceiptOutbox(), {
        requestEventId: "dd".repeat(32),
        paymentHash: "ee".repeat(32),
        preimage: "ff".repeat(32),
        amountMsat: 2000,
        channelId: "ch-b",
        nowUnix: 2,
      }),
    );
    assert.equal(getReceiptOutboxState().pending.length, 1);
    assert.equal(
      peekReceiptOutboxForCommunity("community-a").pending[0].channelId,
      "ch-a",
    );
    assert.equal(
      peekReceiptOutboxForCommunity("community-b").pending[0].channelId,
      "ch-b",
    );

    // Switch back to A — durable pending restored.
    resetReceiptOutbox();
    initReceiptOutbox("community-a");
    assert.equal(getReceiptOutboxState().pending[0].channelId, "ch-a");
  });

  it("survives process restart via localStorage", () => {
    installLocalStorage();
    writeReceiptOutboxForCommunity(
      "community-a",
      enqueueReceipt(emptyReceiptOutbox(), {
        requestEventId: "aa".repeat(32),
        paymentHash: "bb".repeat(32),
        preimage: "cc".repeat(32),
        amountMsat: 42,
        channelId: null,
        nowUnix: 9,
      }),
    );
    resetReceiptOutbox();
    initReceiptOutbox("community-a");
    assert.equal(getReceiptOutboxState().pending[0].amountMsat, 42);
  });
});
