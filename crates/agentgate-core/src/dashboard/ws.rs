use super::state::DashboardState;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::broadcast::error::RecvError;

pub async fn ws_live_handler(
    ws: WebSocketUpgrade,
    State(state): State<DashboardState>,
) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: DashboardState) {
    let mut rx = state.live_tx.subscribe();
    let (mut sender, mut receiver) = socket.split();

    loop {
        tokio::select! {
            result = rx.recv() => {
                match result {
                    Ok(record) => {
                        match serde_json::to_string(&record) {
                            Ok(json) => {
                                if sender.send(Message::Text(json)).await.is_err() {
                                    return;
                                }
                            }
                            Err(e) => tracing::warn!("WS serialisation failed: {e}"),
                        }
                    }
                    Err(RecvError::Lagged(n)) => {
                        tracing::warn!("Live stream receiver lagged, skipped {n} records");
                    }
                    Err(RecvError::Closed) => return,
                }
            }
            msg = receiver.next() => {
                // Any message from the client (including close frames) terminates the loop.
                if msg.is_none() {
                    return;
                }
            }
        }
    }
}
