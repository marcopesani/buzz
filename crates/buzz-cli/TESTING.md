# buzz-cli Live Testing Guide

Manual testing runbook for verifying every CLI command against a local relay.
An agent or developer follows this step by step, running each command and
checking the output.

---

## 1. Prerequisites

Docker services running and healthy:

```bash
docker compose ps
# buzz-postgres   healthy
# buzz-redis      healthy
```

If not running: `just setup` from the repo root.

Tools: `jq`, `curl`, Rust toolchain.

---

## 2. Build the CLI

```bash
cargo build -p buzz-cli
```

Use `cargo run -p buzz-cli --` or the built binary at `target/debug/buzz`.

---

## 3. Start the Relay

In a separate terminal:

```bash
cd REPOS/buzz-nostr
set -a && source .env && set +a
cargo run -p buzz-relay
```

Verify:

```bash
curl -s http://localhost:3000/_liveness
# "ok" or 200 status
```

The `.env` should have `BUZZ_REQUIRE_AUTH_TOKEN=false` for local dev.

---

## 4. Mint Test Credentials

### Option A: buzz-admin (full scopes including admin)

This mints a token with all CLI-relevant scopes (including `admin:channels`)
via direct DB access. Use this for testing admin operations (archive,
delete-channel, add/remove-channel-member).

```bash
DATABASE_URL="${DATABASE_URL:?set DATABASE_URL for the local Buzz database}" \
cargo run -p buzz-admin -- mint-token \
  --name "cli-test" \
  --scopes "messages:read,messages:write,channels:read,channels:write,users:read,users:write,files:read,files:write,admin:channels"
```

This generates a keypair and prints:
- **Private key (nsec)** — save for `BUZZ_PRIVATE_KEY` testing

Export:

```bash
export BUZZ_RELAY_URL="http://localhost:3000"
export BUZZ_PRIVATE_KEY="nsec1..."   # from the mint output
```

### Scope reference

| Scope | Self-mintable | Needed for |
|-------|:---:|------------|
| `messages:read` | ✅ | `messages get`, `messages thread`, `messages search`, `feed get` |
| `messages:write` | ✅ | `messages send`, `messages edit`, `messages delete`, `reactions`, `messages vote` |
| `channels:read` | ✅ | `channels list`, `channels get`, `channels members` |
| `channels:write` | ✅ | `channels create`, `channels update`, `channels join`, `channels leave`, `channels topic`, `channels purpose` |
| `users:read` | ✅ | `users get`, `users presence` |
| `users:write` | ✅ | `users set-profile`, `users set-presence` |
| `files:read` | ✅ | — |
| `files:write` | ✅ | — |
| `admin:channels` | ❌ | `channels archive`, `channels unarchive`, `channels delete`, `channels add-member`, `channels remove-member` |

---

## 5. Unit Tests

```bash
cargo test -p buzz-cli
# Expected: see cargo test -p buzz-cli for current count

cargo clippy -p buzz-cli -- -D warnings
# Expected: zero warnings
```

---

## 6. Live Testing — Command by Command

Run each command, verify exit code 0 and check output. Most commands
return JSON (pipe through `jq .` to validate). Commands are ordered so
earlier ones create resources that later ones need.

### 6.1 Channels

