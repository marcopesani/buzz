import { expect, test, type Page } from "@playwright/test";
import { createHash } from "node:crypto";
import { mkdirSync, readFileSync } from "node:fs";
import path from "node:path";

import { installMockBridge } from "../helpers/bridge";
import { waitForAnimations } from "../helpers/animations";

const OUT_DIR = path.resolve("test-results/wallet-payment-events-screenshots");
const CHANNEL = "random";
const MOCK_BOLT11 =
  "lnbc210n1pmockinvoice000000000000000000000000000000000000000000000000000";

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

async function seedChannelMessage(page: Page) {
  await page.evaluate((channelName) => {
    window.__BUZZ_E2E_EMIT_MOCK_MESSAGE__?.({
      channelName,
      content: "seed for payment events e2e",
      kind: 9,
    });
  }, CHANNEL);
  await expect(page.getByText("seed for payment events e2e")).toBeVisible({
    timeout: 10_000,
  });
}

async function openChannel(page: Page) {
  await page.goto("/");
  await page.getByTestId(`channel-${CHANNEL}`).click();
  await waitForMockLiveSubscription(page, CHANNEL);
  await seedChannelMessage(page);
}

async function publishRequest(page: Page, amountSats: string) {
  await page.getByTestId("request-payment-button").click();
  await expect(page.getByTestId("request-payment-dialog")).toBeVisible();
  await page.getByTestId("request-payment-amount").fill(amountSats);
  await page.getByTestId("request-payment-memo").fill("verified receive");
  await page.getByTestId("request-payment-submit").click();
  await expect(page.getByTestId("payment-request-card")).toBeVisible({
    timeout: 10_000,
  });
}

async function publishedRequestId(page: Page): Promise<string> {
  // Timeline row id is what the decorative receipt aux-join keys on.
  const fromCard = await page
    .getByTestId("payment-request-card")
    .locator("xpath=ancestor::*[@data-message-id][1]")
    .getAttribute("data-message-id");
  expect(fromCard, "published 40009 should have an id").toBeTruthy();

  // Registry must track the same id (publish-success hook).
  const fromRegistry = await page.evaluate(() => {
    const raw = window.localStorage.getItem(
      "buzz-wallet-pending-payment-requests.v1:e2e-default-community",
    );
    if (!raw) return null;
    try {
      const parsed = JSON.parse(raw) as {
        pending?: Array<{ requestEventId?: string }>;
        verifiedIds?: string[];
      };
      const pending = parsed.pending ?? [];
      const verified = parsed.verifiedIds ?? [];
      return pending[pending.length - 1]?.requestEventId ?? verified[0] ?? null;
    } catch {
      return null;
    }
  });
  expect(
    fromRegistry,
    "pending registry should track the published 40009",
  ).toBe((fromCard as string).toLowerCase());
  return fromCard as string;
}

async function emitReceipt(
  page: Page,
  requestEventId: string,
  receiptId: string,
) {
  const event = await page.evaluate(
    ({ channelName, requestEventId: reqId, receiptId: id }) => {
      return (
        window.__BUZZ_E2E_EMIT_MOCK_MESSAGE__?.({
          channelName,
          content: "",
          kind: 40010,
          id,
          createdAt: Math.floor(Date.now() / 1000),
          extraTags: [
            ["e", reqId],
            ["payment_hash", "cd".repeat(32)],
            ["preimage", "ab".repeat(32)],
            ["amount", "500000"],
          ],
        }) ?? null
      );
    },
    {
      channelName: CHANNEL,
      requestEventId,
      receiptId,
    },
  );
  expect(event, "mock emit should return the 40010 event").toBeTruthy();
}

