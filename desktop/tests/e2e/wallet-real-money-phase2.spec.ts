/**
 * Real-money phase-2 wallet proof — Chat Payment Requests + verified toast.
 *
 * REAL WalletRuntime → NWC → a REAL operator wallet. No mock wallet on the
 * money path. Opt-in local artifact (skipped in CI):
 *   BUZZ_NWC_URI=nostr+walletconnect://… \
 *   BUZZ_REAL_WALLET_E2E_PHASE2=1 pnpm exec playwright test \
 *     tests/e2e/wallet-real-money-phase2.spec.ts
 *
 * Money mechanics: every payee invoice is minted FROM the linked wallet
 * itself (harness real mode), so each settle is a genuine self-payment —
 * real preimage, net-zero cost. Amount is 1 sat per leg (max three).
 *
 * Honesty: wallet path is real end to end (link, balance, receive, pay,
 * check_incoming, preimage). 40009/40010 timeline fan-out is bridge-fed
 * (relay ingest proven separately). Own-request vs payer roles are split
 * in one serial test because you cannot pay your own 40009 through the UI.
 */
import { expect, test, type Page } from "@playwright/test";
import { createHash } from "node:crypto";
import {
  mkdirSync,
  writeFileSync,
  existsSync,
  createWriteStream,
  readdirSync,
  readFileSync,
  type WriteStream,
} from "node:fs";
import path from "node:path";
import {
  spawn,
  spawnSync,
  type ChildProcessWithoutNullStreams,
} from "node:child_process";
import { fileURLToPath } from "node:url";

import { installMockBridge, TEST_IDENTITIES } from "../helpers/bridge";
import { openSettings } from "../helpers/settings";
import { waitForAnimations } from "../helpers/animations";

// Skip the whole file (including beforeAll harness build) unless explicitly enabled.
// Own flag so phase-1 wallet-real-money.spec.ts does not co-fire.
test.skip(
  !process.env.BUZZ_REAL_WALLET_E2E_PHASE2 || !process.env.BUZZ_NWC_URI,
  "real wallet phase-2 — set BUZZ_REAL_WALLET_E2E_PHASE2=1 and BUZZ_NWC_URI to run locally",
);

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(__dirname, "../../..");
const OUT_DIR = path.resolve("test-results/wallet-real-phase2-screenshots");
const HARNESS_URL = "http://127.0.0.1:4189";
const HARNESS_LOG = "/tmp/proofs/wallet-real-phase2-harness.log";
const CHANNEL = "random";
const ALICE = TEST_IDENTITIES.alice.pubkey;
const ALICE_REQUEST_ID = "d4".repeat(32);
const STAGE_C_RECEIPT_ID = "e5".repeat(32);
const PAY_AMOUNT_MSAT = 1_000;
const PAY_AMOUNT_SATS = 1;

let harness: ChildProcessWithoutNullStreams | null = null;
let harnessLogStream: WriteStream | null = null;

async function waitForHarnessReady(timeoutMs = 60_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const res = await fetch(`${HARNESS_URL}/uri`);
      if (res.ok) {
        const body = (await res.json()) as { uri?: string };
        if (body.uri?.startsWith("nostr+walletconnect://")) {
          return body.uri;
        }
      }
    } catch {
      // not ready
    }
    await new Promise((r) => setTimeout(r, 250));
  }
  throw new Error("wallet_e2e_harness did not become ready");
}

async function buildHarness() {
  const result = spawnSync(
    "cargo",
    [
      "build",
      "--manifest-path",
      path.join(REPO_ROOT, "desktop/src-tauri/Cargo.toml"),
      "--bin",
      "wallet_e2e_harness",
    ],
    {
      cwd: REPO_ROOT,
      encoding: "utf8",
      env: { ...process.env },
    },
  );
  if (result.status !== 0) {
    throw new Error(
      `cargo build wallet_e2e_harness failed:\n${result.stdout}\n${result.stderr}`,
    );
  }
}

