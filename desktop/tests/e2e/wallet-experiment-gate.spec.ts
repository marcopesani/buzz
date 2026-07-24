import { expect, test, type Page } from "@playwright/test";
import { createHash } from "node:crypto";
import { mkdirSync, readFileSync } from "node:fs";
import path from "node:path";

import { installMockBridge, TEST_IDENTITIES } from "../helpers/bridge";
import { openSettings } from "../helpers/settings";
import { waitForAnimations } from "../helpers/animations";

const OUT_DIR = path.resolve("test-results/wallet-gate-screenshots");
const CHANNEL = "random";
const ALICE = TEST_IDENTITIES.alice.pubkey;
const REQUEST_ID = "aa".repeat(32);

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

async function emitPaymentRequest(page: Page) {
  await page.evaluate(
    ({ channelName, id, pubkey }) => {
      const now = Math.floor(Date.now() / 1000);
      window.__BUZZ_E2E_EMIT_MOCK_MESSAGE__?.({
        channelName,
        content: "coffee money",
        pubkey,
        kind: 40009,
        id,
        createdAt: now - 10,
        extraTags: [
          ["amount", "210000"],
          ["memo", "coffee money"],
          ["expiry", String(now + 3600)],
          ["p", pubkey],
          ["bolt11", "lnbc1mock"],
        ],
      });
    },
    {
      channelName: CHANNEL,
      id: REQUEST_ID,
      pubkey: ALICE,
    },
  );
}

async function openChannel(page: Page) {
  await page.goto("/");
  await page.getByTestId(`channel-${CHANNEL}`).click();
  await waitForMockLiveSubscription(page, CHANNEL);
}

function sha256File(filePath: string): string {
  return createHash("sha256").update(readFileSync(filePath)).digest("hex");
}

async function shotComposer(page: Page, name: string): Promise<string> {
  const composer = page.locator("footer").filter({
    has: page.getByTestId("message-input-scroll"),
  });
  await waitForAnimations(page);
  await expect(composer).toBeVisible();
  const file = path.join(OUT_DIR, `${name}.png`);
  await composer.screenshot({ path: file });
  return file;
}

test.describe.configure({ mode: "serial" });

test.beforeEach(async () => {
  mkdirSync(OUT_DIR, { recursive: true });
});

const screenshotHashes: string[] = [];

test("experiment off hides request button, pay card, and settings wallet", async ({
  page,
}) => {
  // Do not install a fake clock here — it races mock-bridge init and surfaces
  // "Cannot read properties of undefined (reading 'invoke')".
  await installMockBridge(page, undefined, { seedPreviewFeatures: false });

  await openChannel(page);
  await emitPaymentRequest(page);

  await expect(page.getByTestId("request-payment-button")).toHaveCount(0);
  await expect(page.getByTestId("payment-request-card")).toHaveCount(0);

  const composerShot = await shotComposer(page, "01-experiment-off-composer");
  screenshotHashes.push(sha256File(composerShot));

  await openSettings(page);
  await expect(page.getByTestId("settings-nav-wallet")).toHaveCount(0);
  await expect(page.getByTestId("settings-nav-experimental")).toBeVisible();
  await page.getByTestId("settings-nav-experimental").click();
  await expect(page.locator("#feature-toggle-wallet-label")).toHaveText(
    "Lightning Wallet",
  );
});

test("experiment on + unlinked: soft-disabled button deep-links to wallet settings", async ({
  page,
}) => {
  await installMockBridge(page, {
    walletStatus: {
      linked: false,
    },
  });

  await openChannel(page);

  const button = page.getByTestId("request-payment-button");
  await expect(button).toBeVisible();
  await expect(button).toHaveAttribute("aria-disabled", "true");
  // Soft-disabled: aria-disabled only — native disabled would kill click/tooltip.
  await expect(button).not.toHaveAttribute("disabled");

  await button.hover();
  await expect(page.getByRole("tooltip")).toContainText(
    "Connect a wallet to request payment",
  );

  // Playwright treats aria-disabled as non-actionable; force matches real UI clicks.
  await button.click({ force: true });
  await expect(page.getByTestId("settings-view")).toBeVisible();
  await expect(page.getByTestId("settings-wallet")).toBeVisible();

  await waitForAnimations(page);
  const settingsShot = path.join(OUT_DIR, "02-unlinked-settings-redirect.png");
  await page.getByTestId("settings-wallet").screenshot({ path: settingsShot });
  screenshotHashes.push(sha256File(settingsShot));
});

test("experiment on + linked: request button is enabled", async ({ page }) => {
  await installMockBridge(page, {
    walletStatus: {
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
    },
  });

  await openChannel(page);

  const button = page.getByTestId("request-payment-button");
  await expect(button).toBeVisible();
  await expect(button).not.toHaveAttribute("aria-disabled", "true");
  await expect(button).not.toHaveAttribute("disabled");

  await button.hover();
  await expect(page.getByRole("tooltip")).toContainText("Request payment");

  const composerShot = await shotComposer(page, "03-linked-enabled-composer");
  screenshotHashes.push(sha256File(composerShot));

  expect(screenshotHashes).toHaveLength(3);
  expect(new Set(screenshotHashes).size).toBe(3);
});