async function setCheckIncoming(
  page: Page,
  outcome:
    | { status: "paid" }
    | { status: "unpaid" }
    | { status: "unconfirmable" },
) {
  await page.evaluate((next) => {
    (
      window as Window & {
        __BUZZ_E2E_SET_WALLET_CHECK_INCOMING__?: (o: typeof next) => void;
      }
    ).__BUZZ_E2E_SET_WALLET_CHECK_INCOMING__?.(next);
  }, outcome);
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

function receivedToast(page: Page) {
  return page
    .locator("[data-sonner-toast]")
    .filter({ hasText: "Payment received" });
}

function sha256File(filePath: string): string {
  return createHash("sha256").update(readFileSync(filePath)).digest("hex");
}

test.describe.configure({ mode: "serial" });
test.describe.configure({ timeout: 60_000 });

test.beforeEach(async () => {
  mkdirSync(OUT_DIR, { recursive: true });
});

const screenshotHashes: string[] = [];

test("receipt + check_incoming=paid shows verified toast; decorative Paid still works", async ({
  page,
}) => {
  await installMockBridge(page, {
    walletStatus: LINKED_WALLET,
    walletReceiveBolt11: MOCK_BOLT11,
    walletCheckIncomingOutcome: { status: "paid" },
  });

  await openChannel(page);
  await publishRequest(page, "500");

  const requestId = await publishedRequestId(page);
  await emitReceipt(page, requestId, "a1".repeat(32));

  await expect(receivedToast(page)).toBeVisible({ timeout: 10_000 });
  await expect(receivedToast(page)).toContainText("500 sats");

  await waitForAnimations(page);
  const toastShot = path.join(OUT_DIR, "01-verified-toast.png");
  await receivedToast(page).screenshot({ path: toastShot });
  screenshotHashes.push(sha256File(toastShot));

  // Decorative Paid ✓ is V3 aux-join — remount if the deferred timeline lags
  // (same fallback as wallet-receipt.spec.ts).
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
});

test("receipt alone never toasts; poll after paid flips once", async ({
  page,
}) => {
  await installMockBridge(page, {
    walletStatus: LINKED_WALLET,
    walletReceiveBolt11: MOCK_BOLT11,
    walletCheckIncomingOutcome: { status: "unpaid" },
  });

  await openChannel(page);
  await publishRequest(page, "210");

  const requestId = await publishedRequestId(page);
  await emitReceipt(page, requestId, "a2".repeat(32));

  // Decorative Paid may appear from the receipt join — verification must not.
  const paid = page.locator('[data-pay-card-state="paid"]');
  try {
    await expect(paid).toBeVisible({ timeout: 3_000 });
  } catch {
    await page.getByTestId("channel-general").click();
    await page.getByTestId(`channel-${CHANNEL}`).click();
    await waitForMockLiveSubscription(page, CHANNEL);
    await expect(paid).toBeVisible({ timeout: 10_000 });
  }
  await expect(receivedToast(page)).toHaveCount(0);

  await waitForAnimations(page);
  // Capture toast host corner + paid card so the PNG proves toast ABSENCE
  // (card-only crops cannot show a missing sonner toast).
  const noToastShot = path.join(OUT_DIR, "02-no-toast-on-unverified.png");
  const cardBox = await paid.boundingBox();
  expect(cardBox, "paid card should have a bounding box").toBeTruthy();
  const clipX = Math.max(0, Math.floor((cardBox?.x ?? 400) - 24));
  const clipW = Math.min(1280 - clipX, Math.ceil((cardBox?.width ?? 400) + 48));
  const clipH = Math.min(
    720,
    Math.ceil((cardBox?.y ?? 200) + (cardBox?.height ?? 160) + 24),
  );
  await page.screenshot({
    path: noToastShot,
    clip: { x: clipX, y: 0, width: clipW, height: clipH },
  });
  screenshotHashes.push(sha256File(noToastShot));

  await setCheckIncoming(page, { status: "paid" });
  await reconcileReceipts(page);

  await expect(receivedToast(page)).toHaveCount(1, { timeout: 10_000 });
  await expect(receivedToast(page)).toContainText("210 sats");

  // Second poll must not duplicate.
  await reconcileReceipts(page);
  await expect(receivedToast(page)).toHaveCount(1);

  await waitForAnimations(page);
  const afterPollShot = path.join(OUT_DIR, "03-toast-after-poll.png");
  await receivedToast(page).screenshot({ path: afterPollShot });
  screenshotHashes.push(sha256File(afterPollShot));
});

test("duplicate receipt injections still produce exactly one verified toast", async ({
  page,
}) => {
  await installMockBridge(page, {
    walletStatus: LINKED_WALLET,
    walletReceiveBolt11: MOCK_BOLT11,
    walletCheckIncomingOutcome: { status: "paid" },
  });

  await openChannel(page);
  await publishRequest(page, "100");

  const requestId = await publishedRequestId(page);
  await emitReceipt(page, requestId, "a3".repeat(32));
  await emitReceipt(page, requestId, "a4".repeat(32));

  await expect(receivedToast(page)).toHaveCount(1, { timeout: 10_000 });
  await expect(receivedToast(page)).toContainText("100 sats");
});

test.afterAll(() => {
  const unique = new Set(screenshotHashes);
  expect(
    unique.size,
    `screenshot hashes must be distinct, got ${screenshotHashes.join(", ")}`,
  ).toBe(screenshotHashes.length);
});