function harnessBinaryPath(): string {
  const debug = path.join(
    REPO_ROOT,
    "desktop/src-tauri/target/debug/wallet_e2e_harness",
  );
  const workspace = path.join(REPO_ROOT, "target/debug/wallet_e2e_harness");
  if (existsSync(debug)) return debug;
  if (existsSync(workspace)) return workspace;
  throw new Error("wallet_e2e_harness binary not found after build");
}

async function startHarness() {
  mkdirSync(path.dirname(HARNESS_LOG), { recursive: true });
  harnessLogStream = createWriteStream(HARNESS_LOG, { flags: "w" });
  await buildHarness();
  const bin = harnessBinaryPath();
  harness = spawn(bin, [], {
    env: {
      ...process.env,
      BUZZ_HARNESS_REAL_WALLET: "1",
      RUST_LOG: "info,buzz_wallet=info,nwc=info",
    },
    stdio: ["ignore", "pipe", "pipe"],
  });
  const write = (chunk: Buffer) => {
    harnessLogStream?.write(chunk);
  };
  harness.stdout.on("data", write);
  harness.stderr.on("data", write);
  harness.on("exit", (code, signal) => {
    harnessLogStream?.write(`\n[exit code=${code} signal=${signal}]\n`);
  });
  return waitForHarnessReady();
}

function stopHarness() {
  if (harness && !harness.killed) {
    harness.kill("SIGTERM");
    harness = null;
  }
  harnessLogStream?.end();
  harnessLogStream = null;
}

async function fullWindowShot(page: Page, name: string) {
  await waitForAnimations(page);
  const file = path.join(OUT_DIR, `${name}.png`);
  await page.screenshot({ path: file });
  return file;
}

function sha256Hex(hex: string): string {
  const buf = Buffer.from(hex, "hex");
  return createHash("sha256").update(buf).digest("hex");
}

/** Parse "27,532 sats" → 27532. */
function parseSats(text: string): number {
  const match = text.replace(/,/g, "").match(/(\d+)\s*sats/);
  if (!match) throw new Error(`no sats amount in: ${text}`);
  return Number(match[1]);
}

async function waitForMockLiveSubscription(page: Page, channelName: string) {
  await expect
    .poll(async () =>
      page.evaluate(
        (name) =>
          window.__BUZZ_E2E_HAS_MOCK_LIVE_SUBSCRIPTION__?.({
            channelName: name,
            kind: 40009,
          }) ?? false,
        channelName,
      ),
    )
    .toBe(true);
}

async function seedChannelMessage(page: Page) {
  await page.evaluate(
    ({ channelName, pubkey }) => {
      window.__BUZZ_E2E_EMIT_MOCK_MESSAGE__?.({
        channelName,
        content: "warmup before phase-2 pay cards",
        pubkey,
        kind: 9,
        id: "f6".repeat(32),
        createdAt: Math.floor(Date.now() / 1000) - 30,
      });
    },
    { channelName: CHANNEL, pubkey: ALICE },
  );
  await expect(page.getByText("warmup before phase-2 pay cards")).toBeVisible({
    timeout: 15_000,
  });
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

async function stageARegistryEntry(page: Page): Promise<{
  requestEventId: string;
  bolt11: string;
  paymentHash: string;
  amountMsat: number;
}> {
  return page.evaluate(() => {
    const raw = window.localStorage.getItem(
      "buzz-wallet-pending-payment-requests.v1:e2e-default-community",
    );
    if (!raw) throw new Error("pending payment request registry missing");
    const parsed = JSON.parse(raw) as {
      pending?: Array<{
        requestEventId?: string;
        bolt11?: string;
        paymentHash?: string;
        amountMsat?: number;
      }>;
    };
    const entry = parsed.pending?.[0];
    if (
      !entry?.requestEventId ||
      !entry.bolt11 ||
      !entry.paymentHash ||
      entry.amountMsat == null
    ) {
      throw new Error("stage-A registry entry incomplete");
    }
    return {
      requestEventId: entry.requestEventId,
      bolt11: entry.bolt11,
      paymentHash: entry.paymentHash,
      amountMsat: entry.amountMsat,
    };
  });
}

async function harnessInvoke(
  cmd: string,
  args: Record<string, unknown>,
): Promise<unknown> {
  const res = await fetch(`${HARNESS_URL}/invoke`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ cmd, args }),
  });
  const body = (await res.json()) as {
    ok?: boolean;
    result?: unknown;
    error?: string;
  };
  if (!res.ok || body.ok === false) {
    throw new Error(body.error ?? `harness invoke failed for ${cmd}`);
  }
  return body.result ?? null;
}

