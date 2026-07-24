import { expect, test, type Page } from "@playwright/test";
import { createHash } from "node:crypto";
import { mkdirSync, readFileSync } from "node:fs";
import path from "node:path";

import { installMockBridge, TEST_IDENTITIES } from "../helpers/bridge";
import { waitForAnimations } from "../helpers/animations";

const OUT_DIR = path.resolve("test-results/wallet-receipt-screenshots");
const CHANNEL = "random";
const VIEWER = "deadbeef".repeat(8);
const ALICE = TEST_IDENTITIES.alice.pubkey;
const REQUEST_ID = "11".repeat(32);
const REQUEST_ID_RETRY = "12".repeat(32);
const REQUEST_ID_DEDUPE = "13".repeat(32);
const RECEIPT_ID_EXISTING = "14".repeat(32);
const PAYMENT_HASH = "cd".repeat(32);
const PREIMAGE = "ab".repeat(32);
const AMOUNT_MSAT = 210_000;

const LINKED_WALLET = {
  linked: true,
  capabilities: [
    "pay_invoice",
    "make_invoice",
    "lookup_invoice",
    "get_balance",
  ],
  receive_mode: "interactive",
  lud16: "alice@getalby.com",
  balance_msat: 21_000_000,
} as const;

async function waitForMockLiveSubscription(page: Page, channelName: string) {
  await expect
    .poll(async () =>
      page.evaluate(
        (name) =>
          window.__BUZZ_E2E_HAS_MOCK_LIVE_SUBSCRIPTION__?.({
            channelName: name,
          }) ?? false,
        channelName,
      ),
    )
    .toBe(true);
}

/** Seed a normal row so the channel leaves empty-intro mode (live rows mount). */
async function seedChannelMessage(page: Page) {
  await page.evaluate((channelName) => {
    window.__BUZZ_E2E_EMIT_MOCK_MESSAGE__?.({
      channelName,
      content: "seed for payment receipt e2e",
      kind: 9,
    });
  }, CHANNEL);
  await expect(page.getByText("seed for payment receipt e2e")).toBeVisible({
    timeout: 10_000,
  });
}

async function emitPaymentRequest(
  page: Page,
  input: {
    id: string;
    amountMsat: number;
    memo: string;
  },
) {
  const event = await page.evaluate(
    ({ channelName, id, amountMsat, memo, pubkey }) => {
      return (
        window.__BUZZ_E2E_EMIT_MOCK_MESSAGE__?.({
          channelName,
          content: memo,
          pubkey,
          kind: 40009,
          id,
          createdAt: Math.floor(Date.now() / 1000) - 10,
          extraTags: [
            ["amount", String(amountMsat)],
            ["memo", memo],
            ["expiry", String(Math.floor(Date.now() / 1000) + 3600)],
            ["p", pubkey],
            ["bolt11", "lnbc210n1pmockpayrequest"],
          ],
        }) ?? null
      );
    },
    {
      channelName: CHANNEL,
      id: input.id,
      amountMsat: input.amountMsat,
      memo: input.memo,
      pubkey: ALICE,
    },
  );
  expect(event, "mock emit should return the 40009 event").toBeTruthy();
  expect((event as { id?: string }).id).toBe(input.id);
}

async function countSignedReceipts(page: Page): Promise<number> {
  return page.evaluate(() => {
    const signed =
      (
        window as Window & {
          __BUZZ_E2E_SIGNED_EVENTS__?: Array<{ kind: number }>;
        }
      ).__BUZZ_E2E_SIGNED_EVENTS__ ?? [];
    return signed.filter((e) => e.kind === 40010).length;
  });
}

/** Receipts accepted by the mock relay (OK true → store), not merely signed. */
async function countStoredReceipts(page: Page): Promise<number> {
  return page.evaluate(
    (channelName) =>
      (
        window as Window & {
          __BUZZ_E2E_MOCK_KIND_COUNT__?: (input: {
            channelName: string;
            kind: number;
          }) => number;
        }
      ).__BUZZ_E2E_MOCK_KIND_COUNT__?.({
        channelName,
        kind: 40010,
      }) ?? 0,
    CHANNEL,
  );
}

async function latestSignedReceipt(page: Page) {
  return page.evaluate(() => {
    const signed =
      (
        window as Window & {
          __BUZZ_E2E_SIGNED_EVENTS__?: Array<{
            kind: number;
            tags: string[][];
            content: string;
          }>;
        }
      ).__BUZZ_E2E_SIGNED_EVENTS__ ?? [];
    return [...signed].reverse().find((e) => e.kind === 40010) ?? null;
  });
}

async function outboxPendingCount(page: Page): Promise<number> {
  return page.evaluate(() => {
    const raw = window.localStorage.getItem(
      "buzz-wallet-receipt-outbox.v1:e2e-default-community",
    );
    if (!raw) return 0;
    try {
      const parsed = JSON.parse(raw) as { pending?: unknown[] };
      return Array.isArray(parsed.pending) ? parsed.pending.length : 0;
    } catch {
      return 0;
    }
  });
}

