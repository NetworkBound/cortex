//! WebSocket fan-out: `GET /ws`. Every connected client receives a JSON frame
//! for each [`MobileEvent`] published by the POST handlers via the shared
//! `broadcast` channel in [`MobileState`].
//!
//! Each frame is the serialized `MobileEvent` (`{ "type": "...", ... }`). The
//! client switches on `type` and correlates on `run_id`. Inbound client
//! messages are accepted and ignored (the protocol is server-push only); a
//! `Close` frame ends the connection.

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
};

use super::state::MobileState;

/// `GET /ws` upgrade handler. Registered with `any(...)` (not `get(...)`) in the
/// router so the upgrade negotiation isn't method-gated.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<MobileState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| client(socket, state))
}

async fn client(mut socket: WebSocket, state: MobileState) {
    let mut rx = state.events.subscribe();

    loop {
        tokio::select! {
            // Server-push: forward each broadcast event as a text frame.
            ev = rx.recv() => match ev {
                Ok(ev) => {
                    let body = match serde_json::to_string(&ev) {
                        Ok(b) => b,
                        Err(e) => {
                            tracing::warn!(error = %e, "mobile ws: encode failed");
                            continue;
                        }
                    };
                    if socket.send(Message::Text(body)).await.is_err() {
                        return; // client gone
                    }
                }
                // Lagged: a slow client missed frames. Keep going from the
                // newest available frame rather than tearing the socket down.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(skipped = n, "mobile ws: client lagged, dropping frames");
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            },

            // Inbound: drain client messages. We don't act on them (push-only),
            // but we must read so ping/pong + close are handled.
            msg = socket.recv() => match msg {
                Some(Ok(Message::Close(_))) | None => return,
                Some(Ok(_)) => {}
                Some(Err(_)) => return,
            },
        }
    }
}
