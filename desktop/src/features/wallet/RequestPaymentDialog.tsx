import { useEffect, useRef, useState } from "react";
import { toast } from "sonner";

import { useIdentityQuery } from "@/shared/api/hooks";
import { relayClient } from "@/shared/api/relayClient";
import { signRelayEvent } from "@/shared/api/tauri";
import { Button } from "@/shared/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/shared/ui/dialog";
import { Input } from "@/shared/ui/input";
import { Spinner } from "@/shared/ui/spinner";

import { executeRequestPaymentSubmit } from "./executeRequestPaymentSubmit";
import { satsToMsat } from "./msat";
import {
  createSubmitLatch,
  reduceRequestSubmit,
  type MintedInvoice,
  type RequestSubmitPhase,
} from "./requestSubmitMachine";
import { registerPublishedPaymentRequest } from "./verifyPendingPaymentRequest";
import { walletReceive } from "./walletApi";

export type RequestPaymentDialogProps = {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  channelId: string | null;
  /** DM prepare-first — must run before mint so `h` is valid. */
  onPrepareSendChannel?: (pubkeys?: string[]) => Promise<string | null>;
};

type ExpiryPreset = "invoice" | "15m" | "1h" | "24h";

const EXPIRY_PRESET_SECONDS: Record<
  Exclude<ExpiryPreset, "invoice">,
  number
> = {
  "15m": 15 * 60,
  "1h": 60 * 60,
  "24h": 24 * 60 * 60,
};

function requestedExpiryUnix(
  preset: ExpiryPreset,
  nowUnix: number,
): number | null {
  if (preset === "invoice") return null;
  return nowUnix + EXPIRY_PRESET_SECONDS[preset];
}

/**
 * Composer dialog: amount + optional memo/expiry → mint invoice → publish 40009.
 * Retry reuses the sticky bolt11 from this dialog session.
 */