async function reconcileReceipts(page: Page) {
  await page.evaluate(async () => {
    const fn = (
      window as Window & {
        __BUZZ_E2E_RECONCILE_RECEIPTS__?: () => Promise<unknown>;
      }
    ).__BUZZ_E2E_RECONCILE_RECEIPTS__;
    if (!fn) {
      throw new Error("__BUZZ_E2E_RECONCILE_RECEIPTS__ not registered");
    }
    await fn();
  });
}

function sha256File(filePath: string): string {
  return createHash("sha256").update(readFileSync(filePath)).digest("hex");
}

async function openChannelWithWallet(page: Page) {
  await page.goto("/");
  await page.getByTestId(`channel-${CHANNEL}`).click();
  await waitForMockLiveSubscription(page, CHANNEL);
  await seedChannelMessage(page);
}

async function payVisibleRequestCard(page: Page) {
  const card = page.getByTestId("payment-request-card");
  await expect(card).toBeVisible({ timeout: 10_000 });
  await card.getByTestId("payment-request-pay").click();
  await expect(page.getByTestId("wallet-confirm-dialog")).toBeVisible();
  await page.getByTestId("wallet-confirm-submit").click();
  await expect(page.getByTestId("wallet-confirm-settled")).toBeVisible({
    timeout: 15_000,
  });
}

test.describe.configure({ mode: "serial" });
// Confirm + outbox + channel remount can exceed the default 30s smoke budget.
test.describe.configure({ timeout: 60_000 });

test.beforeEach(async () => {
  mkdirSync(OUT_DIR, { recursive: true });
});

const screenshotHashes: string[] = [];

test("confirm settle publishes 40010 and flips pay card to paid", async ({
  page,
}) => {
  await installMockBridge(page, {
    walletStatus: LINKED_WALLET,
    walletConfirmOutcome: {
      status: "settled",
      preimage: PREIMAGE,
    },
    searchProfiles: [{ pubkey: ALICE, displayName: "alice", isAgent: false }],
  });

  await openChannelWithWallet(page);

  await emitPaymentRequest(page, {
    id: REQUEST_ID,
    amountMsat: AMOUNT_MSAT,
    memo: "receipt path a",
  });

  await payVisibleRequestCard(page);

  await expect.poll(async () => countSignedReceipts(page)).toBe(1);

  const receipt = await latestSignedReceipt(page);
  expect(receipt).not.toBeNull();
  expect(receipt?.content).toBe("");
  const tag = (name: string) =>
    receipt?.tags.find((t) => t[0] === name)?.[1] ?? null;
  expect(tag("e")).toBe(REQUEST_ID);
  expect(tag("payment_hash")).toBe(PAYMENT_HASH);
  expect(tag("preimage")).toBe(PREIMAGE);
  expect(tag("amount")).toBe(String(AMOUNT_MSAT));
  expect(tag("h")).toBeTruthy();
  expect(tag("p")).toBeNull();

  // Publish succeeded → outbox acked (empty pending).
  await expect.poll(async () => outboxPendingCount(page)).toBe(0);

  // Dismiss confirm without relying on Done click actionability (Radix focus
  // trap can stall pointer events under the settled toast).
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("wallet-confirm-dialog")).toHaveCount(0, {
    timeout: 5_000,
  });

  // Prefer live join; if the deferred timeline lags, a channel remount refetches
  // the newest window (aux 40010 is in CHANNEL_WINDOW_AUX_KINDS) from the store.
  const paid = page.locator('[data-pay-card-state="paid"]');
  try {
    await expect(paid).toBeVisible({ timeout: 3_000 });
  } catch {
    await page.getByTestId("channel-general").click();
    await page.getByTestId(`channel-${CHANNEL}`).click();
    await waitForMockLiveSubscription(page, CHANNEL);
    await expect(paid).toBeVisible({ timeout: 10_000 });
  }
  await expect(paid.getByTestId("payment-request-status")).toHaveText("Paid ✓");
  await paid.evaluate((el) => {
    el.scrollIntoView({ block: "center", inline: "nearest" });
  });
  await expect(paid).toBeInViewport();

  await waitForAnimations(page);
  const paidShot = path.join(OUT_DIR, "01-paid-card-with-receipt.png");
  await paid.screenshot({ path: paidShot });
  screenshotHashes.push(sha256File(paidShot));
});

