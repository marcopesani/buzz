import { useState } from "react";
import { toast } from "sonner";

import { Button } from "@/shared/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/shared/ui/dialog";
import { Spinner } from "@/shared/ui/spinner";

import { msatToSatsDisplay } from "./msat";
import {
  enqueueAndPublishReceipt,
  reconcileAndFlushReceipts,
} from "./reconcileWalletReceipts";
import {
  walletCancel,
  walletConfirm,
  type PrepareSendQuote,
  type SendConfirmOutcome,
} from "./walletApi";

export type ConfirmSendPayRequestContext = {
  requestEventId: string;
  channelId: string;
};

export type ConfirmSendDialogProps = {
  open: boolean;
  quote: PrepareSendQuote | null;
  onOpenChange: (open: boolean) => void;
  onSettled?: (preimage: string) => void;
  /** When set, a settled confirm enqueues+publishes a 40010 for this request. */
  payRequest?: ConfirmSendPayRequestContext | null;
};

type Phase =
  | { kind: "ready" }
  | { kind: "confirming" }
  | { kind: "unknown" }
  | { kind: "failed"; reason: string }
  | { kind: "settled"; preimage: string };

/**
 * Sole UI path to `wallet_confirm`. Pay / Send only prepare; this dialog confirms.
 */
export function ConfirmSendDialog({
  open,
  quote,
  onOpenChange,
  onSettled,
  payRequest = null,
}: ConfirmSendDialogProps) {
  const [phase, setPhase] = useState<Phase>({ kind: "ready" });

  const resetAndClose = (nextOpen: boolean) => {
    if (!nextOpen) {
      setPhase({ kind: "ready" });
    }
    onOpenChange(nextOpen);
  };

  const handleCancel = async () => {
    if (!quote) {
      resetAndClose(false);
      return;
    }
    try {
      await walletCancel(quote.handle_id);
    } catch {
      // Handle may already be consumed; still close.
    }
    resetAndClose(false);
  };

  const handleConfirm = async () => {
    if (!quote) return;
    setPhase({ kind: "confirming" });
    let outcome: SendConfirmOutcome;
    try {
      outcome = await walletConfirm(quote.handle_id);
    } catch (err) {
      setPhase({
        kind: "failed",
        reason: err instanceof Error ? err.message : String(err),
      });
      return;
    }
    switch (outcome.status) {
      case "settled":
        setPhase({ kind: "settled", preimage: outcome.preimage });
        toast.success("Payment settled");
        onSettled?.(outcome.preimage);
        if (payRequest) {
          // Fire-and-forget: outbox is durable and retries on reconcile.
          // Never block or fail the confirm UI on a decorative receipt.
          void enqueueAndPublishReceipt({
            requestEventId: payRequest.requestEventId,
            paymentHash: quote.payment_hash,
            preimage: outcome.preimage,
            amountMsat: quote.amount_msat,
            channelId: payRequest.channelId,
          }).catch((err) => {
            console.warn(
              "[wallet-receipt] publish after settle failed:",
              err instanceof Error ? err.message : String(err),
            );
          });
        }
        break;
      case "failed":
        setPhase({ kind: "failed", reason: outcome.reason });
        break;
      case "unknown":
        setPhase({ kind: "unknown" });
        void reconcileAndFlushReceipts();
        break;
      case "already_claimed":
        setPhase({
          kind: "failed",
          reason: `Already claimed (${outcome.state})`,
        });
        break;
      default: {
        const _exhaustive: never = outcome;
        return _exhaustive;
      }
    }
  };

  const amountSats = quote ? msatToSatsDisplay(quote.amount_msat) : 0;
  const target =
    quote?.target_description ?? (quote ? `${quote.bolt11.slice(0, 24)}…` : "");

  return (
    <Dialog open={open && quote != null} onOpenChange={resetAndClose}>
      <DialogContent data-testid="wallet-confirm-dialog">
        <DialogHeader>
          <DialogTitle>Confirm payment</DialogTitle>
          <DialogDescription>
            Review the amount and destination before sending. This cannot be
            undone.
          </DialogDescription>
        </DialogHeader>

        {quote ? (
          <div className="space-y-3 text-sm" data-testid="wallet-confirm-quote">
            <div className="flex justify-between gap-4">
              <span className="text-muted-foreground">Amount</span>
              <span className="font-medium" data-testid="wallet-confirm-amount">
                {amountSats.toLocaleString()} sats
              </span>
            </div>
            <div className="flex justify-between gap-4">
              <span className="text-muted-foreground">To</span>
              <span
                className="min-w-0 truncate font-medium"
                data-testid="wallet-confirm-target"
                title={quote.target_description ?? quote.bolt11}
              >
                {target}
              </span>
            </div>
          </div>
        ) : null}

        {phase.kind === "unknown" ? (
          <p
            className="text-sm text-amber-600 dark:text-amber-400"
            data-testid="wallet-confirm-unknown"
          >
            Confirming… payment status is unknown. Checking the wallet — do not
            pay again.
          </p>
        ) : null}
        {phase.kind === "failed" ? (
          <p
            className="text-sm text-destructive"
            data-testid="wallet-confirm-failed"
          >
            Payment failed: {phase.reason}
          </p>
        ) : null}
        {phase.kind === "settled" ? (
          <p
            className="text-sm text-emerald-600 dark:text-emerald-400"
            data-testid="wallet-confirm-settled"
          >
            Paid ✓
          </p>
        ) : null}

        <DialogFooter>
          {phase.kind === "ready" || phase.kind === "failed" ? (
            <>
              <Button
                data-testid="wallet-confirm-cancel"
                onClick={() => void handleCancel()}
                type="button"
                variant="outline"
              >
                Cancel
              </Button>
              {phase.kind === "ready" ? (
                <Button
                  data-testid="wallet-confirm-submit"
                  onClick={() => void handleConfirm()}
                  type="button"
                >
                  Confirm
                </Button>
              ) : null}
            </>
          ) : null}
          {phase.kind === "confirming" ? (
            <Button disabled type="button">
              <Spinner className="mr-2" />
              Confirming…
            </Button>
          ) : null}
          {phase.kind === "unknown" ? (
            <Button
              data-testid="wallet-confirm-reconcile"
              onClick={() => void reconcileAndFlushReceipts()}
              type="button"
              variant="outline"
            >
              Refresh status
            </Button>
          ) : null}
          {phase.kind === "settled" ? (
            <Button
              data-testid="wallet-confirm-done"
              onClick={() => resetAndClose(false)}
              type="button"
            >
              Done
            </Button>
          ) : null}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
