//! Loopback-only minimal Nostr relay for NWC traffic.
//!
//! Supports EVENT (store + fan-out), REQ (kinds / authors / `#p` / `#e` / since),
//! EOSE, and CLOSE. No auth. No persistence. No policy.
//! (`#e` is required so NWC clients can wait on the matching 23195 response.)

use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// In-memory event store + live subscriptions.
pub struct Relay {
    events: Mutex<Vec<Value>>,
    subs: Mutex<HashMap<(u64, String), Sub>>,
    next_conn_id: AtomicU64,
}

struct Sub {
    filter: Filter,
    tx: mpsc::UnboundedSender<String>,
}

#[derive(Debug, Clone, Default)]
struct Filter {
    kinds: Option<Vec<u64>>,
    authors: Option<Vec<String>>,
    p_tags: Option<Vec<String>>,
    e_tags: Option<Vec<String>>,
    since: Option<u64>,
}

impl Relay {
    /// Create an empty relay.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            events: Mutex::new(Vec::new()),
            subs: Mutex::new(HashMap::new()),
            next_conn_id: AtomicU64::new(1),
        })
    }

    /// Bind `127.0.0.1:{port}` (`port == 0` picks a free port) and serve until cancelled.
    pub async fn listen(
        self: &Arc<Self>,
        port: u16,
        cancel: CancellationToken,
    ) -> Result<(SocketAddr, tokio::task::JoinHandle<()>), std::io::Error> {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port))).await?;
        let addr = listener.local_addr()?;
        let relay = Arc::clone(self);
        let handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    accepted = listener.accept() => {
                        match accepted {
                            Ok((stream, peer)) => {
                                if !peer.ip().is_loopback() {
                                    warn!("rejecting non-loopback peer {peer}");
                                    continue;
                                }
                                let relay = Arc::clone(&relay);
                                let cancel = cancel.clone();
                                tokio::spawn(async move {
                                    if let Err(e) = handle_connection(relay, stream, cancel).await {
                                        debug!("connection closed: {e}");
                                    }
                                });
                            }
                            Err(e) => {
                                warn!("accept failed: {e}");
                                break;
                            }
                        }
                    }
                }
            }
        });
        Ok((addr, handle))
    }
}

async fn handle_connection(
    relay: Arc<Relay>,
    stream: TcpStream,
    cancel: CancellationToken,
) -> Result<(), String> {
    let ws = tokio_tungstenite::accept_async(stream)
        .await
        .map_err(|e| e.to_string())?;
    let conn_id = relay.next_conn_id.fetch_add(1, Ordering::Relaxed);
    let (mut sink, mut stream) = ws.split();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();

    let writer = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            if sink.send(Message::Text(msg.into())).await.is_err() {
                break;
            }
        }
    });

    let result = async {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                msg = stream.next() => {
                    let Some(msg) = msg else { break };
                    let msg = msg.map_err(|e| e.to_string())?;
                    match msg {
                        Message::Text(text) => {
                            handle_client_msg(&relay, conn_id, &text, &out_tx)?;
                        }
                        Message::Close(_) => break,
                        Message::Ping(_) | Message::Pong(_) | Message::Binary(_) | Message::Frame(_) => {}
                    }
                }
            }
        }
        Ok::<(), String>(())
    }
    .await;

    relay.subs.lock().retain(|(cid, _), _| *cid != conn_id);
    drop(out_tx);
    let _ = writer.await;
    result
}

