import assert from "node:assert/strict";
import { test } from "node:test";

import {
  derivePayCardState,
  payCardShowsPayButton,
} from "./derivePayCardState.ts";

const BASE = {
  requestPubkey: "aa".repeat(32),
  myPubkey: "bb".repeat(32),
  expiryUnix: 2_000,
  nowUnix: 1_000,
  hasReceipt: false,
  walletLinked: true,
};

test("derivePayCardState: payable when linked stranger request", () => {
  const state = derivePayCardState(BASE);
  assert.equal(state.kind, "payable");
  assert.equal(payCardShowsPayButton(state), true);
});

test("derivePayCardState: paid wins over everything", () => {
  const state = derivePayCardState({
    ...BASE,
    hasReceipt: true,
    expiryUnix: 500,
    requestPubkey: BASE.myPubkey,
  });
  assert.equal(state.kind, "paid");
  assert.equal(payCardShowsPayButton(state), false);
});

test("derivePayCardState: expired hides Pay", () => {
  const state = derivePayCardState({
    ...BASE,
    nowUnix: 2_000,
    expiryUnix: 2_000,
  });
  assert.equal(state.kind, "expired");
  assert.equal(payCardShowsPayButton(state), false);
});

test("derivePayCardState: own request hides Pay", () => {
  const state = derivePayCardState({
    ...BASE,
    requestPubkey: BASE.myPubkey,
  });
  assert.equal(state.kind, "own_request");
  assert.equal(payCardShowsPayButton(state), false);
});

test("derivePayCardState: unlinked hides Pay", () => {
  const state = derivePayCardState({
    ...BASE,
    walletLinked: false,
  });
  assert.equal(state.kind, "unlinked");
  assert.equal(payCardShowsPayButton(state), false);
});