```bash
# channels create (stream)
buzz channels create --name "test-stream" --type stream --visibility open \
  --description "CLI test channel" | jq .
# Save the channel ID:
CHANNEL_ID=$(buzz channels create --name "test-cli" --type stream --visibility open | jq -r '.channel_id')
# Expected: {"event_id":"...","accepted":true,"message":"...","channel_id":"<uuid>"}

# channels create (forum) — needed for messages vote later
FORUM_ID=$(buzz channels create --name "test-forum" --type forum --visibility open | jq -r '.channel_id')

# channels list
buzz channels list | jq .
# Expected: [{"channel_id":"...","name":"...","description":"...","created_at":N}]
buzz channels list --visibility open | jq .
buzz channels list --member | jq .

# channels get
buzz channels get --channel "$CHANNEL_ID" | jq .
# Expected: {"channel_id":"...","name":"...","description":"...","created_at":N,"pubkey":"..."} or null

# channels update
buzz channels update --channel "$CHANNEL_ID" --name "test-cli-updated" \
  --description "Updated" | jq .
# Expected: {"event_id":"...","accepted":true,"message":"..."}

# channels topic
buzz channels topic --channel "$CHANNEL_ID" --topic "Test topic" | jq .
# Expected: {"event_id":"...","accepted":true,"message":"..."}

# channels purpose
buzz channels purpose --channel "$CHANNEL_ID" --purpose "Testing" | jq .
# Expected: {"event_id":"...","accepted":true,"message":"..."}

# channels join (may already be a member from create)
buzz channels join --channel "$CHANNEL_ID" | jq .
# Expected: {"event_id":"...","accepted":true,"message":"..."}

# channels leave
# NOTE: Fails with 400 "cannot remove the last owner" if this identity is the
# sole owner (which it is after channels create). To test leave successfully,
# first add-member a second pubkey as owner. The relay enforces ≥1 owner.
buzz channels leave --channel "$CHANNEL_ID" | jq .
# Expected: {"event_id":"...","accepted":true,"message":"..."} (or 400 if last owner)

# Re-join so we can send messages
buzz channels join --channel "$CHANNEL_ID" | jq .
# Expected: {"event_id":"...","accepted":true,"message":"..."}

# channels archive (requires admin:channels scope)
buzz channels archive --channel "$CHANNEL_ID" | jq .
# Expected: {"event_id":"...","accepted":true,"message":"..."}

# channels unarchive
buzz channels unarchive --channel "$CHANNEL_ID" | jq .
# Expected: {"event_id":"...","accepted":true,"message":"..."}
```

### 6.2 Canvas

```bash
# canvas set
buzz canvas set --channel "$CHANNEL_ID" --content "# Test Canvas" | jq .

# canvas set from stdin
echo "# Canvas from stdin" | buzz canvas set --channel "$CHANNEL_ID" --content - | jq .

# canvas get
buzz canvas get --channel "$CHANNEL_ID"
# Expected: raw markdown string, or: null
```

### 6.3 Messages

```bash
# messages send
MSG=$(buzz messages send --channel "$CHANNEL_ID" --content "Hello from CLI test" | jq .)
echo "$MSG"
EVENT_ID=$(echo "$MSG" | jq -r '.event_id')

# messages send with reply + broadcast
REPLY=$(buzz messages send --channel "$CHANNEL_ID" --content "Reply" \
  --reply-to "$EVENT_ID" --broadcast | jq .)
echo "$REPLY"
REPLY_ID=$(echo "$REPLY" | jq -r '.event_id')

# messages send with mentions — @name in content is auto-resolved, no flag needed
buzz messages send --channel "$CHANNEL_ID" --content "Hey @someone" | jq .

# messages send with NIP-27 nostr:npub1… inline mention — auto-resolved to p-tag
buzz messages send --channel "$CHANNEL_ID" \
  --content "Check with nostr:npub10elfcs4fr0l0r8af98jlmgdh9c8tcxjvz9qkw038js35mp4dma8qzvjptg on this" | jq .

# messages send from stdin — safe path for content with shell metacharacters
# (backticks, $vars, code blocks) that would otherwise be expanded by the shell.
echo 'Body with `backticks` and $vars stays literal.' \
  | buzz messages send --channel "$CHANNEL_ID" --content - | jq .

# messages get
buzz messages get --channel "$CHANNEL_ID" | jq .
buzz messages get --channel "$CHANNEL_ID" --limit 5 | jq .

# messages thread
buzz messages thread --channel "$CHANNEL_ID" --event "$EVENT_ID" | jq .

# messages search
buzz messages search --query "Hello" | jq .
buzz messages search --query "CLI test" --limit 5 | jq .

# messages edit
buzz messages edit --event "$EVENT_ID" --content "Edited by CLI test" | jq .

# messages delete
buzz messages delete --event "$REPLY_ID" | jq .
```

### 6.4 Diff Messages

