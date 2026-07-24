import assert from "node:assert/strict";
import { describe, it } from "node:test";

import {
  AGENT_WALLET_UNLINK_WARNING,
  AGENT_WALLET_WORKING_GUARD_TOOLTIP,
  deriveAgentWalletBlockState,
} from "./deriveAgentWalletBlockState.ts";

const idleBase = {
  statusLoading: false,
  mutation: "idle",
  probeError: null,
  confirmUnlink: false,
  agentWorking: false,
  needsRestart: false,
  agentRunning: false,
};

describe("deriveAgentWalletBlockState", () => {
  it("status loading maps to loading", () => {
    const state = deriveAgentWalletBlockState({
      ...idleBase,
      status: null,
      statusLoading: true,
    });
    assert.equal(state.kind, "loading");
  });

  it("null status before first fetch maps to loading", () => {
    const state = deriveAgentWalletBlockState({
      ...idleBase,
      status: null,
      statusLoading: false,
    });
    assert.equal(state.kind, "loading");
  });

  it("provisioned false maps to unlinked", () => {
    const state = deriveAgentWalletBlockState({
      ...idleBase,
      status: { kind: "ok", provisioned: false },
    });
    assert.equal(state.kind, "unlinked");
    if (state.kind !== "unlinked") return;
    assert.equal(state.error, null);
    assert.equal(state.actionsGuarded, false);
    assert.equal(state.showRestartCta, false);
  });

  it("provisioned true maps to linked", () => {
    const state = deriveAgentWalletBlockState({
      ...idleBase,
      status: { kind: "ok", provisioned: true },
    });
    assert.equal(state.kind, "linked");
    if (state.kind !== "linked") return;
    assert.equal(state.confirmUnlink, false);
    assert.equal(state.showRestartCta, false);
  });

  it("keyring-unavailable error maps to keyring_unavailable, not unlinked", () => {
    const state = deriveAgentWalletBlockState({
      ...idleBase,
      status: { kind: "keyring_unavailable" },
    });
    assert.equal(state.kind, "keyring_unavailable");
    assert.notEqual(state.kind, "unlinked");
  });

  it("probe-error surfaces message and returns to unlinked", () => {
    const message = "receive probe failed: make_invoice missing";
    const state = deriveAgentWalletBlockState({
      ...idleBase,
      status: { kind: "ok", provisioned: false },
      probeError: message,
    });
    assert.equal(state.kind, "unlinked");
    if (state.kind !== "unlinked") return;
    assert.equal(state.error, message);
  });

  it("probing mutation wins over unlinked status", () => {
    const state = deriveAgentWalletBlockState({
      ...idleBase,
      status: { kind: "ok", provisioned: false },
      mutation: "probing",
    });
    assert.equal(state.kind, "probing");
  });

  it("unlinking mutation wins over linked status", () => {
    const state = deriveAgentWalletBlockState({
      ...idleBase,
      status: { kind: "ok", provisioned: true },
      mutation: "unlinking",
      needsRestart: true,
      agentRunning: true,
    });
    assert.equal(state.kind, "unlinking");
    if (state.kind !== "unlinking") return;
    assert.equal(state.showRestartCta, true);
  });

  it("working-agent guards link/unlink actions", () => {
    const unlinked = deriveAgentWalletBlockState({
      ...idleBase,
      status: { kind: "ok", provisioned: false },
      agentWorking: true,
    });
    assert.equal(unlinked.kind, "unlinked");
    if (unlinked.kind !== "unlinked") return;
    assert.equal(unlinked.actionsGuarded, true);
    assert.equal(unlinked.guardTooltip, AGENT_WALLET_WORKING_GUARD_TOOLTIP);

    const linked = deriveAgentWalletBlockState({
      ...idleBase,
      status: { kind: "ok", provisioned: true },
      agentWorking: true,
    });
    assert.equal(linked.kind, "linked");
    if (linked.kind !== "linked") return;
    assert.equal(linked.actionsGuarded, true);
    assert.equal(linked.guardTooltip, AGENT_WALLET_WORKING_GUARD_TOOLTIP);
  });

  it("unlink confirm gate flips confirmUnlink on linked state", () => {
    const closed = deriveAgentWalletBlockState({
      ...idleBase,
      status: { kind: "ok", provisioned: true },
      confirmUnlink: false,
    });
    assert.equal(closed.kind, "linked");
    if (closed.kind !== "linked") return;
    assert.equal(closed.confirmUnlink, false);

    const open = deriveAgentWalletBlockState({
      ...idleBase,
      status: { kind: "ok", provisioned: true },
      confirmUnlink: true,
    });
    assert.equal(open.kind, "linked");
    if (open.kind !== "linked") return;
    assert.equal(open.confirmUnlink, true);
    assert.match(AGENT_WALLET_UNLINK_WARNING, /pending payment requests/i);
  });

  it("shows restart CTA only when running and needsRestart", () => {
    const staleRunning = deriveAgentWalletBlockState({
      ...idleBase,
      status: { kind: "ok", provisioned: true },
      needsRestart: true,
      agentRunning: true,
    });
    assert.equal(staleRunning.kind, "linked");
    if (staleRunning.kind !== "linked") return;
    assert.equal(staleRunning.showRestartCta, true);

    const staleStopped = deriveAgentWalletBlockState({
      ...idleBase,
      status: { kind: "ok", provisioned: true },
      needsRestart: true,
      agentRunning: false,
    });
    assert.equal(staleStopped.kind, "linked");
    if (staleStopped.kind !== "linked") return;
    assert.equal(staleStopped.showRestartCta, false);
  });
});
