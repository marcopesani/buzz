import { useState } from "react";
import { QRCodeSVG } from "qrcode.react";
import { Copy, Unplug } from "lucide-react";
import { toast } from "sonner";

import { Button } from "@/shared/ui/button";
import { Input } from "@/shared/ui/input";
import { writeTextToClipboard } from "@/shared/lib/clipboard";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/shared/ui/dialog";
import { SettingsSectionHeader } from "@/features/settings/ui/SettingsSectionHeader";
import {
  SettingsOptionGroup,
  SettingsOptionRow,
} from "@/features/settings/ui/SettingsOptionGroup";

import { ConfirmSendDialog } from "./ConfirmSendDialog";
import { msatToSatsDisplay, satsToMsat } from "./msat";
import { useWalletStatus } from "./useWalletStatus";
import {
  linkWallet,
  unlinkWallet,
  walletPrepareSend,
  walletReceive,
  type PrepareSendQuote,
} from "./walletApi";

/**
 * Settings pane: link / unlink / status / receive / standalone send.
 * The NWC URI is accepted once into a password-masked field and never redisplayed.
 */
export function WalletSettingsCard() {
  const { status, loading, error, refresh } = useWalletStatus();
  const [uri, setUri] = useState("");
  const [linking, setLinking] = useState(false);
  const [linkError, setLinkError] = useState<string | null>(null);
  const [unlinkOpen, setUnlinkOpen] = useState(false);
  const [unlinking, setUnlinking] = useState(false);

  const [receiveSats, setReceiveSats] = useState("210");
  const [receiveMemo, setReceiveMemo] = useState("");
  const [receiveBolt11, setReceiveBolt11] = useState<string | null>(null);
  const [receiving, setReceiving] = useState(false);
  const [receiveError, setReceiveError] = useState<string | null>(null);

  const [sendBolt11, setSendBolt11] = useState("");
  const [sendAmountSats, setSendAmountSats] = useState("");
  const [quote, setQuote] = useState<PrepareSendQuote | null>(null);
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [preparing, setPreparing] = useState(false);
  const [sendError, setSendError] = useState<string | null>(null);

  const balanceSats =
    status.balanceMsat != null ? msatToSatsDisplay(status.balanceMsat) : null;

  const handleLink = async () => {
    const trimmed = uri.trim();
    if (!trimmed.startsWith("nostr+walletconnect://")) {
      setLinkError("Paste a nostr+walletconnect:// URI");
      return;
    }
    setLinking(true);
    setLinkError(null);
    try {
      await linkWallet(trimmed);
      setUri("");
      toast.success("Wallet linked");
      await refresh();
    } catch (err) {
      setLinkError(err instanceof Error ? err.message : String(err));
    } finally {
      setLinking(false);
    }
  };

  const handleUnlink = async () => {
    setUnlinking(true);
    try {
      await unlinkWallet();
      setUnlinkOpen(false);
      setReceiveBolt11(null);
      toast.success("Wallet unlinked");
      await refresh();
    } catch (err) {
      toast.error(err instanceof Error ? err.message : String(err));
    } finally {
      setUnlinking(false);
    }
  };

  const handleReceive = async () => {
    const sats = Number(receiveSats);
    if (!Number.isFinite(sats) || sats <= 0) {
      setReceiveError("Enter a positive sats amount");
      return;
    }
    setReceiving(true);
    setReceiveError(null);
    try {
      const invoice = await walletReceive(
        satsToMsat(sats),
        receiveMemo.trim() || null,
      );
      setReceiveBolt11(invoice.bolt11);
    } catch (err) {
      setReceiveError(err instanceof Error ? err.message : String(err));
    } finally {
      setReceiving(false);
    }
  };

  const handlePrepareSend = async () => {
    const invoice = sendBolt11.trim();
    if (!invoice) {
      setSendError("Paste a bolt11 invoice");
      return;
    }
    const sats = Number(sendAmountSats);
    if (!Number.isFinite(sats) || sats <= 0) {
      setSendError("Enter the invoice amount in sats");
      return;
    }
    setPreparing(true);
    setSendError(null);
    try {
      const next = await walletPrepareSend({
        target: { type: "bolt11", invoice },
        amountMsat: satsToMsat(sats),
        attempt: { type: "standalone" },
      });
      setQuote(next);
      setConfirmOpen(true);
    } catch (err) {
      setSendError(err instanceof Error ? err.message : String(err));
    } finally {
      setPreparing(false);
    }
  };

  return (
    <section className="min-w-0 space-y-10" data-testid="settings-wallet">
      <SettingsSectionHeader
        description="Link an external Lightning wallet over Nostr Wallet Connect. Buzz never holds funds."
        title="Wallet"
      />

      {error ? (
        <p
          className="text-sm text-destructive"
          data-testid="wallet-status-error"
        >
          {error}
        </p>
      ) : null}

      {!status.linked ? (
        <SettingsOptionGroup>
          <SettingsOptionRow className="items-start py-4">
            <div className="min-w-0 flex-1 space-y-3">
              <div>
                <p className="text-sm font-medium">Link wallet</p>
                <p className="text-sm text-muted-foreground">
                  Paste a nostr+walletconnect:// URI. It is stored in the native
                  secure store and never shown again.
                </p>
              </div>
              <Input
                autoComplete="off"
                className="max-w-md"
                data-testid="wallet-link-uri"
                onChange={(e) => setUri(e.target.value)}
                placeholder="nostr+walletconnect://…"
                spellCheck={false}
                type="password"
                value={uri}
              />
              {linkError ? (
                <p className="text-xs text-destructive">{linkError}</p>
              ) : null}
              <Button
                data-testid="wallet-link-submit"
                disabled={linking || !uri.trim()}
                onClick={() => void handleLink()}
                type="button"
              >
                {linking ? "Linking…" : "Link"}
              </Button>
            </div>
          </SettingsOptionRow>
        </SettingsOptionGroup>
      ) : (
        <>
          <SettingsOptionGroup data-testid="wallet-linked-status">
            <SettingsOptionRow>
              <div className="min-w-0">
                <p className="text-sm font-medium">Status</p>
                <p className="text-sm text-muted-foreground">
                  {loading
                    ? "Refreshing…"
                    : status.lud16
                      ? `Lightning address ${status.lud16}`
                      : "Linked via Nostr Wallet Connect"}
                </p>
              </div>
              <span
                className="text-sm font-medium text-emerald-600 dark:text-emerald-400"
                data-testid="wallet-linked-badge"
              >
                Linked
              </span>
            </SettingsOptionRow>
            <SettingsOptionRow>
              <div className="min-w-0">
                <p className="text-sm font-medium">Capabilities</p>
                <p
                  className="text-sm text-muted-foreground"
                  data-testid="wallet-capabilities"
                >
                  {status.capabilities.join(", ") || "None reported"}
                </p>
              </div>
              <span className="text-sm text-muted-foreground">
                {status.receiveMode}
              </span>
            </SettingsOptionRow>
            <SettingsOptionRow>
              <div className="min-w-0">
                <p className="text-sm font-medium">Balance</p>
                <p className="text-sm text-muted-foreground">
                  Shown in sats when the wallet exposes get_balance
                </p>
              </div>
              <span
                className="text-sm font-medium"
                data-testid="wallet-balance"
              >
                {balanceSats != null
                  ? `${balanceSats.toLocaleString()} sats`
                  : "Hidden"}
              </span>
            </SettingsOptionRow>
            <SettingsOptionRow>
              <div className="min-w-0">
                <p className="text-sm font-medium">Unlink</p>
                <p className="text-sm text-muted-foreground">
                  Removes the NWC secret for this community.
                </p>
              </div>
              <Button
                data-testid="wallet-unlink"
                onClick={() => setUnlinkOpen(true)}
                type="button"
                variant="outline"
              >
                <Unplug className="mr-2 h-4 w-4" />
                Unlink
              </Button>
            </SettingsOptionRow>
          </SettingsOptionGroup>

          <SettingsOptionGroup>
            <SettingsOptionRow className="items-start py-4">
              <div className="min-w-0 flex-1 space-y-3">
                <div>
                  <p className="text-sm font-medium">Receive</p>
                  <p className="text-sm text-muted-foreground">
                    Mint an invoice with your linked wallet (amount in sats).
                  </p>
                </div>
                <Input
                  className="max-w-xs"
                  data-testid="wallet-receive-amount"
                  inputMode="numeric"
                  onChange={(e) => setReceiveSats(e.target.value)}
                  placeholder="Amount (sats)"
                  value={receiveSats}
                />
                <Input
                  className="max-w-md"
                  data-testid="wallet-receive-memo"
                  onChange={(e) => setReceiveMemo(e.target.value)}
                  placeholder="Description (optional)"
                  value={receiveMemo}
                />
                <Button
                  data-testid="wallet-receive-submit"
                  disabled={receiving}
                  onClick={() => void handleReceive()}
                  type="button"
                >
                  {receiving ? "Creating…" : "Create invoice"}
                </Button>
                {receiveError ? (
                  <p className="text-xs text-destructive">{receiveError}</p>
                ) : null}
              </div>
            </SettingsOptionRow>
            {receiveBolt11 ? (
              <div
                className="flex flex-col items-center gap-3 border-t border-border/50 px-4 py-4"
                data-testid="wallet-receive-invoice"
              >
                <QRCodeSVG
                  data-testid="wallet-receive-qr"
                  size={180}
                  value={receiveBolt11}
                />
                <p className="max-w-full break-all text-center text-2xs text-muted-foreground">
                  {receiveBolt11}
                </p>
                <Button
                  data-testid="wallet-receive-copy"
                  onClick={async () => {
                    await writeTextToClipboard(receiveBolt11);
                    toast.success("Invoice copied");
                  }}
                  size="sm"
                  type="button"
                  variant="outline"
                >
                  <Copy className="mr-2 h-3.5 w-3.5" />
                  Copy bolt11
                </Button>
              </div>
            ) : null}
          </SettingsOptionGroup>

          <SettingsOptionGroup>
            <SettingsOptionRow className="items-start py-4">
              <div className="min-w-0 flex-1 space-y-3">
                <div>
                  <p className="text-sm font-medium">Send</p>
                  <p className="text-sm text-muted-foreground">
                    Paste a bolt11 invoice. Confirmation is always required
                    before spend.
                  </p>
                </div>
                <Input
                  className="max-w-md"
                  data-testid="wallet-send-bolt11"
                  onChange={(e) => setSendBolt11(e.target.value)}
                  placeholder="lnbc…"
                  spellCheck={false}
                  value={sendBolt11}
                />
                <Input
                  className="max-w-xs"
                  data-testid="wallet-send-amount"
                  inputMode="numeric"
                  onChange={(e) => setSendAmountSats(e.target.value)}
                  placeholder="Amount (sats)"
                  value={sendAmountSats}
                />
                {sendError ? (
                  <p className="text-xs text-destructive">{sendError}</p>
                ) : null}
                <Button
                  data-testid="wallet-send-prepare"
                  disabled={preparing}
                  onClick={() => void handlePrepareSend()}
                  type="button"
                >
                  {preparing ? "Preparing…" : "Review payment"}
                </Button>
              </div>
            </SettingsOptionRow>
          </SettingsOptionGroup>
        </>
      )}

      <Dialog open={unlinkOpen} onOpenChange={setUnlinkOpen}>
        <DialogContent data-testid="wallet-unlink-confirm">
          <DialogHeader>
            <DialogTitle>Unlink wallet?</DialogTitle>
            <DialogDescription>
              This removes the NWC connection for this community. Your external
              wallet is unchanged.
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button
              onClick={() => setUnlinkOpen(false)}
              type="button"
              variant="outline"
            >
              Cancel
            </Button>
            <Button
              data-testid="wallet-unlink-confirm-submit"
              disabled={unlinking}
              onClick={() => void handleUnlink()}
              type="button"
              variant="destructive"
            >
              {unlinking ? "Unlinking…" : "Unlink"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <ConfirmSendDialog
        onOpenChange={setConfirmOpen}
        open={confirmOpen}
        quote={quote}
      />
    </section>
  );
}
