//! Scriptable [`WalletService`](crate::ports::WalletService) with a call log.

use crate::error::WalletError;
use crate::ports::WalletService;
use crate::types::{Bolt11, InvoiceStatus, Tx};
use async_trait::async_trait;
use buzz_core::payment::Amount;
use std::collections::{HashMap, VecDeque};
use std::future;
use std::sync::Mutex;
use std::time::Duration;

/// One recorded [`WalletService`] call (method + args).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WalletCall {
    /// `get_balance`
    GetBalance,
    /// `make_invoice`
    MakeInvoice {
        /// Requested amount.
        amount: Amount,
        /// Optional memo.
        memo: Option<String>,
    },
    /// `pay_invoice`
    PayInvoice {
        /// Opaque bolt11 paid.
        bolt11: Bolt11,
    },
    /// `lookup_invoice`
    LookupInvoice {
        /// Hex payment hash looked up.
        payment_hash: String,
    },
    /// `list_transactions`
    ListTransactions,
}

/// Scripted outcome for one `pay_invoice` call.
#[derive(Debug, Clone)]
pub enum PayScript {
    /// Settle successfully with this hex preimage.
    Settle {
        /// Hex-encoded 32-byte preimage.
        preimage: String,
    },
    /// Fail with this exact error.
    Fail(WalletError),
    /// Sleep `duration` (honours `tokio::time::pause`), then run `then`.
    Delay {
        /// How long to wait before continuing.
        duration: Duration,
        /// Outcome after the delay.
        then: Box<PayScript>,
    },
    /// Pending future that never resolves.
    NeverRespond,
}

/// Scripted outcome for one `make_invoice` call.
#[derive(Debug, Clone)]
pub enum MakeInvoiceScript {
    /// Return this bolt11.
    Ok(Bolt11),
    /// Fail with this exact error.
    Fail(WalletError),
    /// Sleep then continue.
    Delay {
        /// How long to wait before continuing.
        duration: Duration,
        /// Outcome after the delay.
        then: Box<MakeInvoiceScript>,
    },
    /// Pending future that never resolves.
    NeverRespond,
}

/// Scripted outcome for one `lookup_invoice` call.
#[derive(Debug, Clone)]
pub enum InvoiceScript {
    /// Return this status.
    Status(InvoiceStatus),
    /// Fail with a transport-level error.
    Fail(WalletError),
    /// Sleep then continue.
    Delay {
        /// How long to wait before continuing.
        duration: Duration,
        /// Outcome after the delay.
        then: Box<InvoiceScript>,
    },
    /// Pending future that never resolves.
    NeverRespond,
}

struct Scripts {
    pay: VecDeque<PayScript>,
    make: VecDeque<MakeInvoiceScript>,
    /// Per-hash lookup queues; empty key `""` is the default queue.
    lookup: HashMap<String, VecDeque<InvoiceScript>>,
    balance: Option<Amount>,
    transactions: Vec<Tx>,
}

/// Fake wallet: scripted per scenario, records every call.
pub struct FakeWalletService {
    calls: Mutex<Vec<WalletCall>>,
    scripts: Mutex<Scripts>,
}

impl std::fmt::Debug for FakeWalletService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeWalletService")
            .field("calls", &self.calls())
            .finish_non_exhaustive()
    }
}

