# buzz-mock-wallet

**Dev-only** local Nostr Wallet Connect (NIP-47) wallet for end-to-end testing.

This crate moves **no real money**. It binds loopback only, keeps an in-memory
msat ledger, and mints real bolt11 invoices with genuine preimage/hash pairs so
clients can exercise pay / receive / lookup against a live NWC URI.