```bash
# messages send-diff from stdin
echo '--- a/foo.rs
+++ b/foo.rs
@@ -1,3 +1,3 @@
-fn old() {}
+fn new() {}' | buzz messages send-diff \
  --channel "$CHANNEL_ID" \
  --diff - \
  --repo "https://github.com/example/repo" \
  --commit "abcdef1234567890abcdef1234567890abcdef12" | jq .

# messages send-diff with metadata
echo "diff content" | buzz messages send-diff \
  --channel "$CHANNEL_ID" \
  --diff - \
  --repo "https://github.com/example/repo" \
  --commit "abcdef1234567890abcdef1234567890abcdef12" \
  --file "src/main.rs" \
  --lang "rust" \
  --description "Refactored main" | jq .

# messages send-diff with branch + PR metadata
echo "diff content" | buzz messages send-diff \
  --channel "$CHANNEL_ID" \
  --diff - \
  --repo "https://github.com/example/repo" \
  --commit "abcdef1234567890abcdef1234567890abcdef12" \
  --parent-commit "1234567890abcdef1234567890abcdef12345678" \
  --source-branch "feature/cli" \
  --target-branch "main" \
  --pr 42 | jq .
```

### 6.5 Reactions

```bash
# Send a message to react to
REACT_MSG=$(buzz messages send --channel "$CHANNEL_ID" --content "React to this")
REACT_ID=$(echo "$REACT_MSG" | jq -r '.event_id')

# reactions add
buzz reactions add --event "$REACT_ID" --emoji "👍" | jq .

# reactions get
buzz reactions get --event "$REACT_ID" | jq .
# Expected: {"reactions":[{"emoji":"...","count":N,"pubkeys":["..."]}]}

# reactions remove
buzz reactions remove --event "$REACT_ID" --emoji "👍" | jq .
```

### 6.6 DMs

```bash
# dms list
buzz dms list | jq .
# Expected: [{"dm_id":"...","participants":["..."],"created_at":N}]

# dms open (needs a real pubkey — use your own or a test one)
# Get your own pubkey first:
MY_PUBKEY=$(buzz users get | jq -r '.[0].pubkey // empty')
echo "My pubkey: $MY_PUBKEY"

# dms open with a synthetic pubkey (relay will create the user)
DM_RESULT=$(buzz dms open --pubkey "0000000000000000000000000000000000000000000000000000000000000001")
echo "$DM_RESULT" | jq .
# Expected: {"event_id":"...","accepted":true,"message":"...","dm_id":"<uuid>"}
DM_ID=$(echo "$DM_RESULT" | jq -r '.dm_id')

# dms add-member (requires messages:write scope — NOT admin:channels)
buzz dms add-member --channel "$DM_ID" \
  --pubkey "0000000000000000000000000000000000000000000000000000000000000002" | jq .
```

### 6.7 Users & Presence

```bash
# users get — own profile (0 pubkeys)
buzz users get | jq .
# Expected: [{...profile...}] — always returns an array, even for single results

# users get — single pubkey
buzz users get --pubkey "$MY_PUBKEY" | jq .

# users get — batch (2+ pubkeys)
buzz users get --pubkey "$MY_PUBKEY" --pubkey "$MY_PUBKEY" | jq .

# users set-profile
buzz users set-profile --name "CLI Test Agent" --about "Testing buzz-cli" | jq .

# users presence
buzz users presence --pubkeys "$MY_PUBKEY" | jq .

# users set-presence
buzz users set-presence --status online | jq .
buzz users set-presence --status away | jq .
buzz users set-presence --status offline | jq .
# Note: set-presence may fail — kind:20001 is ephemeral and rejected by the HTTP bridge
```

### 6.8 Channel Members (add/remove require admin:channels)

```bash
# channels add-member
buzz channels add-member --channel "$CHANNEL_ID" \
  --pubkey "0000000000000000000000000000000000000000000000000000000000000001" \
  --role member | jq .

# channels members
buzz channels members --channel "$CHANNEL_ID" | jq .
# Expected: [{"pubkey":"...","role":"..."}]

# channels remove-member
buzz channels remove-member --channel "$CHANNEL_ID" \
  --pubkey "0000000000000000000000000000000000000000000000000000000000000001" | jq .
```

### 6.9 Workflows

