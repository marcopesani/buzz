/**
 * Live wallet fullscreen screenshots — REAL WalletRuntime → NWC → buzz-mock-wallet.
 *
 * Opt-in local artifact (skipped in CI). Requires Tauri native deps to build the
 * Rust harness. Run with:
 *   BUZZ_LIVE_WALLET_E2E=1 pnpm exec playwright test tests/e2e/wallet-live-fullscreen.spec.ts
 *
 * Harness mechanism:
 * 1. `cargo build --manifest-path desktop/src-tauri/Cargo.toml --bin wallet_e2e_harness`
 * 2. Spawn the binary with RUST_LOG; wait until GET http://127.0.0.1:4189/uri works
 * 3. Inject `window.__BUZZ_E2E_WALLET_PROXY__ = "http://127.0.0.1:4189"` BEFORE
 *    installMockBridge so wallet IPC is proxied; relay/timeline stay on the mock bridge
 * 4. Kill the harness in afterAll
 *
 * Honesty: wallet path is real (bolt11, preimage, balance). 40009/40010 fan-out
 * into the timeline is bridge-fed (relay ingest proven separately in U9).
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
  !process.env.BUZZ_LIVE_WALLET_E2E,
  "live wallet harness — set BUZZ_LIVE_WALLET_E2E=1 to run locally",
);

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(__dirname, "../../..");
const OUT_DIR = path.resolve("test-results/wallet-live-screenshots");
const HARNESS_URL = "http://127.0.0.1:4189";
const HARNESS_LOG = "/tmp/proofs/unit-11b-harness.log";
const CHANNEL = "random";
const VIEWER = "deadbeef".repeat(8);
const ALICE = TEST_IDENTITIES.alice.pubkey;
const REQUEST_ID = "a1".repeat(32);
const RECEIPT_ID = "b2".repeat(32);
const START_BALANCE_SATS = 100_000;
const PAY_AMOUNT_MSAT = 21_000;
const PAY_AMOUNT_SATS = 21;
const AFTER_BALANCE_SATS = START_BALANCE_SATS - PAY_AMOUNT_SATS;

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
      RUST_LOG: "info,buzz_mock_wallet=info,nwc=info",
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

test.describe.configure({ mode: "serial", timeout: 180_000 });

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
  writeFileSync("/tmp/proofs/unit-11b-hashes.log", `${lines.join("\n")}\n`);
});

test("live wallet fullscreen flow with real NWC", async ({ page }) => {
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
  await expect(page.getByTestId("wallet-balance")).toContainText(
    "100,000 sats",
  );
  await expect(page.getByTestId("wallet-capabilities")).toContainText(
    "pay_invoice",
  );
  await fullWindowShot(page, "02-settings-linked");

  // Let the "Wallet linked" toast clear before later shots (visual noise).
  await expect(page.locator("[data-sonner-toast]")).toHaveCount(0, {
    timeout: 8_000,
  });

  // Receive 21,000 msat = 21 sats (UI speaks sats at the edge).
  await page.getByTestId("wallet-receive-amount").fill("21");
  await page.getByTestId("wallet-receive-memo").fill("live-receive");
  await page.getByTestId("wallet-receive-submit").click();
  const receiveSection = page.getByTestId("wallet-receive-invoice");
  await expect(page.getByTestId("wallet-receive-qr")).toBeVisible({
    timeout: 60_000,
  });
  const receiveBolt11 = (await receiveSection.innerText()).trim().toLowerCase();
  expect(receiveBolt11.includes("lnbc")).toBe(true);
  // Settings pane is tall — bring the full QR into the 1280x720 viewport.
  await receiveSection.scrollIntoViewIfNeeded();
  await fullWindowShot(page, "03-receive-qr");

  // Mint a real payee invoice (debits on pay; verifying preimage).
  const mintRes = await fetch(`${HARNESS_URL}/mint`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      amount_msat: PAY_AMOUNT_MSAT,
      description: "live-pay-card",
    }),
  });
  expect(mintRes.ok).toBe(true);
  const minted = (await mintRes.json()) as {
    bolt11: string;
    payment_hash: string;
  };
  expect(minted.bolt11.toLowerCase().startsWith("lnbc")).toBe(true);
  expect(minted.payment_hash).toMatch(/^[0-9a-f]{64}$/i);

  // Close settings → channel → emit payable 40009 with real bolt11.
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
        content: "warmup before pay card",
        pubkey,
        kind: 9,
        id: "c3".repeat(32),
        createdAt: Math.floor(Date.now() / 1000) - 30,
      });
    },
    { channelName: CHANNEL, pubkey: ALICE },
  );
  await expect(page.getByText("warmup before pay card")).toBeVisible({
    timeout: 15_000,
  });

  const expiryUnix = Math.floor(Date.now() / 1000) + 3600;
  await page.evaluate(
    ({ channelName, id, pubkey, amountMsat, bolt11, expiry }) => {
      window.__BUZZ_E2E_EMIT_MOCK_MESSAGE__?.({
        channelName,
        content: "live tip",
        pubkey,
        kind: 40009,
        id,
        createdAt: Math.floor(Date.now() / 1000) - 5,
        extraTags: [
          ["amount", String(amountMsat)],
          ["memo", "live tip"],
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

  await openSettings(page, "wallet");
  await expect(page.getByTestId("wallet-balance")).toContainText(
    `${AFTER_BALANCE_SATS.toLocaleString()} sats`,
    { timeout: 60_000 },
  );
  await fullWindowShot(page, "07-settings-balance-after");

  writeFileSync(
    path.join(OUT_DIR, "body.md"),
    `## Honesty boundary

**Real:** wallet IPC → \`WalletRuntime\` → \`NwcWalletConnector\` → NWC wire → \`buzz-mock-wallet\` daemon; real bolt11s; real preimages (\`sha256(preimage) === payment_hash\`); real msat ledger balance.

**Bridge-fed:** relay/timeline events (40009 payment request + 40010 receipt fan-out). The receipt carries the **real** preimage returned by the live \`pay_invoice\`. Relay ingest was proven separately in U9.

### 01 — Settings unlinked
Unlinked wallet section (no NWC secret). Harness running; UI not yet linked.

{{01-settings-unlinked}}

### 02 — Settings linked (REAL)
Linked via real NWC URI from harness \`GET /uri\`. Capabilities + **100,000 sats** from live \`get_balance\`.

{{02-settings-linked}}

### 03 — Receive QR (REAL)
Interactive receive of 21 sats (21,000 msat). QR shows a real \`lnbc…\` bolt11 from \`make_invoice\`.

{{03-receive-qr}}

### 04 — Pay card (REAL bolt11)
Timeline 40009 is bridge-fed, but the \`bolt11\` tag is a harness-minted payee invoice (real bolt11 / payment_hash).

{{04-pay-card}}

### 05 — Quote dialog (REAL prepare_send)
Confirm dialog from live \`prepare_send\` decoding the real bolt11; amount **21 sats**.

{{05-quote-dialog}}

### 06 — Paid card (REAL preimage)
After live \`pay_invoice\`, receipt emitted with the real preimage/hash. Card shows Paid ✓.

{{06-pay-card-paid}}

### 07 — Balance after (REAL debit)
Settings balance **99,979 sats** — msat moved in the mock daemon ledger (not a scripted mock).

{{07-settings-balance-after}}
`,
  );
});
