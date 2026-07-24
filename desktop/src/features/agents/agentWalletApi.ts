/**
 * Thin Tauri IPC wrappers for managed-agent NWC wallet provisioning.
 *
 * Status never returns the URI — only a provisioned bool. Keyring load
 * failure is a distinct typed outcome, not "unlinked".
 */

import { invokeTauri } from "@/shared/api/tauri";

export const AGENT_WALLET_SECRET_UNAVAILABLE =
  "agent_wallet_secret_unavailable" as const;

export type AgentWalletStatusOutcome =
  | { kind: "ok"; provisioned: boolean }
  | { kind: "keyring_unavailable" };

type AgentWalletStatusView = {
  provisioned: boolean;
};

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/** Keyring presence for an agent's NWC wallet. Never returns the URI. */
export async function fetchAgentWalletStatus(
  pubkey: string,
): Promise<AgentWalletStatusOutcome> {
  try {
    const view = await invokeTauri<AgentWalletStatusView>(
      "agent_wallet_status",
      { pubkey },
    );
    return { kind: "ok", provisioned: view.provisioned };
  } catch (error) {
    if (errorMessage(error) === AGENT_WALLET_SECRET_UNAVAILABLE) {
      return { kind: "keyring_unavailable" };
    }
    throw error;
  }
}

/**
 * Probe + store a receive-only NWC URI for the agent.
 * Errors from the backend (probe failure, invalid URI) are thrown as-is —
 * callers should surface `error.message` verbatim.
 */
export async function provisionManagedAgentWallet(
  pubkey: string,
  uri: string,
): Promise<void> {
  await invokeTauri("provision_managed_agent_wallet", { pubkey, uri });
}

/** Remove the agent's NWC URI from the keyring. */
export async function unprovisionManagedAgentWallet(
  pubkey: string,
): Promise<void> {
  await invokeTauri("unprovision_managed_agent_wallet", { pubkey });
}
