/**
 * Thin Tauri IPC wrappers for the Lightning wallet.
 * The NWC URI is accepted once by `linkWallet` and never returned.
 */

import { invokeTauri } from "@/shared/api/tauri";
import {
  invalidateWalletStatus,
  setWalletStatusCache,
  type WalletStatusCache,
} from "./walletState";

export type WalletStatusView = {
  linked: boolean;
  capabilities: string[];
  receive_mode: string;
  lud16: string | null;
  balance_msat: number | null;
};

export type PrepareSendQuote = {
  handle_id: string;
  amount_msat: number;
  bolt11: string;
  payment_hash: string;
  expires_at_unix: number;
  target_description: string | null;
};

export type ReceiveInvoice = {
  bolt11: string;
  payment_hash: string;
  expires_at_unix: number;
};

export type SendConfirmOutcome =
  | { status: "settled"; preimage: string }
  | { status: "failed"; reason: string }
  | { status: "unknown" }
  | { status: "already_claimed"; state: string };

export type IncomingCheckOutcome =
  | { status: "paid" }
  | { status: "unpaid" }
  | { status: "unconfirmable" };

export type SettledPayRequest = {
  request_event_id: string;
  payment_hash: string;
  preimage: string | null;
  amount_msat: number;
};

export type SendTargetDto =
  | { type: "lud16"; address: string }
  | { type: "bolt11"; invoice: string };

export type AttemptKeyDto =
  | { type: "standalone" }
  | { type: "pay_request"; event_id: string };

function toCache(view: WalletStatusView): WalletStatusCache {
  return {
    linked: view.linked,
    capabilities: view.capabilities,
    receiveMode: view.receive_mode,
    lud16: view.lud16,
    balanceMsat: view.balance_msat,
  };
}

/** Link an NWC wallet. URI is never redisplayed. */
export async function linkWallet(uri: string): Promise<WalletStatusCache> {
  const view = await invokeTauri<WalletStatusView>("link_wallet", { uri });
  const cache = toCache(view);
  setWalletStatusCache(cache);
  return cache;
}

/** Unlink the active community's wallet. */
export async function unlinkWallet(): Promise<void> {
  await invokeTauri("unlink_wallet");
  invalidateWalletStatus();
  setWalletStatusCache({
    linked: false,
    capabilities: [],
    receiveMode: "unavailable",
    lud16: null,
    balanceMsat: null,
  });
}

/** Fetch linked status / capabilities / balance (msat). */
export async function fetchWalletStatus(): Promise<WalletStatusCache> {
  const view = await invokeTauri<WalletStatusView>("wallet_status");
  const cache = toCache(view);
  setWalletStatusCache(cache);
  return cache;
}

/** Mint a receive invoice. Amount is msat. */
export async function walletReceive(
  amountMsat: number,
  description?: string | null,
): Promise<ReceiveInvoice> {
  return invokeTauri<ReceiveInvoice>("wallet_receive", {
    amountMsat,
    description: description ?? null,
  });
}

/** Prepare a send — returns a quote; does not spend. */
export async function walletPrepareSend(input: {
  target: SendTargetDto;
  amountMsat: number;
  attempt: AttemptKeyDto;
  memo?: string | null;
}): Promise<PrepareSendQuote> {
  return invokeTauri<PrepareSendQuote>("wallet_prepare_send", {
    target: input.target,
    amountMsat: input.amountMsat,
    attempt: input.attempt,
    memo: input.memo ?? null,
  });
}

/** Confirm a prepared send. Sole path that may spend. */
export async function walletConfirm(
  handleId: string,
): Promise<SendConfirmOutcome> {
  return invokeTauri<SendConfirmOutcome>("wallet_confirm", { handleId });
}

/** Cancel a prepared send. */
export async function walletCancel(handleId: string): Promise<void> {
  await invokeTauri("wallet_cancel", { handleId });
}

/** Drain Paying/Unknown via lookup_invoice; returns newly settled pay requests. */
export async function walletReconcile(): Promise<SettledPayRequest[]> {
  return invokeTauri<SettledPayRequest[]>("wallet_reconcile");
}

/** Confirm an incoming payment request against this wallet only. */
export async function walletCheckIncoming(input: {
  bolt11?: string | null;
  lud16?: string | null;
}): Promise<IncomingCheckOutcome> {
  return invokeTauri<IncomingCheckOutcome>("wallet_check_incoming", {
    bolt11: input.bolt11 ?? null,
    lud16: input.lud16 ?? null,
  });
}
