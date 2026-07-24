import { expect, test, type Page } from "@playwright/test";
import { createHash } from "node:crypto";
import { mkdirSync, readFileSync } from "node:fs";
import path from "node:path";

import { installMockBridge, TEST_IDENTITIES } from "../helpers/bridge";
import { waitForAnimations } from "../helpers/animations";

const OUT_DIR = path.resolve("test-results/agent-wallet-modal-screenshots");
const AGENT_PUBKEY = TEST_IDENTITIES.tyler.pubkey;
const AGENT_NAME = "Tyler Agent";
const PROBE_ERROR = "receive probe failed: make_invoice missing";
const NWC_URI =
  "nostr+walletconnect://aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899?relay=wss://relay.example&secret=deadbeef";

function sha256File(filePath: string): string {
  return createHash("sha256").update(readFileSync(filePath)).digest("hex");
}

async function openEditAdvanced(page: Page) {
  await page.goto("/");
  await page.getByTestId("open-agents-view").click();

  const agentButton = page.getByRole("button", {
    name: `${AGENT_NAME} agent profile`,
  });
  await expect(agentButton).toBeVisible({ timeout: 10_000 });
  await agentButton.click();

  await expect(page.getByTestId("user-profile-panel")).toBeVisible({
    timeout: 10_000,
  });
  await page.getByTestId("user-profile-edit-agent").click();

  await expect(page.getByTestId("edit-agent-dialog")).toBeVisible({
    timeout: 10_000,
  });
  await expect(page.locator("#edit-agent-llm-provider")).toBeVisible({
    timeout: 10_000,
  });

  await page.getByRole("button", { name: "Advanced" }).click();
  await waitForAnimations(page);
}

async function shotBlock(page: Page, name: string): Promise<string> {
  const block = page.getByTestId("agent-wallet-block");
  await expect(block).toBeVisible();
  await waitForAnimations(page);
  const file = path.join(OUT_DIR, `${name}.png`);
  await block.screenshot({ path: file });
  return file;
}

function setAgentWalletMock(
  page: Page,
  next: {
    provisionedByPubkey?: Record<string, boolean>;
    statusError?: string | null;
    provisionError?: string | null;
    stickyProvisionError?: boolean;
  },
) {
  return page.evaluate((payload) => {
    window.__BUZZ_E2E_SET_AGENT_WALLET_MOCK__?.(payload);
  }, next);
}

test.describe.configure({ mode: "serial" });

test.beforeEach(async () => {
  mkdirSync(OUT_DIR, { recursive: true });
});

const screenshotHashes: string[] = [];

test("unlinked agent shows probe error then links on success", async ({
  page,
}) => {
  await installMockBridge(page, {
    managedAgents: [
      {
        pubkey: AGENT_PUBKEY,
        name: AGENT_NAME,
        status: "stopped",
        channelNames: ["agents"],
      },
    ],
    agentWallet: {
      provisionedByPubkey: { [AGENT_PUBKEY]: false },
      provisionError: PROBE_ERROR,
    },
  });

  await openEditAdvanced(page);
  await expect(page.getByTestId("agent-wallet-block")).toBeVisible();
  await expect(page.getByTestId("agent-wallet-unlinked")).toBeVisible();

  await page.getByTestId("agent-wallet-nwc-uri").fill(NWC_URI);
  await page.getByTestId("agent-wallet-link").click();
  await expect(page.getByTestId("agent-wallet-error")).toHaveText(PROBE_ERROR);
  await expect(page.getByTestId("agent-wallet-nwc-uri")).toHaveValue(NWC_URI);

  const errorShot = await shotBlock(page, "01-unlinked-with-error");
  screenshotHashes.push(sha256File(errorShot));

  await setAgentWalletMock(page, { provisionError: null });
  await page.getByTestId("agent-wallet-link").click();
  await expect(page.getByTestId("agent-wallet-linked")).toBeVisible({
    timeout: 10_000,
  });
  await expect(page.getByTestId("agent-wallet-linked-badge")).toBeVisible();
  await expect(page.getByTestId("agent-wallet-nwc-uri")).toHaveCount(0);
});

test("linked running agent unlink confirm and restart CTA", async ({
  page,
}) => {
  await installMockBridge(page, {
    managedAgents: [
      {
        pubkey: AGENT_PUBKEY,
        name: AGENT_NAME,
        status: "running",
        channelNames: ["agents"],
        needsRestart: false,
      },
    ],
    agentWallet: {
      provisionedByPubkey: { [AGENT_PUBKEY]: false },
    },
  });

  await openEditAdvanced(page);
  await expect(page.getByTestId("agent-wallet-unlinked")).toBeVisible();

  await page.getByTestId("agent-wallet-nwc-uri").fill(NWC_URI);
  await page.getByTestId("agent-wallet-link").click();
  await expect(page.getByTestId("agent-wallet-linked")).toBeVisible({
    timeout: 10_000,
  });
  await expect(page.getByTestId("agent-wallet-restart-cta")).toBeVisible();
  await expect(page.getByTestId("agent-wallet-restart")).toHaveText(
    "Restart to apply",
  );

  const restartShot = await shotBlock(page, "02-linked-with-restart-cta");
  screenshotHashes.push(sha256File(restartShot));

  await page.getByTestId("agent-wallet-unlink").click();
  const confirm = page.getByTestId("agent-wallet-unlink-confirm");
  await expect(confirm).toBeVisible();
  await expect(confirm).toContainText(
    "Pending payment requests made by this agent can no longer be verified by it after unlinking.",
  );

  await page.getByTestId("agent-wallet-unlink-confirm-submit").click();
  await expect(page.getByTestId("agent-wallet-unlinked")).toBeVisible({
    timeout: 10_000,
  });
  await expect(page.getByTestId("agent-wallet-restart-cta")).toBeVisible();
});

test("keyring unavailable shows degraded notice with actions disabled", async ({
  page,
}) => {
  await installMockBridge(page, {
    managedAgents: [
      {
        pubkey: AGENT_PUBKEY,
        name: AGENT_NAME,
        status: "stopped",
        channelNames: ["agents"],
      },
    ],
    agentWallet: {
      statusError: "agent_wallet_secret_unavailable",
    },
  });

  await openEditAdvanced(page);
  await expect(
    page.getByTestId("agent-wallet-keyring-unavailable"),
  ).toBeVisible();
  await expect(page.getByTestId("agent-wallet-unlinked")).toHaveCount(0);
  await expect(page.getByTestId("agent-wallet-link")).toHaveCount(0);
  await expect(page.getByTestId("agent-wallet-unlink")).toHaveCount(0);

  const keyringShot = await shotBlock(page, "03-keyring-unavailable");
  screenshotHashes.push(sha256File(keyringShot));
});

test("experiment off hides Lightning Wallet block in Advanced", async ({
  page,
}) => {
  await installMockBridge(
    page,
    {
      managedAgents: [
        {
          pubkey: AGENT_PUBKEY,
          name: AGENT_NAME,
          status: "stopped",
          channelNames: ["agents"],
        },
      ],
    },
    { seedPreviewFeatures: false },
  );

  await openEditAdvanced(page);
  await expect(page.getByTestId("agent-wallet-block")).toHaveCount(0);
  await expect(page.getByText("Lightning Wallet")).toHaveCount(0);
});

test("screenshot hashes are distinct", async () => {
  expect(screenshotHashes.length).toBeGreaterThanOrEqual(3);
  expect(new Set(screenshotHashes).size).toBe(screenshotHashes.length);
});
