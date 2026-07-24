import { expect, test, type Page } from "@playwright/test";
import { createHash } from "node:crypto";
import { mkdirSync, readFileSync } from "node:fs";
import path from "node:path";

import { installMockBridge } from "../helpers/bridge";
import { waitForAnimations } from "../helpers/animations";

const OUT_DIR = path.resolve("test-results/wallet-request-screenshots");
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

async function openChannel(page: Page) {
  await page.goto("/");
  await page.getByTestId(`channel-${CHANNEL}`).click();
  await waitForMockLiveSubscription(page, CHANNEL);
}

async function openRequestDialog(page: Page) {
  const button = page.getByTestId("request-payment-button");
  await expect(button).toBeVisible();
  await expect(button).not.toHaveAttribute("aria-disabled", "true");
  await button.click();
  await expect(page.getByTestId("request-payment-dialog")).toBeVisible();
}

function sha256File(filePath: string): string {
  return createHash("sha256").update(readFileSync(filePath)).digest("hex");
}

async function countWalletReceive(page: Page): Promise<number> {
  return page.evaluate(() => {
    const commands =
      (
        window as Window & {
          __BUZZ_E2E_COMMANDS__?: string[];
        }
      ).__BUZZ_E2E_COMMANDS__ ?? [];
    return commands.filter((c) => c === "wallet_receive").length;
  });
}

test.describe.configure({ mode: "serial" });

test.beforeEach(async () => {
  mkdirSync(OUT_DIR, { recursive: true });
});

const screenshotHashes: string[] = [];

test("request dialog mints once and posts a 40009 pay card", async ({
  page,
}) => {
  await installMockBridge(page, {
    walletStatus: LINKED_WALLET,
    walletReceiveBolt11: MOCK_BOLT11,
  });

  await openChannel(page);
  await openRequestDialog(page);

  await page.getByTestId("request-payment-amount").fill("500");
  await page.getByTestId("request-payment-memo").fill("coffee money");

  await waitForAnimations(page);
  const dialogShot = path.join(OUT_DIR, "01-dialog-open.png");
  await page.getByTestId("request-payment-dialog").screenshot({
    path: dialogShot,
  });
  screenshotHashes.push(sha256File(dialogShot));

  await page.getByTestId("request-payment-submit").click();

  await expect(page.getByTestId("payment-request-card")).toBeVisible({
    timeout: 10_000,
  });
  await expect(page.getByTestId("payment-request-amount")).toContainText("500");
  await expect(page.getByTestId("payment-request-memo")).toHaveText(
    "coffee money",
  );
  await expect(page.getByTestId("payment-request-status")).toHaveText(
    "Your request",
  );

  expect(await countWalletReceive(page)).toBe(1);

  // Contract: signed 40009 must carry self-payee `p` (CLI/agent from_tags).
  // Load-bearing proof is the Tauri Rust sign_event test; this asserts the
  // tags we hand to sign_event include `p` (mock JS signer does not scrub).
  const signedPayee = await page.evaluate(() => {
    const signed =
      (
        window as Window & {
          __BUZZ_E2E_SIGNED_EVENTS__?: Array<{
            kind: number;
            tags: string[][];
          }>;
        }
      ).__BUZZ_E2E_SIGNED_EVENTS__ ?? [];
    const req = [...signed].reverse().find((e) => e.kind === 40009);
    return req?.tags.find((t) => t[0] === "p")?.[1] ?? null;
  });
  expect(signedPayee).toBeTruthy();
  expect(signedPayee).toMatch(/^[0-9a-f]{64}$/);

  await waitForAnimations(page);
  const cardShot = path.join(OUT_DIR, "02-card-in-timeline.png");
  await page.getByTestId("payment-request-card").screenshot({
    path: cardShot,
  });
  screenshotHashes.push(sha256File(cardShot));
});

test("publish failure surfaces error; retry reuses the same bolt11", async ({
  page,
}) => {
  await installMockBridge(page, {
    walletStatus: LINKED_WALLET,
    walletReceiveBolt11: MOCK_BOLT11,
    paymentRequestPublishErrors: ["mock publish rejected"],
  });

  await openChannel(page);
  await openRequestDialog(page);

  await page.getByTestId("request-payment-amount").fill("210");
  await page.getByTestId("request-payment-memo").fill("retry me");
  await page.getByTestId("request-payment-submit").click();

  await expect(page.getByTestId("request-payment-error")).toContainText(
    "mock publish rejected",
  );
  await expect(page.getByTestId("request-payment-retry")).toBeVisible();
  await expect(page.getByTestId("request-payment-retry-bolt11")).toContainText(
    MOCK_BOLT11.slice(0, 24),
  );

  expect(await countWalletReceive(page)).toBe(1);

  await waitForAnimations(page);
  const errorShot = path.join(OUT_DIR, "03-error-retry.png");
  await page.getByTestId("request-payment-dialog").screenshot({
    path: errorShot,
  });
  screenshotHashes.push(sha256File(errorShot));

  await page.getByTestId("request-payment-retry").click();

  await expect(page.getByTestId("payment-request-card")).toBeVisible({
    timeout: 10_000,
  });
  await expect(page.getByTestId("payment-request-amount")).toContainText("210");

  // Still exactly one mint for this dialog session.
  expect(await countWalletReceive(page)).toBe(1);

  expect(screenshotHashes).toHaveLength(3);
  expect(new Set(screenshotHashes).size).toBe(3);
});

test("expiry longer than invoice is clamped and shown", async ({ page }) => {
  await installMockBridge(page, {
    walletStatus: LINKED_WALLET,
    walletReceiveBolt11: MOCK_BOLT11,
    // 10-minute invoice — 24h preset must clamp.
    walletReceiveExpiresInSecs: 600,
  });

  await openChannel(page);
  await openRequestDialog(page);

  await page.getByTestId("request-payment-amount").fill("21");
  await page.getByTestId("request-payment-expiry").selectOption("24h");
  await page.getByTestId("request-payment-submit").click();

  await expect(page.getByTestId("request-payment-expiry-clamped")).toBeVisible({
    timeout: 10_000,
  });
  await expect(page.getByTestId("request-payment-done")).toBeVisible();

  // Card still lands with the request; clamp notice stays on the dialog.
  await expect(page.getByTestId("payment-request-card")).toBeVisible();
  await expect(page.getByTestId("payment-request-amount")).toContainText("21");

  const signedExpiry = await page.evaluate(() => {
    const signed =
      (
        window as Window & {
          __BUZZ_E2E_SIGNED_EVENTS__?: Array<{
            kind: number;
            tags: string[][];
          }>;
        }
      ).__BUZZ_E2E_SIGNED_EVENTS__ ?? [];
    const req = [...signed].reverse().find((e) => e.kind === 40009);
    return req?.tags.find((t) => t[0] === "expiry")?.[1] ?? null;
  });
  expect(signedExpiry).toBeTruthy();
  const expiryUnix = Number(signedExpiry);
  const now = Math.floor(Date.now() / 1000);
  // Clamped to ~now+600, not now+86400.
  expect(expiryUnix).toBeGreaterThan(now + 500);
  expect(expiryUnix).toBeLessThan(now + 700);
});