```bash
# workflows create
# NOTE: trigger uses `on:` tag (serde internally tagged enum).
# Valid triggers: message_posted, reaction_added, diff_posted, schedule, webhook
# Steps use `action:` tag: send_message, send_dm, set_channel_topic, add_reaction, etc.
WF=$(buzz workflows create --channel "$CHANNEL_ID" \
  --yaml 'name: test-wf
trigger:
  on: webhook
steps:
  - id: step1
    action: send_message
    text: "Hello from workflow"' | jq .)
echo "$WF"
WF_ID=$(echo "$WF" | jq -r '.workflow_id')

# workflows list
buzz workflows list --channel "$CHANNEL_ID" | jq .

# workflows get
buzz workflows get --workflow "$WF_ID" | jq .
# Expected: {"workflow_id":"...","content":"<yaml>","created_at":N,"pubkey":"..."} or null

# workflows update (requires --channel)
buzz workflows update --channel "$CHANNEL_ID" --workflow "$WF_ID" \
  --yaml 'name: test-wf-updated
trigger:
  on: webhook
steps:
  - id: step1
    action: send_message
    text: "Updated"' | jq .

# workflows trigger
# NOTE: May return 400 "workflow not found" — the relay indexes workflow
# definitions into a DB table asynchronously. If the definition event hasn't
# been indexed yet, the trigger handler won't find it.
buzz workflows trigger --workflow "$WF_ID" | jq .

# workflows runs
buzz workflows runs --workflow "$WF_ID" | jq .
# Expected: [] — relay stores runs in DB, not as Nostr events; empty is normal

# workflows approve — requires a workflow run waiting for approval
# This is hard to test ad-hoc without a workflow that has an approval gate.
# Test the validation instead:
buzz workflows approve --token "00000000-0000-0000-0000-000000000000" 2>&1 || true
# Should fail with relay error (token not found), not a validation error
# To test the deny path: buzz workflows approve --token <UUID> --approved false

# workflows delete
buzz workflows delete --workflow "$WF_ID" | jq .
```

### 6.10 Feed

```bash
buzz feed get | jq .
buzz feed get --limit 5 | jq .
# Expected: [{id,pubkey,kind,content,created_at,tags}] — sig-stripped, sorted newest-first
```

### 6.11 Forum & Voting

```bash
# Send a forum post (kind 45001) to the forum channel
FORUM_POST=$(buzz messages send --channel "$FORUM_ID" \
  --content "Forum post for vote testing" --kind 45001 | jq .)
echo "$FORUM_POST"
FORUM_EVENT_ID=$(echo "$FORUM_POST" | jq -r '.event_id')

# messages vote (up)
buzz messages vote --event "$FORUM_EVENT_ID" --direction up | jq .

# messages vote (down)
buzz messages vote --event "$FORUM_EVENT_ID" --direction down | jq .
```

### 6.12 Notes (NIP-23 long-form, kind:30023)

Editable team-knowledge notes keyed by `(kind:30023, you, d=slug)`. `set` is an
idempotent upsert; `rm` is a NIP-09 a-tag deletion. Output is plain text (refs),
not JSON — except `get`/`ls`, which emit JSON.

```bash
# set (first publish — --title required, body from stdin)
cat <<'EOF' | buzz notes set --name dco-check --title "DCO Check" \
  --summary "How we verify DCO" --tag dco --tag ci --content -
Run `git log --format='%(trailers:key=Signed-off-by)'` ...
EOF
# → prints event_id / naddr / coordinate / slug / title

# set (edit — omit --title to carry it forward; published_at preserved)
echo "Updated body." | buzz notes set --name dco-check --content -

# get by name (own author resolves directly; cross-author #d query otherwise)
buzz notes get --name dco-check | jq .
buzz notes get --name dco-check --content-only

# get by naddr (exact coordinate; paste the naddr from a set/get above)
buzz notes get --naddr "$NADDR" | jq .

# ls (own by default; --author all across the team; --tag filters)
buzz notes ls | jq .
buzz notes ls --tag dco | jq .
buzz notes ls --author all --limit 10 | jq .

# rm (NIP-09 a-tag deletion; subsequent get must 404)
buzz notes rm --name dco-check
# → prints deleted <coordinate> / deletion <event-id>
buzz notes get --name dco-check   # exits non-zero: not found

# rm of a slug you never published → NotFound, no kind:5 emitted
buzz notes rm --name does-not-exist   # exits non-zero
```