export function RequestPaymentDialog({
  open,
  onOpenChange,
  channelId,
  onPrepareSendChannel,
}: RequestPaymentDialogProps) {
  const identityQuery = useIdentityQuery();
  const payeePubkey = identityQuery.data?.pubkey ?? null;

  const [amountSats, setAmountSats] = useState("");
  const [memo, setMemo] = useState("");
  const [expiryPreset, setExpiryPreset] = useState<ExpiryPreset>("invoice");
  const [phase, setPhase] = useState<RequestSubmitPhase>({ kind: "form" });
  const [formError, setFormError] = useState<string | null>(null);
  const [clampedNotice, setClampedNotice] = useState<string | null>(null);

  const latchRef = useRef(createSubmitLatch());
  const openedChannelIdRef = useRef<string | null>(null);
  const currentChannelIdRef = useRef(channelId);
  currentChannelIdRef.current = channelId;
  const stickyInvoiceRef = useRef<MintedInvoice | null>(null);
  const stickyChannelIdRef = useRef<string | null>(null);
  const stickyAmountMsatRef = useRef(0);
  const stickyMemoRef = useRef<string | null>(null);
  const stickyExpiryUnixRef = useRef<number | null>(null);

  const wasOpenRef = useRef(false);
  useEffect(() => {
    if (open && !wasOpenRef.current) {
      // Capture epoch only on open — do not refresh if channelId changes later.
      openedChannelIdRef.current = channelId;
      stickyInvoiceRef.current = null;
      stickyChannelIdRef.current = null;
      stickyAmountMsatRef.current = 0;
      stickyMemoRef.current = null;
      stickyExpiryUnixRef.current = null;
      setAmountSats("");
      setMemo("");
      setExpiryPreset("invoice");
      setPhase({ kind: "form" });
      setFormError(null);
      setClampedNotice(null);
      latchRef.current = createSubmitLatch();
    }
    wasOpenRef.current = open;
  }, [open, channelId]);

  const resetAndClose = (nextOpen: boolean) => {
    if (!nextOpen) {
      setPhase(reduceRequestSubmit(phase, { type: "reset" }));
      setFormError(null);
      setClampedNotice(null);
    }
    onOpenChange(nextOpen);
  };

  const runSubmit = async (mode: "mint_and_publish" | "retry_publish") => {
    if (!latchRef.current.tryEnter()) return;

    const payee = payeePubkey;
    if (!payee) {
      latchRef.current.exit();
      setFormError("No identity available.");
      return;
    }

    let amountMsat: number;
    let memoValue: string | null;
    let requestedExpiry: number | null;

    if (mode === "retry_publish") {
      amountMsat = stickyAmountMsatRef.current;
      memoValue = stickyMemoRef.current;
      requestedExpiry = stickyExpiryUnixRef.current;
      setPhase((prev) => reduceRequestSubmit(prev, { type: "begin_retry" }));
    } else {
      const sats = Number(amountSats.trim());
      if (!Number.isSafeInteger(sats) || sats <= 0) {
        latchRef.current.exit();
        setFormError("Enter a positive whole-number amount in sats.");
        return;
      }
      amountMsat = satsToMsat(sats);
      memoValue = memo.trim() ? memo.trim() : null;
      requestedExpiry = requestedExpiryUnix(
        expiryPreset,
        Math.floor(Date.now() / 1000),
      );
      setFormError(null);
      setClampedNotice(null);
      setPhase((prev) => reduceRequestSubmit(prev, { type: "begin" }));
    }

    try {
      const result = await executeRequestPaymentSubmit({
        mode,
        amountMsat,
        memo: memoValue,
        requestedExpiryUnix: requestedExpiry,
        openedChannelId: openedChannelIdRef.current,
        getCurrentChannelId: () => currentChannelIdRef.current,
        payeePubkey: payee,
        existingInvoice: stickyInvoiceRef.current,
        existingChannelId: stickyChannelIdRef.current,
        prepareChannel: onPrepareSendChannel
          ? () => onPrepareSendChannel()
          : undefined,
        mintInvoice: async ({ amountMsat: msat, description }) =>
          walletReceive(msat, description),
        publishEvent: async ({ kind, content, tags }) => {
          const event = await signRelayEvent({ kind, content, tags });
          await relayClient.publishEvent(
            event,
            "Timed out while publishing the payment request.",
            "Failed to publish the payment request.",
          );
          return { eventId: event.id };
        },
      });

      if (result.ok) {
        stickyInvoiceRef.current = result.invoice;
        stickyChannelIdRef.current = result.channelId;
        registerPublishedPaymentRequest({
          requestEventId: result.requestEventId,
          channelId: result.channelId,
          amountMsat,
          bolt11: result.invoice.bolt11,
          paymentHash: result.invoice.payment_hash,
          createdAtUnix: Math.floor(Date.now() / 1000),
          expiryUnix: result.expiryUnix,
        });
        setPhase((prev) => reduceRequestSubmit(prev, { type: "published" }));
        toast.success("Payment request posted");
        if (result.clamped) {
          // Keep dialog open so the clamp is visible (event already published).
          setClampedNotice(
            `Expiry clamped to the invoice (${new Date(result.expiryUnix * 1000).toLocaleString()}).`,
          );
          return;
        }
        resetAndClose(false);
        return;
      }

      if (result.invoice) {
        stickyInvoiceRef.current = result.invoice;
        stickyChannelIdRef.current = result.channelId;
        stickyAmountMsatRef.current = result.amountMsat;
        stickyMemoRef.current = result.memo;
        stickyExpiryUnixRef.current = result.expiryUnix;
      }

      if (result.retryable && result.invoice && result.channelId != null) {
        const failedInvoice: MintedInvoice = result.invoice;
        const failedChannelId = result.channelId;
        setPhase((prev) =>
          reduceRequestSubmit(prev, {
            type: "publish_failed",
            invoice: failedInvoice,
            amountMsat: result.amountMsat,
            memo: result.memo,
            expiryUnix: result.expiryUnix ?? failedInvoice.expires_at_unix,
            channelId: failedChannelId,
            error: result.error,
          }),
        );
        setFormError(result.error);
        return;
      }

      setPhase((prev) => reduceRequestSubmit(prev, { type: "abort_to_form" }));
      setFormError(result.error);
    } finally {
      latchRef.current.exit();
    }
  };

  const busy = phase.kind === "busy";
  const retryable = phase.kind === "retryable";
  const success = phase.kind === "success";
  const formLocked = busy || retryable || success;
  const errorText =
    formError ?? (phase.kind === "retryable" ? phase.error : null);
  const retryBolt11 = stickyInvoiceRef.current?.bolt11 ?? null;

  return (
    <Dialog open={open} onOpenChange={resetAndClose}>
      <DialogContent data-testid="request-payment-dialog">
        <DialogHeader>
          <DialogTitle>Request payment</DialogTitle>
          <DialogDescription>
            Create an invoice and post a payment request in this conversation.
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-3">
          <div className="space-y-1.5">
            <label
              className="text-sm font-medium"
              htmlFor="request-payment-amount"
            >
              Amount (sats)
            </label>
            <Input
              data-testid="request-payment-amount"
              disabled={formLocked}
              id="request-payment-amount"
              inputMode="numeric"
              onChange={(e) => setAmountSats(e.target.value)}
              placeholder="210"
              value={amountSats}
            />
          </div>
          <div className="space-y-1.5">
            <label
              className="text-sm font-medium"
              htmlFor="request-payment-memo"
            >
              Memo (optional)
            </label>
            <Input
              data-testid="request-payment-memo"
              disabled={formLocked}
              id="request-payment-memo"
              onChange={(e) => setMemo(e.target.value)}
              placeholder="What is this for?"
              value={memo}
            />
          </div>
          <div className="space-y-1.5">
            <label
              className="text-sm font-medium"
              htmlFor="request-payment-expiry"
            >
              Expiry
            </label>
            <select
              className="flex h-9 w-full rounded-md border border-input bg-transparent px-3 py-1 text-sm shadow-xs focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-50"
              data-testid="request-payment-expiry"
              disabled={formLocked}
              id="request-payment-expiry"
              onChange={(e) => setExpiryPreset(e.target.value as ExpiryPreset)}
              value={expiryPreset}
            >
              <option value="invoice">Match invoice (default)</option>
              <option value="15m">15 minutes</option>
              <option value="1h">1 hour</option>
              <option value="24h">24 hours</option>
            </select>
            <p
              className="text-xs text-muted-foreground"
              data-testid="request-payment-expiry-hint"
            >
              Never longer than the invoice. Longer picks are clamped.
            </p>
          </div>
          {clampedNotice ? (
            <p
              className="text-xs text-amber-600 dark:text-amber-400"
              data-testid="request-payment-expiry-clamped"
            >
              {clampedNotice}
            </p>
          ) : null}
          {errorText ? (
            <p
              className="text-sm text-destructive"
              data-testid="request-payment-error"
            >
              {errorText}
            </p>
          ) : null}
          {retryable && retryBolt11 ? (
            <p
              className="text-xs text-muted-foreground"
              data-testid="request-payment-retry-bolt11"
              title={retryBolt11}
            >
              Retry will republish the same invoice ({retryBolt11.slice(0, 24)}
              …)
            </p>
          ) : null}
        </div>

        <DialogFooter>
          {success ? (
            <Button
              data-testid="request-payment-done"
              onClick={() => resetAndClose(false)}
              type="button"
            >
              Done
            </Button>
          ) : (
            <>
              <Button
                data-testid="request-payment-cancel"
                disabled={busy}
                onClick={() => resetAndClose(false)}
                type="button"
                variant="outline"
              >
                Cancel
              </Button>
              {retryable ? (
                <Button
                  data-testid="request-payment-retry"
                  disabled={busy}
                  onClick={() => void runSubmit("retry_publish")}
                  type="button"
                >
                  Retry
                </Button>
              ) : (
                <Button
                  data-testid="request-payment-submit"
                  disabled={busy}
                  onClick={() => void runSubmit("mint_and_publish")}
                  type="button"
                >
                  {busy ? (
                    <>
                      <Spinner className="mr-2" />
                      Posting…
                    </>
                  ) : (
                    "Request payment"
                  )}
                </Button>
              )}
            </>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
