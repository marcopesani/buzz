//! Tauri IPC skin for the Lightning wallet — no payment decisions here.

use tauri::State;

use crate::wallet::{
    AttemptKeyDto, CheckIncomingDto, IncomingCheckOutcome, PrepareSendQuote, ReceiveInvoice,
    SendConfirmOutcome, SendTargetDto, SettledPayRequestDto, WalletManager, WalletStatusView,
};

/// Link an NWC wallet. The URI is accepted once and never returned.
#[tauri::command]
pub async fn link_wallet(
    uri: String,
    manager: State<'_, WalletManager>,
) -> Result<WalletStatusView, String> {
    manager.link_wallet(uri).await
}

/// Unlink the active community's wallet.
#[tauri::command]
pub async fn unlink_wallet(manager: State<'_, WalletManager>) -> Result<(), String> {
    manager.unlink_wallet().await
}

/// Linked status / capabilities / receive mode — never secret material.
#[tauri::command]
pub async fn wallet_status(manager: State<'_, WalletManager>) -> Result<WalletStatusView, String> {
    manager.wallet_status().await
}

/// Mint a receive invoice (msat) — returns bolt11, payment_hash, expiry.
#[tauri::command]
pub async fn wallet_receive(
    amount_msat: u64,
    description: Option<String>,
    manager: State<'_, WalletManager>,
) -> Result<ReceiveInvoice, String> {
    manager.wallet_receive(amount_msat, description).await
}

/// Prepare a send; returns a quote with an opaque handle id.
#[tauri::command]
pub async fn wallet_prepare_send(
    target: SendTargetDto,
    amount_msat: u64,
    attempt: AttemptKeyDto,
    memo: Option<String>,
    manager: State<'_, WalletManager>,
) -> Result<PrepareSendQuote, String> {
    manager
        .wallet_prepare_send(target, amount_msat, attempt, memo)
        .await
}

/// Confirm a prepared send by opaque handle id.
#[tauri::command]
pub async fn wallet_confirm(
    handle_id: String,
    manager: State<'_, WalletManager>,
) -> Result<SendConfirmOutcome, String> {
    manager.wallet_confirm(handle_id).await
}

/// Cancel a prepared send by opaque handle id.
#[tauri::command]
pub async fn wallet_cancel(
    handle_id: String,
    manager: State<'_, WalletManager>,
) -> Result<(), String> {
    manager.wallet_cancel(handle_id).await
}

/// Drain Paying/Unknown records via lookup_invoice.
///
/// Returns newly settled pay-request attempts (for receipt publishing).
#[tauri::command]
pub async fn wallet_reconcile(
    manager: State<'_, WalletManager>,
) -> Result<Vec<SettledPayRequestDto>, String> {
    manager.wallet_reconcile().await
}

/// Confirm an incoming payment request against this wallet only.
#[tauri::command]
pub async fn wallet_check_incoming(
    bolt11: Option<String>,
    lud16: Option<String>,
    manager: State<'_, WalletManager>,
) -> Result<IncomingCheckOutcome, String> {
    manager
        .wallet_check_incoming(CheckIncomingDto { bolt11, lud16 })
        .await
}