---

## 7. Error Path Testing

Verify the CLI produces correct JSON on stderr and correct exit codes.

```bash
# Exit 1: Invalid UUID
buzz channels get --channel "not-a-uuid" 2>&1; echo "exit: $?"
# stderr: {"error":"user_error","message":"invalid UUID: not-a-uuid"}
# exit: 1

# Exit 1: Invalid hex64
buzz messages delete --event "not-hex" 2>&1; echo "exit: $?"
# stderr: {"error":"user_error","message":"must be a 64-character hex string: not-hex"}
# exit: 1

# Exit 1: Invalid --type value (clap validates the enum — multi-line error)
buzz channels create --name x --type invalid --visibility open 2>&1; echo "exit: $?"
# stderr: {"error":"user_error","message":"error: invalid value 'invalid' for '--type <CHANNEL_TYPE>'\n  [possible values: stream, forum]\n..."}
# exit: 1

# Exit 1: Invalid --direction value
buzz messages vote --event "$(printf '0%.0s' {1..64})" \
  --direction sideways 2>&1; echo "exit: $?"
# exit: 1

# Exit 1: Empty body guard
buzz users set-profile 2>&1; echo "exit: $?"
# exit: 1 (at least one field required)

# Exit 3: No auth configured
env -u BUZZ_PRIVATE_KEY \
  cargo run -p buzz-cli -- channels list 2>&1; echo "exit: $?"
# stderr: {"error":"auth_error","message":"auth error: BUZZ_PRIVATE_KEY is required (use --private-key or set env var)"}
# exit: 3

# Not-found returns null, not an error (exit 0)
buzz channels get --channel "00000000-0000-0000-0000-000000000000"
# stdout: null
# exit: 0
```

---

## 8. Auth Testing

Test authentication.

```bash
# Private key (BUZZ_PRIVATE_KEY)
BUZZ_PRIVATE_KEY="nsec1..." buzz channels list | jq .
# Should succeed

# No auth → exit 3
env -u BUZZ_PRIVATE_KEY \
  cargo run -p buzz-cli -- channels list 2>&1; echo "exit: $?"
# stderr: {"error":"auth_error","message":"auth error: BUZZ_PRIVATE_KEY is required (use --private-key or set env var)"}
# exit: 3
```

---

## 9. Cleanup

```bash
# Delete test channels
buzz channels delete --channel "$CHANNEL_ID" | jq .
buzz channels delete --channel "$FORUM_ID" | jq .
```

---

## 10. Checklist