impl FakeWalletService {
    /// Empty fake — each call must be scripted (or use defaults for balance/list).
    pub fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            scripts: Mutex::new(Scripts {
                pay: VecDeque::new(),
                make: VecDeque::new(),
                lookup: HashMap::new(),
                balance: None,
                transactions: Vec::new(),
            }),
        }
    }

    /// Queue a `pay_invoice` outcome (FIFO).
    pub fn script_pay(&self, script: PayScript) {
        self.scripts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pay
            .push_back(script);
    }

    /// Queue a `make_invoice` outcome (FIFO).
    pub fn script_make_invoice(&self, script: MakeInvoiceScript) {
        self.scripts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .make
            .push_back(script);
    }

    /// Queue a `lookup_invoice` outcome for `payment_hash` (FIFO per hash).
    pub fn script_lookup(&self, payment_hash: impl Into<String>, script: InvoiceScript) {
        self.scripts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .lookup
            .entry(payment_hash.into())
            .or_default()
            .push_back(script);
    }

    /// Set the balance returned by `get_balance` (`None` = wallet hides it).
    pub fn set_balance(&self, balance: Option<Amount>) {
        self.scripts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .balance = balance;
    }

    /// Set the list returned by `list_transactions`.
    pub fn set_transactions(&self, txs: Vec<Tx>) {
        self.scripts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .transactions = txs;
    }

    /// Snapshot of every recorded call, in order.
    pub fn calls(&self) -> Vec<WalletCall> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Count of recorded calls equal to `call`.
    pub fn call_count(&self, call: &WalletCall) -> usize {
        self.calls().iter().filter(|c| *c == call).count()
    }

    fn record(&self, call: WalletCall) {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(call);
    }

    fn pop_pay(&self) -> PayScript {
        self.scripts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pay
            .pop_front()
            .unwrap_or_else(|| {
                panic!("FakeWalletService: pay_invoice called with no script queued")
            })
    }

    fn pop_make(&self) -> MakeInvoiceScript {
        self.scripts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .make
            .pop_front()
            .unwrap_or_else(|| {
                panic!("FakeWalletService: make_invoice called with no script queued")
            })
    }

    fn pop_lookup(&self, payment_hash: &str) -> InvoiceScript {
        let mut scripts = self.scripts.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(q) = scripts.lookup.get_mut(payment_hash) {
            if let Some(s) = q.pop_front() {
                return s;
            }
        }
        panic!("FakeWalletService: lookup_invoice({payment_hash}) called with no script queued");
    }
}

impl Default for FakeWalletService {
    fn default() -> Self {
        Self::new()
    }
}

async fn run_pay(script: PayScript) -> Result<String, WalletError> {
    match script {
        PayScript::Settle { preimage } => Ok(preimage),
        PayScript::Fail(err) => Err(err),
        PayScript::Delay { duration, then } => {
            tokio::time::sleep(duration).await;
            Box::pin(run_pay(*then)).await
        }
        PayScript::NeverRespond => {
            future::pending::<()>().await;
            unreachable!("NeverRespond pending resolved")
        }
    }
}

async fn run_make(script: MakeInvoiceScript) -> Result<Bolt11, WalletError> {
    match script {
        MakeInvoiceScript::Ok(bolt11) => Ok(bolt11),
        MakeInvoiceScript::Fail(err) => Err(err),
        MakeInvoiceScript::Delay { duration, then } => {
            tokio::time::sleep(duration).await;
            Box::pin(run_make(*then)).await
        }
        MakeInvoiceScript::NeverRespond => {
            future::pending::<()>().await;
            unreachable!("NeverRespond pending resolved")
        }
    }
}

async fn run_lookup(script: InvoiceScript) -> Result<InvoiceStatus, WalletError> {
    match script {
        InvoiceScript::Status(status) => Ok(status),
        InvoiceScript::Fail(err) => Err(err),
        InvoiceScript::Delay { duration, then } => {
            tokio::time::sleep(duration).await;
            Box::pin(run_lookup(*then)).await
        }
        InvoiceScript::NeverRespond => {
            future::pending::<()>().await;
            unreachable!("NeverRespond pending resolved")
        }
    }
}

#[async_trait]
impl WalletService for FakeWalletService {
    async fn get_balance(&self) -> Result<Option<Amount>, WalletError> {
        self.record(WalletCall::GetBalance);
        let balance = self
            .scripts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .balance;
        Ok(balance)
    }

    async fn make_invoice(
        &self,
        amount: Amount,
        memo: Option<&str>,
    ) -> Result<Bolt11, WalletError> {
        self.record(WalletCall::MakeInvoice {
            amount,
            memo: memo.map(str::to_string),
        });
        let script = self.pop_make();
        run_make(script).await
    }

    async fn pay_invoice(&self, bolt11: &Bolt11) -> Result<String, WalletError> {
        self.record(WalletCall::PayInvoice {
            bolt11: bolt11.clone(),
        });
        let script = self.pop_pay();
        run_pay(script).await
    }

    async fn lookup_invoice(&self, payment_hash: &str) -> Result<InvoiceStatus, WalletError> {
        self.record(WalletCall::LookupInvoice {
            payment_hash: payment_hash.to_string(),
        });
        let script = self.pop_lookup(payment_hash);
        run_lookup(script).await
    }

    async fn list_transactions(&self) -> Result<Vec<Tx>, WalletError> {
        self.record(WalletCall::ListTransactions);
        let txs = self
            .scripts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .transactions
            .clone();
        Ok(txs)
    }
}