test("publish failure lands in outbox; reconcile publishes exactly once", async ({
  page,
}) => {
  await installMockBridge(page, {
    walletStatus: LINKED_WALLET,
    walletConfirmOutcome: {
      status: "settled",
      preimage: PREIMAGE,
    },
    paymentReceiptPublishErrors: ["mock receipt publish rejected"],
    searchProfiles: [{ pubkey: ALICE, displayName: "alice", isAgent: false }],
  });

  await openChannelWithWallet(page);

  await emitPaymentRequest(page, {
    id: REQUEST_ID_RETRY,
    amountMsat: AMOUNT_MSAT,
    memo: "outbox retry",
  });

  await payVisibleRequestCard(page);

  await expect.poll(async () => outboxPendingCount(page)).toBe(1);
  // Sign runs before relay OK — a rejected publish still leaves a signed
  // artifact, but nothing is stored until OK true.
  expect(await countStoredReceipts(page)).toBe(0);

  await waitForAnimations(page);
  const outboxShot = path.join(OUT_DIR, "02-outbox-retry-moment.png");
  await page.getByTestId("wallet-confirm-dialog").screenshot({
    path: outboxShot,
  });
  screenshotHashes.push(sha256File(outboxShot));

  await page.keyboard.press("Escape");
  await expect(page.getByTestId("wallet-confirm-dialog")).toHaveCount(0, {
    timeout: 5_000,
  });

  const signedBeforeRetry = await countSignedReceipts(page);
  await reconcileReceipts(page);

  // Exactly one accepted receipt in the store; one additional sign for retry.
  await expect.poll(async () => countStoredReceipts(page)).toBe(1);
  await expect
    .poll(async () => countSignedReceipts(page))
    .toBe(signedBeforeRetry + 1);
  await expect.poll(async () => outboxPendingCount(page)).toBe(0);

  const receipt = await latestSignedReceipt(page);
  expect(receipt?.tags.find((t) => t[0] === "e")?.[1]).toBe(REQUEST_ID_RETRY);
  expect(receipt?.tags.find((t) => t[0] === "payment_hash")?.[1]).toBe(
    PAYMENT_HASH,
  );
});

test("reconcile does not re-publish when receipt already on timeline", async ({
  page,
}) => {
  await installMockBridge(page, {
    walletStatus: LINKED_WALLET,
    walletReconcileSettled: [
      {
        request_event_id: REQUEST_ID_DEDUPE,
        payment_hash: PAYMENT_HASH,
        preimage: PREIMAGE,
        amount_msat: AMOUNT_MSAT,
      },
    ],
    searchProfiles: [{ pubkey: ALICE, displayName: "alice", isAgent: false }],
  });

  await openChannelWithWallet(page);

  await page.evaluate(
    ({
      channelName,
      receiptId,
      requestId,
      viewer,
      paymentHash,
      preimage,
      amountMsat,
    }) => {
      window.__BUZZ_E2E_EMIT_MOCK_MESSAGE__?.({
        channelName,
        content: "",
        pubkey: viewer,
        kind: 40010,
        id: receiptId,
        createdAt: Math.floor(Date.now() / 1000) - 5,
        extraTags: [
          ["e", requestId],
          ["payment_hash", paymentHash],
          ["preimage", preimage],
          ["amount", String(amountMsat)],
        ],
      });
    },
    {
      channelName: CHANNEL,
      receiptId: RECEIPT_ID_EXISTING,
      requestId: REQUEST_ID_DEDUPE,
      viewer: VIEWER,
      paymentHash: PAYMENT_HASH,
      preimage: PREIMAGE,
      amountMsat: AMOUNT_MSAT,
    },
  );

  await emitPaymentRequest(page, {
    id: REQUEST_ID_DEDUPE,
    amountMsat: AMOUNT_MSAT,
    memo: "already receipted",
  });

  const paid = page.locator('[data-pay-card-state="paid"]');
  await expect(paid).toBeVisible({ timeout: 10_000 });
  await expect(paid.getByTestId("payment-request-status")).toHaveText("Paid ✓");

  const before = await countSignedReceipts(page);
  await reconcileReceipts(page);
  await page.waitForTimeout(500);
  expect(await countSignedReceipts(page)).toBe(before);

  // Re-assert paid state and center the card before capture. A bottom-docked
  // card sits under the sticky composer — element screenshots then capture
  // composer pixels instead of the emerald Paid ✓ card.
  const paidCard = page
    .getByTestId("payment-request-card")
    .filter({ hasText: "already receipted" });
  await expect(paidCard).toHaveAttribute("data-pay-card-state", "paid");
  await expect(paidCard.getByTestId("payment-request-status")).toHaveText(
    "Paid ✓",
  );
  await paidCard.evaluate((el) => {
    el.scrollIntoView({ block: "center", inline: "nearest" });
  });
  await expect(paidCard).toBeInViewport();
  const box = await paidCard.boundingBox();
  expect(box, "paid card must have a layout box").not.toBeNull();
  if (!box) {
    throw new Error("paid card bounding box missing");
  }
  expect(box.height).toBeGreaterThan(60);
  expect(box.width).toBeGreaterThan(200);
  await expect(paidCard.getByTestId("payment-request-memo")).toBeVisible();
  await waitForAnimations(page);
  const dedupeShot = path.join(OUT_DIR, "03-dedupe-already-paid.png");
  await paidCard.screenshot({ path: dedupeShot });
  screenshotHashes.push(sha256File(dedupeShot));

  expect(screenshotHashes).toHaveLength(3);
  expect(new Set(screenshotHashes).size).toBe(3);
});