| # | Command | Tested | Notes |
|---|---------|:------:|-------|
| 1 | `messages send` | ☐ | Basic, reply, broadcast, mentions, stdin |
| 2 | `messages send-diff` | ☐ | Stdin, metadata, branch/PR |
| 3 | `messages edit` | ☐ | |
| 4 | `messages delete` | ☐ | |
| 5 | `messages get` | ☐ | With limit |
| 6 | `messages thread` | ☐ | |
| 7 | `messages search` | ☐ | With limit |
| 8 | `messages vote` | ☐ | Up and down |
| 9 | `channels list` | ☐ | With visibility, member |
| 10 | `channels get` | ☐ | |
| 11 | `channels create` | ☐ | Stream and forum |
| 12 | `channels update` | ☐ | |
| 13 | `channels topic` | ☐ | |
| 14 | `channels purpose` | ☐ | |
| 15 | `channels join` | ☐ | |
| 16 | `channels leave` | ☐ | |
| 17 | `channels archive` | ☐ | Needs admin:channels |
| 18 | `channels unarchive` | ☐ | Needs admin:channels |
| 19 | `channels delete` | ☐ | Needs admin:channels |
| 20 | `channels members` | ☐ | |
| 21 | `channels add-member` | ☐ | Needs admin:channels |
| 22 | `channels remove-member` | ☐ | Needs admin:channels |
| 23 | `canvas get` | ☐ | |
| 24 | `canvas set` | ☐ | Direct and stdin |
| 25 | `reactions add` | ☐ | |
| 26 | `reactions remove` | ☐ | |
| 27 | `reactions get` | ☐ | |
| 28 | `dms list` | ☐ | |
| 29 | `dms open` | ☐ | |
| 30 | `dms add-member` | ☐ | Needs messages:write |
| 31 | `users get` | ☐ | Self, single, batch |
| 32 | `users set-profile` | ☐ | |
| 33 | `users presence` | ☐ | |
| 34 | `users set-presence` | ☐ | online, away, offline |
| 35 | `workflows list` | ☐ | |
| 36 | `workflows create` | ☐ | |
| 37 | `workflows update` | ☐ | |
| 38 | `workflows delete` | ☐ | |
| 39 | `workflows trigger` | ☐ | |
| 40 | `workflows runs` | ☐ | |
| 41 | `workflows get` | ☐ | |
| 42 | `workflows approve` | ☐ | Validation only (needs approval gate); bare = approve, `--approved false` = deny |
| 43 | `feed get` | ☐ | |
| 44 | `social publish` | ☐ | |
| 45 | `social set-contacts` | ☐ | |
| 46 | `social event` | ☐ | |
| 47 | `social notes` | ☐ | |
| 48 | `social contacts` | ☐ | |
| 49 | `repos create` | ☐ | |
| 50 | `repos get` | ☐ | |
| 51 | `repos list` | ☐ | |
| 52 | `repos protect list` | ☐ | Empty/populated rules; unknown rules visible; malformed rule reported in validation_error |
| 53 | `repos protect set` | ☐ | Create and replace complete exact-ref rule; verify metadata is preserved |
| 54 | `repos protect remove` | ☐ | Remove exact ref; missing rule → NotFound |
| 55 | `upload file` | ☐ | |
| 56 | `pack validate` | ☐ | Local, no relay |
| 57 | `pack inspect` | ☐ | Local, no relay |
| 58 | `notes set` | ☐ | First publish, edit/carry, --clear-tags, ambiguity, empty-stdin guard |
| 59 | `notes get` | ☐ | By name, by naddr, --content-only, cross-author, ambiguous → exit 1 |
| 60 | `notes ls` | ☐ | Own, --author all, --tag, --limit |
| 61 | `notes rm` | ☐ | Delete→get 404, double-delete idempotent, missing slug → NotFound |

## Local NWC mock wallet (`buzz-mock-wallet`)

Dev-only loopback NWC wallet for end-to-end Lightning testing. **Moves no real money.**

```bash
# From repo root (Hermit activated)
cargo run -p buzz-mock-wallet -- \
  --balance-msat 1000000 \
  --port 0
  # optional: --receive-only
  # optional: --fail-next-pay PAYMENT_FAILED
  # optional: --response-delay-ms 250
  # optional: --swallow-requests
```

On startup the process prints a ready-to-paste NWC URI on stdout, for example:

```text
nostr+walletconnect://<wallet_pubkey>?relay=ws%3A%2F%2F127.0.0.1%3A<port>&secret=<client_secret_hex>
```

Paste that URI into `buzz wallet link`, desktop link-wallet, or any NIP-47 client. The daemon speaks NIP-04 encrypted kinds `23194`/`23195`/`23196` (matching rust-nostr `nwc` 0.44), mints real bolt11 invoices, and keeps an in-memory msat ledger.

| Flag | Meaning |
|------|--------|
| `--balance-msat` | Starting ledger balance (msat) |
| `--port` | Loopback bind port (`0` = ephemeral) |
| `--receive-only` | Omit pay methods from 13194 / `get_info`; pay → `RESTRICTED` |
| `--fail-next-pay <CODE>` | Next `pay_invoice` returns that NIP-47 error code |
| `--response-delay-ms` | Fixed delay before every RPC response |
| `--swallow-requests` | Accept requests but never send 23195 (client-timeout tests) |

## `buzz wallet` (NWC) runbook

Thin CLI over `buzz-wallet`. Amounts are **msat** everywhere. The NWC secret
never appears in stdout, stderr, or error JSON.

### Env vars and secret file