function receivedToast(page: Page) {
  return page
    .locator("[data-sonner-toast]")
    .filter({ hasText: "Payment received" });
}

test.describe.configure({ mode: "serial", timeout: 240_000 });

test.beforeAll(async () => {
  // Harness cargo build + real NWC connect can exceed the default 30s hook budget.
  test.setTimeout(240_000);
  mkdirSync(OUT_DIR, { recursive: true });
  await startHarness();
});

test.afterAll(async () => {
  stopHarness();

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
});

test("phase-2 real money: request, pay+receipt, verified toast, net-zero", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1280, height: 720 });

  await page.addInitScript((proxyUrl) => {
    window.__BUZZ_E2E_WALLET_PROXY__ = proxyUrl;
  }, HARNESS_URL);

  // seedPreviewFeatures default true → wallet experiment ON (localStorage override).
  await installMockBridge(page, {
    walletStatus: {
      linked: false,
      capabilities: [],
      receive_mode: "unavailable",
      lud16: null,
      balance_msat: null,
    },
    searchProfiles: [{ pubkey: ALICE, displayName: "alice", isAgent: false }],
  });

  await page.goto("/");
  await openSettings(page, "wallet");
  await expect(page.getByTestId("wallet-link-uri")).toBeVisible();

  const uriRes = await fetch(`${HARNESS_URL}/uri`);
  expect(uriRes.ok).toBe(true);
  const { uri } = (await uriRes.json()) as { uri: string };
  expect(uri.startsWith("nostr+walletconnect://")).toBe(true);

  await page.getByTestId("wallet-link-uri").fill(uri);
  await page.getByTestId("wallet-link-submit").click();
  const linked = page.getByTestId("wallet-linked-status");
  await expect(linked).toBeVisible({ timeout: 60_000 });
  const balanceEl = page.getByTestId("wallet-balance");
  await expect(balanceEl).toContainText("sats", { timeout: 60_000 });
  const balanceBeforeSats = parseSats((await balanceEl.innerText()).trim());
  expect(balanceBeforeSats).toBeGreaterThan(PAY_AMOUNT_SATS);
  await fullWindowShot(page, "01-settings-linked");

  // Clear link toast before Stage A shots.
  await expect(page.locator("[data-sonner-toast]")).toHaveCount(0, {
    timeout: 8_000,
  });

  // ── Stage A — Requester: RequestPaymentDialog → real bolt11 → own_request ──
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("settings-view")).toHaveCount(0);
  await page.getByTestId(`channel-${CHANNEL}`).click();
  await waitForMockLiveSubscription(page, CHANNEL);
  await seedChannelMessage(page);

  const requestButton = page.getByTestId("request-payment-button");
  await expect(requestButton).toBeVisible();
  await expect(requestButton).not.toHaveAttribute("aria-disabled", "true");
  await requestButton.click();
  await expect(page.getByTestId("request-payment-dialog")).toBeVisible();

  await page
    .getByTestId("request-payment-amount")
    .fill(String(PAY_AMOUNT_SATS));
  await page
    .getByTestId("request-payment-memo")
    .fill("phase2 own 1-sat request");
  await fullWindowShot(page, "02-request-dialog-open");

  await page.getByTestId("request-payment-submit").click();

  const ownCard = page
    .getByTestId("payment-request-card")
    .filter({ hasText: "phase2 own 1-sat request" });
  await expect(ownCard).toBeVisible({ timeout: 60_000 });
  await expect(ownCard).toHaveAttribute("data-pay-card-state", "own_request");
  await expect(ownCard.getByTestId("payment-request-status")).toHaveText(
    "Your request",
  );
  await expect(ownCard.getByTestId("payment-request-pay")).toHaveCount(0);

  // Signed 40009 must carry a real bolt11 (minted via proxied wallet_receive).
  const signedRequest = await page.evaluate(() => {
    const signed =
      (
        window as Window & {
          __BUZZ_E2E_SIGNED_EVENTS__?: Array<{
            kind: number;
            tags: string[][];
            id: string;
          }>;
        }
      ).__BUZZ_E2E_SIGNED_EVENTS__ ?? [];
    return [...signed].reverse().find((e) => e.kind === 40009) ?? null;
  });
  expect(signedRequest).not.toBeNull();
  const signedBolt11 =
    signedRequest?.tags.find((t) => t[0] === "bolt11")?.[1] ?? "";
  expect(signedBolt11.toLowerCase().includes("lnbc")).toBe(true);

  const registry = await stageARegistryEntry(page);
  expect(registry.requestEventId).toMatch(/^[0-9a-f]{64}$/i);
  expect(registry.bolt11.toLowerCase()).toBe(signedBolt11.toLowerCase());
  expect(registry.amountMsat).toBe(PAY_AMOUNT_MSAT);

  await ownCard.evaluate((el) => {
    el.scrollIntoView({ block: "center", inline: "nearest" });
  });
  await fullWindowShot(page, "03-own-request-card");

  // Clear "Payment request posted" toast before payer stage.
  await expect(page.locator("[data-sonner-toast]")).toHaveCount(0, {
    timeout: 8_000,
  });

  // ── Stage B — Payer: mint Alice 40009, pay through card, outbox 40010 ──
  const mintRes = await fetch(`${HARNESS_URL}/mint`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      amount_msat: PAY_AMOUNT_MSAT,
      description: "phase2-alice-pay-card",
    }),
  });
  expect(mintRes.ok).toBe(true);
  const minted = (await mintRes.json()) as {
    bolt11: string;
    payment_hash: string;
  };
  expect(minted.bolt11.toLowerCase().startsWith("lnbc")).toBe(true);
  expect(minted.payment_hash).toMatch(/^[0-9a-f]{64}$/i);

  const expiryUnix = Math.floor(Date.now() / 1000) + 3600;
  await page.evaluate(
    ({ channelName, id, pubkey, amountMsat, bolt11, expiry }) => {
      window.__BUZZ_E2E_EMIT_MOCK_MESSAGE__?.({
        channelName,
        content: "phase2 alice 1-sat tip",
        pubkey,
        kind: 40009,
        id,
        createdAt: Math.floor(Date.now() / 1000) - 5,
        extraTags: [
          ["amount", String(amountMsat)],
          ["memo", "phase2 alice 1-sat tip"],
          ["expiry", String(expiry)],
          ["p", pubkey],
          ["bolt11", bolt11],
        ],
      });
    },
    {
      channelName: CHANNEL,
      id: ALICE_REQUEST_ID,
      pubkey: ALICE,
      amountMsat: PAY_AMOUNT_MSAT,
      bolt11: minted.bolt11,
      expiry: expiryUnix,
    },
  );

  const payableCard = page
    .getByTestId("payment-request-card")
    .filter({ hasText: "phase2 alice 1-sat tip" });
  await expect(payableCard).toBeVisible({ timeout: 15_000 });
  await expect(payableCard).toHaveAttribute("data-pay-card-state", "payable");
  await expect(payableCard.getByTestId("payment-request-pay")).toBeVisible();
  await payableCard.evaluate((el) => {
    el.scrollIntoView({ block: "center", inline: "nearest" });
  });
  await fullWindowShot(page, "04-payable-card");

  await payableCard.getByTestId("payment-request-pay").click();
  const dialog = page.getByTestId("wallet-confirm-dialog");
  await expect(dialog).toBeVisible({ timeout: 60_000 });
  await expect(page.getByTestId("wallet-confirm-amount")).toHaveText(
    `${PAY_AMOUNT_SATS} sats`,
  );

  await page.getByTestId("wallet-confirm-submit").click();
  await expect(page.getByTestId("wallet-confirm-settled")).toBeVisible({
    timeout: 60_000,
  });

  const confirm = await page.evaluate(() => {
    return window.__BUZZ_E2E_LAST_WALLET_CONFIRM__ as {
      status?: string;
      preimage?: string;
    } | null;
  });
  expect(confirm?.status).toBe("settled");
  expect(confirm?.preimage).toMatch(/^[0-9a-f]{64}$/i);
  const stageBPreimage = confirm?.preimage as string;
  expect(sha256Hex(stageBPreimage)).toBe(minted.payment_hash.toLowerCase());
  await fullWindowShot(page, "05-settled-confirm");

  // Production receipt outbox publishes a REAL 40010 for Alice's request.
  await expect
    .poll(async () => countSignedReceipts(page), {
      timeout: 60_000,
    })
    .toBe(1);
  const receipt = await latestSignedReceipt(page);
  expect(receipt).not.toBeNull();
  expect(receipt?.content).toBe("");
  const receiptTag = (name: string) =>
    receipt?.tags.find((t) => t[0] === name)?.[1] ?? null;
  expect(receiptTag("e")).toBe(ALICE_REQUEST_ID);
  expect(receiptTag("payment_hash")?.toLowerCase()).toBe(
    minted.payment_hash.toLowerCase(),
  );
  expect(receiptTag("preimage")?.toLowerCase()).toBe(
    stageBPreimage.toLowerCase(),
  );
  expect(receiptTag("amount")).toBe(String(PAY_AMOUNT_MSAT));

  await page.keyboard.press("Escape");
  await expect(page.getByTestId("wallet-confirm-dialog")).toHaveCount(0, {
    timeout: 5_000,
  });

  const paidAlice = page
    .getByTestId("payment-request-card")
    .filter({ hasText: "phase2 alice 1-sat tip" });
  try {
    await expect(paidAlice).toHaveAttribute("data-pay-card-state", "paid", {
      timeout: 3_000,
    });
  } catch {
    await page.getByTestId("channel-general").click();
    await page.getByTestId(`channel-${CHANNEL}`).click();
    await waitForMockLiveSubscription(page, CHANNEL);
    await expect(paidAlice).toHaveAttribute("data-pay-card-state", "paid", {
      timeout: 15_000,
    });
  }
  await expect(paidAlice.getByTestId("payment-request-status")).toHaveText(
    "Paid ✓",
  );
  await paidAlice.evaluate((el) => {
    el.scrollIntoView({ block: "center", inline: "nearest" });
  });
  await fullWindowShot(page, "06-paid-card");

  // Clear settle toast; verified toast must not appear yet (Stage A unpaid).
  await expect(page.locator("[data-sonner-toast]")).toHaveCount(0, {
    timeout: 8_000,
  });
  await expect(receivedToast(page)).toHaveCount(0);

  // ── Stage C — Off-UI pay of MY Stage-A bolt11 + bridge 40010 → verified toast ──
  const prepare = (await harnessInvoke("wallet_prepare_send", {
    amountMsat: PAY_AMOUNT_MSAT,
    target: { type: "bolt11", invoice: registry.bolt11 },
    attempt: { type: "standalone" },
    memo: "phase2 stage-c self-pay own request",
  })) as { handle_id?: string; payment_hash?: string };
  expect(prepare.handle_id).toBeTruthy();

  const stageCConfirm = (await harnessInvoke("wallet_confirm", {
    handleId: prepare.handle_id,
  })) as { status?: string; preimage?: string };
  expect(stageCConfirm.status).toBe("settled");
  expect(stageCConfirm.preimage).toMatch(/^[0-9a-f]{64}$/i);
  const stageCPreimage = stageCConfirm.preimage as string;
  expect(sha256Hex(stageCPreimage)).toBe(registry.paymentHash.toLowerCase());

  // Still no verified toast before the receipt nudge + real check_incoming.
  await expect(receivedToast(page)).toHaveCount(0);

  await page.evaluate(
    ({
      channelName,
      receiptId,
      requestId,
      payer,
      paymentHash,
      preimageHex,
      amountMsat,
    }) => {
      window.__BUZZ_E2E_EMIT_MOCK_MESSAGE__?.({
        channelName,
        content: "",
        pubkey: payer,
        kind: 40010,
        id: receiptId,
        createdAt: Math.floor(Date.now() / 1000),
        extraTags: [
          ["e", requestId],
          ["payment_hash", paymentHash],
          ["preimage", preimageHex],
          ["amount", String(amountMsat)],
        ],
      });
    },
    {
      channelName: CHANNEL,
      receiptId: STAGE_C_RECEIPT_ID,
      requestId: registry.requestEventId,
      payer: ALICE,
      paymentHash: registry.paymentHash,
      preimageHex: stageCPreimage,
      amountMsat: PAY_AMOUNT_MSAT,
    },
  );

  await expect(receivedToast(page)).toBeVisible({ timeout: 60_000 });
  await expect(receivedToast(page)).toContainText(
    `Payment received — ${PAY_AMOUNT_SATS} sats`,
  );
  await expect(receivedToast(page)).toContainText(
    "Verified against your wallet",
  );
  await fullWindowShot(page, "07-verified-toast");

  // ── Stage D — Net-zero close ──
  await openSettings(page, "wallet");
  await expect(balanceEl).toContainText(
    `${balanceBeforeSats.toLocaleString()} sats`,
    { timeout: 60_000 },
  );
  await fullWindowShot(page, "08-balance-after");

  writeFileSync(
    path.join(OUT_DIR, "body.md"),
    `## Real money phase-2 — request, pay+receipt, verified toast, net-zero

**Real end to end:** wallet IPC → \`WalletRuntime\` → \`NwcWalletConnector\` → NWC wire → **real operator wallet**. Real link, real \`get_balance\` (**${balanceBeforeSats.toLocaleString()} sats**), real \`make_invoice\` (Stage A request + Stage B mint), real \`pay_invoice\` settle with verifying preimage (\`sha256(preimage) === payment_hash\`), real \`lookup_invoice\` / \`wallet_check_incoming\` for the verified toast.

**Own-request / payer split:** Stage A publishes MY 40009 via RequestPaymentDialog — card renders \`own_request\` (no Pay button). Stage B bridge-emits Alice's 40009 with a real bolt11 and pays it through the card; the production receipt outbox publishes a REAL 40010 carrying the live preimage. Stage C pays MY Stage-A bolt11 off-UI (harness \`/invoke\`) then bridge-feeds Alice's 40010 as a receipt hint — the verified toast fires only after real \`wallet_check_incoming\` returns paid.

**Money mechanics:** every payee invoice is minted from the linked wallet itself, so each 1-sat settle is a genuine self-payment — real settle, net-zero cost (balance before === after).

**Bridge-fed:** relay/timeline fan-out for Alice's 40009 and Stage-C 40010. Stage-B 40010 is production-published (signed + mock-relay stored). Relay ingest proven separately.

### 01 — Settings linked (REAL wallet, balance before)
{{01-settings-linked}}

### 02 — Request payment dialog open
{{02-request-dialog-open}}

### 03 — Own-request card (no Pay button)
{{03-own-request-card}}

### 04 — Payable Alice card (REAL 1-sat bolt11)
{{04-payable-card}}

### 05 — Settled confirm (REAL prepare_send + confirm)
{{05-settled-confirm}}

### 06 — Paid card (production 40010 outbox)
{{06-paid-card}}

### 07 — Verified toast (REAL check_incoming)
{{07-verified-toast}}

### 08 — Balance after (net-zero self-payments)
{{08-balance-after}}
`,
  );
});
