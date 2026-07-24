import { expect, test, type Page } from "@playwright/test";
import { createHash } from "node:crypto";
import { mkdirSync, writeFileSync } from "node:fs";
import path from "node:path";

import { installMockBridge, TEST_IDENTITIES } from "../helpers/bridge";
import { openSettings } from "../helpers/settings";
import { waitForAnimations } from "../helpers/animations";

const OUT_DIR = path.resolve("test-results/wallet-screenshots");
const CHANNEL = "random";
const KIND_PAYMENT_RECEIPT = 40010;
const VIEWER = "deadbeef".repeat(8);
const ALICE = TEST_IDENTITIES.alice.pubkey;
const AGENT_PUBKEY = "a1".repeat(32);
const REQUEST_ID_NORMAL = "11".repeat(32);
const REQUEST_ID_EXPIRED = "22".repeat(32);
const REQUEST_ID_OWN = "33".repeat(32);
const REQUEST_ID_AGENT = "44".repeat(32);
const REQUEST_ID_PAID = "55".repeat(32);
const RECEIPT_ID = "66".repeat(32);

const FIXED_NOW_MS = Date.UTC(2026, 0, 15, 12, 0, 0);
const FIXED_NOW_UNIX = Math.floor(FIXED_NOW_MS / 1000);

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

async function emitPaymentRequest(
  page: Page,
  input: {
    id: string;
    pubkey: string;
    amountMsat: number;
    memo: string;
    expiryUnix: number;
    bolt11?: string;
  },
) {
  await page.evaluate(
    ({ channelName, ...rest }) => {
      window.__BUZZ_E2E_EMIT_MOCK_MESSAGE__?.({
        channelName,
        content: rest.memo,
        pubkey: rest.pubkey,
        kind: 40009,
        id: rest.id,
        createdAt: Math.floor(Date.now() / 1000) - 10,
        extraTags: [
          ["amount", String(rest.amountMsat)],
          ["memo", rest.memo],
          ["expiry", String(rest.expiryUnix)],
          ["p", rest.pubkey],
          ["bolt11", rest.bolt11 ?? "lnbc1mock"],
        ],
      });
    },
    { channelName: CHANNEL, ...input },
  );
}

async function shot(locator: ReturnType<Page["locator"]>, name: string) {
  await waitForAnimations(locator.page());
  await expect(locator).toBeVisible();
  const file = path.join(OUT_DIR, `${name}.png`);
  await locator.screenshot({ path: file });
  return file;
}

test.beforeEach(async ({ page }) => {
  mkdirSync(OUT_DIR, { recursive: true });
  await page.addInitScript(() => {
    // Ensure settings / wallet UI starts from a clean community cache.
  });
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
    walletConfirmOutcome: {
      status: "settled",
      preimage: "ab".repeat(32),
    },
    managedAgents: [
      {
        pubkey: AGENT_PUBKEY,
        name: "PayBot",
        status: "running",
        channelNames: [CHANNEL],
      },
    ],
    searchProfiles: [
      {
        pubkey: AGENT_PUBKEY,
        displayName: "PayBot",
        isAgent: true,
        ownerPubkey: VIEWER,
      },
      {
        pubkey: ALICE,
        displayName: "alice",
        isAgent: false,
      },
    ],
  });
});

test("wallet settings link status receive QR and send confirm", async ({
  page,
}) => {
  await page.goto("/");
  await openSettings(page, "wallet");
  const linked = page.getByTestId("wallet-linked-status");
  await expect(linked).toBeVisible();
  await expect(page.getByTestId("wallet-balance")).toContainText("21,000 sats");
  await shot(linked, "settings-linked");

  await page.getByTestId("wallet-receive-amount").fill("210");
  await page.getByTestId("wallet-receive-memo").fill("coffee");
  await page.getByTestId("wallet-receive-submit").click();
  const qr = page.getByTestId("wallet-receive-invoice");
  await expect(page.getByTestId("wallet-receive-qr")).toBeVisible();
  await shot(qr, "receive-QR");

  await page.getByTestId("wallet-send-bolt11").fill("lnbc210n1ptestinvoice");
  await page.getByTestId("wallet-send-amount").fill("210");
  await page.getByTestId("wallet-send-prepare").click();
  const dialog = page.getByTestId("wallet-confirm-dialog");
  await expect(dialog).toBeVisible();
  await shot(dialog, "quote-dialog");
  await page.getByTestId("wallet-confirm-submit").click();
  await expect(page.getByTestId("wallet-confirm-settled")).toBeVisible();
  await page.getByTestId("wallet-confirm-done").click();

  await page.getByTestId("wallet-unlink").click();
  await page.getByTestId("wallet-unlink-confirm-submit").click();
  await expect(page.getByTestId("wallet-link-uri")).toBeVisible();
});

