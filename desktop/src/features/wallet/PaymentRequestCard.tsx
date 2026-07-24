import { useMemo, useState } from "react";
import { Check, Zap } from "lucide-react";

import type { TimelineMessage } from "@/features/messages/types";
import { getChannelIdFromTags } from "@/features/messages/lib/threading";
import { useIdentityQuery } from "@/shared/api/hooks";
import { Button } from "@/shared/ui/button";
import { cn } from "@/shared/lib/cn";

import {
  derivePayCardState,
  payCardShowsPayButton,
} from "./derivePayCardState";
import { msatToSatsDisplay } from "./msat";
import { parsePaymentRequest } from "./parsePaymentRequest";
import { ConfirmSendDialog } from "./ConfirmSendDialog";
import { walletPrepareSend, type PrepareSendQuote } from "./walletApi";
import { useWalletStatus } from "./useWalletStatus";

export type PaymentRequestCardProps = {
  message: TimelineMessage;
};

function statusLabel(
  state: ReturnType<typeof derivePayCardState>,
): string | null {
  switch (state.kind) {
    case "paid":
      return "Paid ✓";
    case "expired":
      return "Expired";
    case "own_request":
      return "Your request";
    case "unlinked":
      return "Link a wallet to pay";
    case "payable":
      return null;
    default: {
      const _exhaustive: never = state;
      return _exhaustive;
    }
  }
}

/**
 * Timeline card for KIND_PAYMENT_REQUEST.
 * Card state is one derivation — Pay visibility comes from that result alone.
 */
export function PaymentRequestCard({ message }: PaymentRequestCardProps) {
  const { status } = useWalletStatus();
  const identityQuery = useIdentityQuery();
  const myPubkey = identityQuery.data?.pubkey;
  const parsed = useMemo(
    () => parsePaymentRequest(message.tags),
    [message.tags],
  );
  const nowUnix = Math.floor(Date.now() / 1000);
  const state = derivePayCardState({
    requestPubkey: message.pubkey ?? message.signerPubkey ?? "",
    myPubkey,
    expiryUnix: parsed.expiryUnix,
    nowUnix,
    hasReceipt: Boolean(message.paymentReceipt),
    walletLinked: status.linked,
  });
  const showPay = payCardShowsPayButton(state);
  const amountSats =
    parsed.amountMsat != null ? msatToSatsDisplay(parsed.amountMsat) : null;
  const label = statusLabel(state);

  const [quote, setQuote] = useState<PrepareSendQuote | null>(null);
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [preparing, setPreparing] = useState(false);
  const [prepareError, setPrepareError] = useState<string | null>(null);
  const channelId = getChannelIdFromTags(message.tags ?? []) ?? "";

  const handlePay = async () => {
    if (parsed.amountMsat == null) return;
    setPreparing(true);
    setPrepareError(null);
    try {
      const target = parsed.bolt11
        ? { type: "bolt11" as const, invoice: parsed.bolt11 }
        : parsed.lud16
          ? { type: "lud16" as const, address: parsed.lud16 }
          : null;
      if (!target) {
        setPrepareError("Request has no bolt11 or lud16");
        return;
      }
      const next = await walletPrepareSend({
        target,
        amountMsat: parsed.amountMsat,
        attempt: { type: "pay_request", event_id: message.id },
        memo: parsed.memo,
      });
      setQuote(next);
      setConfirmOpen(true);
    } catch (err) {
      setPrepareError(err instanceof Error ? err.message : String(err));
    } finally {
      setPreparing(false);
    }
  };

  return (
    <div
      className={cn(
        "max-w-md rounded-xl border border-border/70 bg-muted/30 p-4",
        state.kind === "paid" && "border-emerald-500/40 bg-emerald-500/5",
        state.kind === "expired" && "opacity-70",
      )}
      data-pay-card-state={state.kind}
      data-has-receipt={message.paymentReceipt ? "1" : "0"}
      data-testid="payment-request-card"
    >
      <div className="flex items-start gap-3">
        <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-full bg-background">
          {state.kind === "paid" ? (
            <Check className="h-4 w-4 text-emerald-600" />
          ) : (
            <Zap className="h-4 w-4 text-amber-500" />
          )}
        </div>
        <div className="min-w-0 flex-1 space-y-1">
          <p
            className="text-sm font-medium"
            data-testid="payment-request-amount"
          >
            {amountSats != null
              ? `Pay ${amountSats.toLocaleString()} sats`
              : "Payment request"}
          </p>
          {parsed.memo ? (
            <p
              className="text-sm text-muted-foreground"
              data-testid="payment-request-memo"
            >
              {parsed.memo}
            </p>
          ) : null}
          {label ? (
            <p
              className={cn(
                "text-xs font-medium",
                state.kind === "paid" &&
                  "text-emerald-600 dark:text-emerald-400",
                state.kind === "expired" && "text-muted-foreground",
              )}
              data-testid="payment-request-status"
            >
              {label}
            </p>
          ) : null}
          {prepareError ? (
            <p className="text-xs text-destructive">{prepareError}</p>
          ) : null}
        </div>
      </div>
      {showPay ? (
        <div className="mt-3">
          <Button
            className="w-full"
            data-testid="payment-request-pay"
            disabled={preparing}
            onClick={() => void handlePay()}
            size="sm"
            type="button"
          >
            {preparing ? "Preparing…" : "Pay"}
          </Button>
        </div>
      ) : null}
      <ConfirmSendDialog
        onOpenChange={setConfirmOpen}
        open={confirmOpen}
        payRequest={
          channelId ? { requestEventId: message.id, channelId } : null
        }
        quote={quote}
      />
    </div>
  );
}
