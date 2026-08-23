use std::time::Duration;

use coding_tools_tunnel_protocol::ControlMessage;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::time::{timeout, Instant};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(45);
const WEBSOCKET_CLOSE_TIMEOUT: Duration = Duration::from_secs(1);

pub(super) type ClientWebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;
pub(super) type ClientSink = SplitSink<ClientWebSocket, Message>;
pub(super) type ClientStream = SplitStream<ClientWebSocket>;

pub(super) struct HeartbeatTracker {
    last_activity: Instant,
}

impl HeartbeatTracker {
    pub(super) fn new_at(now: Instant) -> Self {
        Self { last_activity: now }
    }

    pub(super) fn record_activity_at(&mut self, now: Instant) {
        self.last_activity = now;
    }

    pub(super) fn record_activity(&mut self) {
        self.record_activity_at(Instant::now());
    }

    pub(super) fn expired_at(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.last_activity) >= HEARTBEAT_TIMEOUT
    }
}

pub(super) fn decode_control(text: &str) -> Result<ControlMessage, String> {
    serde_json::from_str(text).map_err(|error| error.to_string())
}

pub(super) fn encode_control(message: &ControlMessage) -> Result<String, String> {
    serde_json::to_string(message).map_err(|error| error.to_string())
}

pub(super) async fn close_client_websocket(sink: &mut ClientSink, stream: &mut ClientStream) {
    if sink.send(Message::Close(None)).await.is_err() {
        return;
    }
    // Dropping immediately after the Close frame can produce a TCP reset on
    // Windows. Give the peer a bounded window to complete the close handshake.
    let _ = timeout(WEBSOCKET_CLOSE_TIMEOUT, async {
        while let Some(frame) = stream.next().await {
            match frame {
                Ok(Message::Close(_)) | Err(_) => break,
                _ => {}
            }
        }
    })
    .await;
}

pub(super) async fn send_heartbeat(
    sink: &mut ClientSink,
    heartbeat: &HeartbeatTracker,
) -> Result<(), String> {
    if heartbeat.expired_at(Instant::now()) {
        return Err("WSS heartbeat timed out".into());
    }
    sink.send(Message::Ping(Vec::new().into()))
        .await
        .map_err(|error| error.to_string())
}

pub(super) async fn receive_control(
    sink: &mut ClientSink,
    stream: &mut ClientStream,
) -> Result<ControlMessage, String> {
    loop {
        match stream.next().await {
            Some(Ok(Message::Text(text))) => return decode_control(text.as_str()),
            Some(Ok(Message::Ping(payload))) => sink
                .send(Message::Pong(payload))
                .await
                .map_err(|error| error.to_string())?,
            Some(Ok(Message::Close(_))) | None => return Err("websocket closed".into()),
            Some(Ok(_)) => return Err("expected text control message".into()),
            Some(Err(error)) => return Err(error.to_string()),
        }
    }
}

pub(super) async fn send_control(
    sink: &mut ClientSink,
    message: &ControlMessage,
) -> Result<(), String> {
    let encoded = encode_control(message)?;
    sink.send(Message::Text(encoded.into()))
        .await
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{decode_control, encode_control};
    use coding_tools_tunnel_protocol::ControlMessage;

    #[test]
    fn control_codec_round_trips_ready() {
        let encoded = encode_control(&ControlMessage::Ready).expect("ready should encode");
        let decoded = decode_control(&encoded).expect("ready should decode");
        assert!(matches!(decoded, ControlMessage::Ready));
    }
}