test("pay card states + receipt-before-request", async ({ page }) => {
  await page.clock.install({ time: FIXED_NOW_MS });

  await page.goto("/");
  await page.getByTestId(`channel-${CHANNEL}`).click();
  await waitForMockLiveSubscription(page, CHANNEL);

  // Receipt BEFORE its request (order-independence).
  await page.evaluate(
    ({ channelName, receiptId, requestId, viewer, kind, createdAt }) => {
      window.__BUZZ_E2E_EMIT_MOCK_MESSAGE__?.({
        channelName,
        content: "",
        pubkey: viewer,
        kind,
        id: receiptId,
        createdAt,
        extraTags: [
          ["e", requestId],
          ["payment_hash", "cd".repeat(32)],
          ["preimage", "ab".repeat(32)],
          ["amount", "500000"],
        ],
      });
    },
    {
      channelName: CHANNEL,
      receiptId: RECEIPT_ID,
      requestId: REQUEST_ID_PAID,
      viewer: VIEWER,
      kind: KIND_PAYMENT_RECEIPT,
      createdAt: FIXED_NOW_UNIX - 5,
    },
  );

  await emitPaymentRequest(page, {
    id: REQUEST_ID_PAID,
    pubkey: ALICE,
    amountMsat: 500_000,
    memo: "already paid lunch",
    expiryUnix: FIXED_NOW_UNIX + 3600,
  });

  await emitPaymentRequest(page, {
    id: REQUEST_ID_NORMAL,
    pubkey: ALICE,
    amountMsat: 210_000,
    memo: "tip jar",
    expiryUnix: FIXED_NOW_UNIX + 3600,
  });

  await emitPaymentRequest(page, {
    id: REQUEST_ID_EXPIRED,
    pubkey: ALICE,
    amountMsat: 100_000,
    memo: "too late",
    expiryUnix: FIXED_NOW_UNIX - 60,
  });

  await emitPaymentRequest(page, {
    id: REQUEST_ID_OWN,
    pubkey: VIEWER,
    amountMsat: 50_000,
    memo: "my own request",
    expiryUnix: FIXED_NOW_UNIX + 3600,
  });

  await emitPaymentRequest(page, {
    id: REQUEST_ID_AGENT,
    pubkey: AGENT_PUBKEY,
    amountMsat: 1_000_000,
    memo: "agent needs sats",
    expiryUnix: FIXED_NOW_UNIX + 3600,
  });

  const cards = page.getByTestId("payment-request-card");
  await expect(cards).toHaveCount(5);

  const paid = page.locator('[data-pay-card-state="paid"]');
  await expect(paid).toHaveCount(1);
  await expect(paid.getByTestId("payment-request-status")).toHaveText("Paid ✓");
  await expect(paid.getByTestId("payment-request-pay")).toHaveCount(0);
  await shot(paid, "pay-card-paid");

  const normal = page.locator('[data-pay-card-state="payable"]').filter({
    hasText: "tip jar",
  });
  await expect(normal.getByTestId("payment-request-pay")).toBeVisible();
  await shot(normal, "pay-card-normal");

  const expired = page.locator('[data-pay-card-state="expired"]');
  await expect(expired.getByTestId("payment-request-status")).toHaveText(
    "Expired",
  );
  await expect(expired.getByTestId("payment-request-pay")).toHaveCount(0);
  await shot(expired, "pay-card-expired");

  const own = page.locator('[data-pay-card-state="own_request"]');
  await expect(own.getByTestId("payment-request-pay")).toHaveCount(0);
  await shot(own, "pay-card-own");

  const agentCard = page.locator('[data-pay-card-state="payable"]').filter({
    hasText: "agent needs sats",
  });
  await expect(agentCard).toBeVisible();
  // Agent badge with owner attribution lives on the message header (reused).
  const agentOwner = page.getByTestId("message-agent-owner").first();
  await expect(agentOwner).toBeVisible();
  // Capture card + nearby owner badge together via a tight ancestor clip.
  const agentBundle = agentCard.locator(
    "xpath=ancestor::div[contains(@class,'relative')][1]",
  );
  await shot(agentBundle, "pay-card-agent");

  // Quote-confirm from normal pay card.
  await normal.getByTestId("payment-request-pay").click();
  const dialog = page.getByTestId("wallet-confirm-dialog");
  await expect(dialog).toBeVisible();
  await page.getByTestId("wallet-confirm-submit").click();
  await expect(page.getByTestId("wallet-confirm-settled")).toBeVisible();
});

test.afterAll(async () => {
  const { readdirSync, readFileSync } = await import("node:fs");
  const files = readdirSync(OUT_DIR)
    .filter((f) => f.endsWith(".png"))
    .sort();
  const lines: string[] = [];
  const hashes = new Set<string>();
  for (const file of files) {
    const buf = readFileSync(path.join(OUT_DIR, file));
    const hash = createHash("sha256").update(buf).digest("hex");
    if (hashes.has(hash)) {
      throw new Error(`Duplicate screenshot hash for ${file}: ${hash}`);
    }
    hashes.add(hash);
    lines.push(`${hash}  ${file}`);
  }
  writeFileSync(path.join(OUT_DIR, "hashes.txt"), `${lines.join("\n")}\n`);
  writeFileSync(
    path.join(OUT_DIR, "body.md"),
    `### Settings linked
Linked wallet status with balance in sats.

{{settings-linked}}

### Receive QR
Invoice QR after interactive receive.

{{receive-QR}}

### Pay card — normal
Pay button visible for another user's request.

{{pay-card-normal}}

### Pay card — expired
Expired request, no Pay button (\`page.clock\`).

{{pay-card-expired}}

### Pay card — own
Own request, no Pay button.

{{pay-card-own}}

### Pay card — agent
Agent-authored request with owner attribution badge.

{{pay-card-agent}}

### Pay card — paid
Receipt-before-request still shows Paid ✓.

{{pay-card-paid}}

### Quote dialog
Mandatory confirm dialog before spend.

{{quote-dialog}}
`,
  );
});