fn handle_client_msg(
    relay: &Relay,
    conn_id: u64,
    text: &str,
    out_tx: &mpsc::UnboundedSender<String>,
) -> Result<(), String> {
    let msg: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let arr = msg
        .as_array()
        .ok_or("client message must be a JSON array")?;
    let typ = arr
        .first()
        .and_then(|v| v.as_str())
        .ok_or("missing message type")?;

    match typ {
        "EVENT" => {
            let event = arr.get(1).ok_or("EVENT missing event")?.clone();
            let id = event
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            relay.events.lock().push(event.clone());
            fanout(relay, &event);
            let _ = out_tx.send(ok_msg(&id, true, ""));
        }
        "REQ" => {
            let sub_id = arr
                .get(1)
                .and_then(|v| v.as_str())
                .ok_or("REQ missing sub id")?
                .to_string();
            let filter = parse_filter(arr.get(2).unwrap_or(&json!({})))?;
            // Replay stored matches.
            {
                let events = relay.events.lock();
                for event in events.iter() {
                    if filter_matches(&filter, event) {
                        let _ = out_tx.send(event_msg(&sub_id, event));
                    }
                }
            }
            relay.subs.lock().insert(
                (conn_id, sub_id.clone()),
                Sub {
                    filter,
                    tx: out_tx.clone(),
                },
            );
            let _ = out_tx.send(eose_msg(&sub_id));
        }
        "CLOSE" => {
            let sub_id = arr
                .get(1)
                .and_then(|v| v.as_str())
                .ok_or("CLOSE missing sub id")?;
            relay.subs.lock().remove(&(conn_id, sub_id.to_string()));
        }
        _ => {
            let _ = out_tx.send(notice_msg(&format!("unsupported: {typ}")));
        }
    }
    Ok(())
}

fn fanout(relay: &Relay, event: &Value) {
    let subs = relay.subs.lock();
    for ((_, sub_id), sub) in subs.iter() {
        if filter_matches(&sub.filter, event) {
            let _ = sub.tx.send(event_msg(sub_id, event));
        }
    }
}

fn parse_filter(v: &Value) -> Result<Filter, String> {
    let obj = v.as_object().ok_or("filter must be an object")?;
    let kinds = obj.get("kinds").and_then(|v| {
        v.as_array()
            .map(|a| a.iter().filter_map(|x| x.as_u64()).collect())
    });
    let authors = obj.get("authors").and_then(|v| {
        v.as_array().map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
    });
    let p_tags = obj.get("#p").and_then(|v| {
        v.as_array().map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
    });
    let e_tags = obj.get("#e").and_then(|v| {
        v.as_array().map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
    });
    let since = obj.get("since").and_then(|v| v.as_u64());
    Ok(Filter {
        kinds,
        authors,
        p_tags,
        e_tags,
        since,
    })
}

fn filter_matches(filter: &Filter, event: &Value) -> bool {
    if let Some(kinds) = &filter.kinds {
        let kind = event
            .get("kind")
            .and_then(|v| v.as_u64())
            .unwrap_or(u64::MAX);
        if !kinds.contains(&kind) {
            return false;
        }
    }
    if let Some(authors) = &filter.authors {
        let pk = event.get("pubkey").and_then(|v| v.as_str()).unwrap_or("");
        if !authors.iter().any(|a| a == pk) {
            return false;
        }
    }
    if let Some(since) = filter.since {
        let created = event
            .get("created_at")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        if created < since {
            return false;
        }
    }
    if let Some(ps) = &filter.p_tags {
        if !tag_values_match(event, "p", ps) {
            return false;
        }
    }
    if let Some(es) = &filter.e_tags {
        if !tag_values_match(event, "e", es) {
            return false;
        }
    }
    true
}

fn tag_values_match(event: &Value, tag_name: &str, wanted: &[String]) -> bool {
    let tags = event
        .get("tags")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let values: Vec<String> = tags
        .iter()
        .filter_map(|t| {
            let arr = t.as_array()?;
            if arr.first().and_then(|v| v.as_str()) == Some(tag_name) {
                arr.get(1).and_then(|v| v.as_str()).map(str::to_string)
            } else {
                None
            }
        })
        .collect();
    wanted.iter().any(|w| values.iter().any(|v| v == w))
}

fn event_msg(sub_id: &str, event: &Value) -> String {
    json!(["EVENT", sub_id, event]).to_string()
}

fn eose_msg(sub_id: &str) -> String {
    json!(["EOSE", sub_id]).to_string()
}

fn ok_msg(id: &str, ok: bool, msg: &str) -> String {
    json!(["OK", id, ok, msg]).to_string()
}

fn notice_msg(msg: &str) -> String {
    json!(["NOTICE", msg]).to_string()
}

/// Publish a signed event JSON value through the relay store + fan-out (in-process).
pub fn inject_event(relay: &Relay, event: Value) {
    relay.events.lock().push(event.clone());
    fanout(relay, &event);
}
