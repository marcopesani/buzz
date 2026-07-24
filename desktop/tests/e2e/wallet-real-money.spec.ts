/**
 * Real-money wallet fullscreen screenshots — REAL WalletRuntime → NWC → a REAL
 * operator wallet (Alby et al.). No mock wallet anywhere on the money path.
 *
 * Opt-in local artifact (skipped in CI). Requires a funded wallet:
 *   BUZZ_NWC_URI=nostr+walletconnect://… \
 *   BUZZ_REAL_WALLET_E2E=1 pnpm exec playwright test tests/e2e/wallet-real-money.spec.ts
 *
 * Money mechanics: the payee invoice is minted FROM the linked wallet itself
 * (harness real mode), so the pay is a genuine self-payment — a real routed
 * settle with a real preimage, at net-zero cost. Amount is 1 sat.
 *
 * Honesty: wallet path is real end to end (link, balance, bolt11, preimage,
 * settle). 40009/40010 timeline fan-out is bridge-fed (relay ingest proven in U9).
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
test.skip(
  !process.env.BUZZ_REAL_WALLET_E2E || !process.env.BUZZ_NWC_URI,
  "real wallet run — set BUZZ_REAL_WALLET_E2E=1 and BUZZ_NWC_URI to run locally",
);

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(__dirname, "../../..");
const OUT_DIR = path.resolve("test-results/wallet-real-screenshots");
const HARNESS_URL = "http://127.0.0.1:4189";
const HARNESS_LOG = "/tmp/proofs/wallet-real-harness.log";
const CHANNEL = "random";
const VIEWER = "deadbeef".repeat(8);
const ALICE = TEST_IDENTITIES.alice.pubkey;
const REQUEST_ID = "d4".repeat(32);
const RECEIPT_ID = "e5".repeat(32);
// Smallest expressible payment: 1 sat. Self-pay makes the net cost zero.
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

test.describe.configure({ mode: "serial", timeout: 240_000 });

test.beforeAll(async () => {
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

test("real money moves through the full wallet flow", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 720 });

  await page.addInitScript((proxyUrl) => {
    window.__BUZZ_E2E_WALLET_PROXY__ = proxyUrl;
  }, HARNESS_URL);

  await installMockBridge(page, {
    walletStatus: {
      linked: false,
      capabilities: [],
      receive_mode: "unavailable",
      lud16: null,
      balance_msat: null,
    },
  });

  await page.goto("/");
  await openSettings(page, "wallet");
  await expect(page.getByTestId("wallet-link-uri")).toBeVisible();
  await fullWindowShot(page, "01-settings-unlinked");

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
  await fullWindowShot(page, "02-settings-linked");

  // Let the "Wallet linked" toast clear before later shots (visual noise).
  await expect(page.locator("[data-sonner-toast]")).toHaveCount(0, {
    timeout: 8_000,
  });

  // Interactive receive: real bolt11 minted by the real wallet.
  await page.getByTestId("wallet-receive-amount").fill(String(PAY_AMOUNT_SATS));
  await page.getByTestId("wallet-receive-memo").fill("real-receive");
  await page.getByTestId("wallet-receive-submit").click();
  const receiveSection = page.getByTestId("wallet-receive-invoice");
  await expect(page.getByTestId("wallet-receive-qr")).toBeVisible({
    timeout: 60_000,
  });
  const receiveBolt11 = (await receiveSection.innerText()).trim().toLowerCase();
  expect(receiveBolt11.includes("lnbc")).toBe(true);
  await receiveSection.scrollIntoViewIfNeeded();
  await fullWindowShot(page, "03-receive-qr");

  // Payee invoice minted from the linked wallet itself → self-payment.
  const mintRes = await fetch(`${HARNESS_URL}/mint`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      amount_msat: PAY_AMOUNT_MSAT,
      description: "real-money-pay-card",
    }),
  });
  expect(mintRes.ok).toBe(true);
  const minted = (await mintRes.json()) as {
    bolt11: string;
    payment_hash: string;
  };
  expect(minted.bolt11.toLowerCase().startsWith("lnbc")).toBe(true);
  expect(minted.payment_hash).toMatch(/^[0-9a-f]{64}$/i);

  // Close settings → channel → emit payable 40009 with the real bolt11.
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("settings-view")).toHaveCount(0);
  await page.getByTestId(`channel-${CHANNEL}`).click();
  await expect
    .poll(async () =>
      page.evaluate(
        (name) =>
          window.__BUZZ_E2E_HAS_MOCK_LIVE_SUBSCRIPTION__?.({
            channelName: name,
            kind: 40009,
          }) ?? false,
        CHANNEL,
      ),
    )
    .toBe(true);

  // Warm the empty-channel intro into list mode so the pay card can mount.
  await page.evaluate(
    ({ channelName, pubkey }) => {
      window.__BUZZ_E2E_EMIT_MOCK_MESSAGE__?.({
        channelName,
        content: "warmup before real pay card",
        pubkey,
        kind: 9,
        id: "f6".repeat(32),
        createdAt: Math.floor(Date.now() / 1000) - 30,
      });
    },
    { channelName: CHANNEL, pubkey: ALICE },
  );
  await expect(page.getByText("warmup before real pay card")).toBeVisible({
    timeout: 15_000,
  });

  const expiryUnix = Math.floor(Date.now() / 1000) + 3600;
  await page.evaluate(
    ({ channelName, id, pubkey, amountMsat, bolt11, expiry }) => {
      window.__BUZZ_E2E_EMIT_MOCK_MESSAGE__?.({
        channelName,
        content: "real 1-sat tip",
        pubkey,
        kind: 40009,
        id,
        createdAt: Math.floor(Date.now() / 1000) - 5,
        extraTags: [
          ["amount", String(amountMsat)],
          ["memo", "real 1-sat tip"],
          ["expiry", String(expiry)],
          ["p", pubkey],
          ["bolt11", bolt11],
        ],
      });
    },
    {
      channelName: CHANNEL,
      id: REQUEST_ID,
      pubkey: ALICE,
      amountMsat: PAY_AMOUNT_MSAT,
      bolt11: minted.bolt11,
      expiry: expiryUnix,
    },
  );

  const payCard = page.getByTestId("payment-request-card");
  await expect(payCard).toBeVisible({ timeout: 15_000 });
  await expect(payCard).toHaveAttribute("data-pay-card-state", "payable");
  await expect(page.getByTestId("payment-request-pay")).toBeVisible();
  await fullWindowShot(page, "04-pay-card");

  await page.getByTestId("payment-request-pay").click();
  const dialog = page.getByTestId("wallet-confirm-dialog");
  await expect(dialog).toBeVisible({ timeout: 60_000 });
  await expect(page.getByTestId("wallet-confirm-amount")).toHaveText(
    `${PAY_AMOUNT_SATS} sats`,
  );
  await fullWindowShot(page, "05-quote-dialog");

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
  const preimage = confirm?.preimage as string;
  expect(sha256Hex(preimage)).toBe(minted.payment_hash.toLowerCase());

  await page.getByTestId("wallet-confirm-done").click();

  // Bridge-fed receipt carrying the REAL preimage/hash from the live pay.
  await page.evaluate(
    ({
      channelName,
      receiptId,
      requestId,
      viewer,
      paymentHash,
      preimageHex,
      amountMsat,
    }) => {
      window.__BUZZ_E2E_EMIT_MOCK_MESSAGE__?.({
        channelName,
        content: "",
        pubkey: viewer,
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
      receiptId: RECEIPT_ID,
      requestId: REQUEST_ID,
      viewer: VIEWER,
      paymentHash: minted.payment_hash,
      preimageHex: preimage,
      amountMsat: PAY_AMOUNT_MSAT,
    },
  );

  const paid = page.locator('[data-pay-card-state="paid"]');
  await expect(paid).toBeVisible();
  await expect(paid.getByTestId("payment-request-status")).toHaveText("Paid ✓");
  await fullWindowShot(page, "06-pay-card-paid");

  // Self-payment: debit and credit land in the same wallet → net-zero.
  await openSettings(page, "wallet");
  await expect(balanceEl).toContainText(
    `${balanceBeforeSats.toLocaleString()} sats`,
    { timeout: 60_000 },
  );
  await fullWindowShot(page, "07-settings-balance-after");

  writeFileSync(
    path.join(OUT_DIR, "body.md"),
    `## Real money run — 1 sat, real wallet, net-zero self-payment

**Real end to end:** wallet IPC → \`WalletRuntime\` → \`NwcWalletConnector\` → NWC wire → **real operator wallet** (\`BUZZ_NWC_URI\`). Real link, real \`get_balance\` (**${balanceBeforeSats.toLocaleString()} sats**), real \`make_invoice\` bolt11s, real \`pay_invoice\` settle with verifying preimage (\`sha256(preimage) === payment_hash\`).

**Money mechanics:** the payee invoice is minted from the linked wallet itself, so the 1-sat payment is a genuine self-payment — real settle, net-zero cost (balance before === after).

**Bridge-fed:** relay/timeline events (40009 request + 40010 receipt fan-out) — the receipt carries the **real** preimage. Relay ingest proven separately in U9.

### 01 — Settings unlinked
{{01-settings-unlinked}}

### 02 — Settings linked (REAL wallet, real balance)
{{02-settings-linked}}

### 03 — Receive QR (REAL 1-sat bolt11)
{{03-receive-qr}}

### 04 — Pay card (REAL 1-sat bolt11)
{{04-pay-card}}

### 05 — Quote dialog (REAL prepare_send)
{{05-quote-dialog}}

### 06 — Paid card (REAL preimage)
{{06-pay-card-paid}}

### 07 — Balance after (net-zero self-payment)
{{07-settings-balance-after}}
`,
  );
});
