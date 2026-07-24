/**
 * Single derived status union for the agent-edit Lightning Wallet block.
 *
 * Call sites switch on `kind` — not a pile of linked/loading/error/keyring
 * booleans. Mutation phases and the working-agent guard fold into the same
 * union so the UI has one obvious render path.
 */

import type { AgentWalletStatusOutcome } from "./agentWalletApi";

export const AGENT_WALLET_WORKING_GUARD_TOOLTIP =
  "Can't change wallet while the agent is working" as const;

export const AGENT_WALLET_UNLINK_WARNING =
  "Pending payment requests made by this agent can no longer be verified by it after unlinking." as const;

export type AgentWalletMutationPhase = "idle" | "probing" | "unlinking";

export type AgentWalletBlockState =
  | { kind: "loading" }
  | { kind: "keyring_unavailable" }
  | { kind: "probing" }
  | {
      kind: "unlinking";
      showRestartCta: boolean;
    }
  | {
      kind: "unlinked";
      error: string | null;
      actionsGuarded: boolean;
      guardTooltip: string | null;
      showRestartCta: boolean;
    }
  | {
      kind: "linked";
      confirmUnlink: boolean;
      actionsGuarded: boolean;
      guardTooltip: string | null;
      showRestartCta: boolean;
    };

export type DeriveAgentWalletBlockStateInput = {
  /** `null` while the first status fetch has not settled. */
  status: AgentWalletStatusOutcome | null;
  statusLoading: boolean;
  mutation: AgentWalletMutationPhase;
  /** Probe/provision error kept on the unlinked form (input preserved). */
  probeError: string | null;
  confirmUnlink: boolean;
  agentWorking: boolean;
  needsRestart: boolean;
  agentRunning: boolean;
};

function guardFields(agentWorking: boolean): {
  actionsGuarded: boolean;
  guardTooltip: string | null;
} {
  if (!agentWorking) {
    return { actionsGuarded: false, guardTooltip: null };
  }
  return {
    actionsGuarded: true,
    guardTooltip: AGENT_WALLET_WORKING_GUARD_TOOLTIP,
  };
}

/**
 * Derive the wallet block view-model from status + mutation + agent liveness.
 *
 * Mutation phases win over status so a probe in flight is never painted as
 * idle-unlinked. Keyring-unavailable is never collapsed into unlinked.
 */
export function deriveAgentWalletBlockState(
  input: DeriveAgentWalletBlockStateInput,
): AgentWalletBlockState {
  const showRestartCta = input.needsRestart && input.agentRunning;

  if (input.mutation === "probing") {
    return { kind: "probing" };
  }
  if (input.mutation === "unlinking") {
    return { kind: "unlinking", showRestartCta };
  }
  if (input.statusLoading || input.status === null) {
    return { kind: "loading" };
  }

  switch (input.status.kind) {
    case "keyring_unavailable":
      return { kind: "keyring_unavailable" };
    case "ok": {
      if (input.status.provisioned) {
        return {
          kind: "linked",
          confirmUnlink: input.confirmUnlink,
          showRestartCta,
          ...guardFields(input.agentWorking),
        };
      }
      return {
        kind: "unlinked",
        error: input.probeError,
        showRestartCta,
        ...guardFields(input.agentWorking),
      };
    }
    default: {
      const _exhaustive: never = input.status;
      return _exhaustive;
    }
  }
}