| Name | Purpose |
|------|---------|
| `BUZZ_NWC_URI` | NWC connection string (takes precedence over the secret file) |
| `BUZZ_NWC_SECRET_PATH` | Override path for the durable secret JSON (default below) |
| `BUZZ_WALLET_DATA_DIR` | Override directory for per-relay payment JSON |
| `BUZZ_PRIVATE_KEY` | Required only for `request` / `pay --request` / `check` (Buzz relay) |
| `BUZZ_RELAY_URL` | Community boundary for the payment store (default `http://localhost:3000`) |

**Secret file (default):** `$XDG_CONFIG_HOME/buzz/nwc-secret.json`
(via `dirs::config_dir()` — on macOS typically
`~/Library/Application Support/buzz/nwc-secret.json`). Written by
`buzz wallet link` with mode **0600** (parents **0700**). A secret file with
group/other bits set is **refused** on read.

**Payment store (default):** `$XDG_DATA_HOME/buzz/payments/<relay-key>.json`
(mode 0600). One-shot processes reconcile Paying/Unknown on the next
`status` / `pay` / `check` / `reconcile` invocation.

**URI resolution order:** `BUZZ_NWC_URI` → secret file.

### Subcommands

```bash
cargo build -p buzz-cli
BUZZ=./target/debug/buzz

# Link (URI via env, --uri, or stdin with --uri -). Never echoes the secret.
BUZZ_NWC_URI="$URI" $BUZZ wallet link
$BUZZ wallet status
$BUZZ wallet balance
$BUZZ wallet receive --amount-msat 21000 --description coffee
# Quote only (no spend) unless --yes:
$BUZZ wallet pay --bolt11 "$BOLT11"
$BUZZ wallet pay --bolt11 "$BOLT11" --yes
$BUZZ wallet reconcile

# Relay verbs (need BUZZ_PRIVATE_KEY + a running Buzz relay):
$BUZZ wallet request --channel <uuid> --amount-msat 500000 --description lunch
$BUZZ wallet pay --request <event-id> --channel <uuid> --yes
$BUZZ wallet check --request <event-id> --channel <uuid>
```

### Exit codes

Same as the rest of the CLI: `0=ok 1=input 2=network/relay 3=auth 4=other 5=conflict`.

Wallet-specific mapping:

| Failure | Exit | Why |
|---------|------|-----|
| `Unsupported` / `Unauthorized` (incl. receive-only `RESTRICTED`) | **3** | Auth-ish: method refused / secret restricted |
| Invalid URI, amountless/expired invoice, not linked | 1 | Bad input |
| Unreachable wallet relay, resolve failure, unknown pay outcome | 4 | Other (wallet transport) |
| Quote without `--yes` | **0** | Success path that deliberately does not pay |

### Live transcript against `buzz-mock-wallet`

```bash
# 1. Start mock (capture URI from its stdout; do not paste into logs)
cargo run -p buzz-mock-wallet -- --balance-msat 100000000 --port 0
# Prefer the bare URI line: grep -m1 '^nostr+walletconnect://'
# (an INFO line also embeds uri=… — do not feed that whole line to link)

export BUZZ_NWC_SECRET_PATH=/tmp/buzz-wallet-proof/nwc-secret.json
export BUZZ_WALLET_DATA_DIR=/tmp/buzz-wallet-proof/payments
mkdir -p /tmp/buzz-wallet-proof

# 2–6. Drive the CLI (redact URI from any saved transcript)
BUZZ_NWC_URI="$URI" ./target/debug/buzz wallet link
./target/debug/buzz wallet status
./target/debug/buzz wallet balance
INV=$(./target/debug/buzz wallet receive --amount-msat 21000)
BOLT11=$(echo "$INV" | jq -r .bolt11)
./target/debug/buzz wallet pay --bolt11 "$BOLT11"          # quote only
./target/debug/buzz wallet pay --bolt11 "$BOLT11" --yes    # paid + preimage
./target/debug/buzz wallet reconcile

# 7. Receive-only leg — pay must fail with exit 3
# restart mock with --receive-only, re-link, then:
./target/debug/buzz wallet pay --bolt11 "$BOLT11" --yes; echo "exit=$?"
```

Relay-dependent verbs (`request` / `pay --request` / `check`) are covered by
unit tests against the client/builder seams when a local relay is not up;
do not fake a live transcript for those paths.
