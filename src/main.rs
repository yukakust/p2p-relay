use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::io::Cursor;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio::io::{AsyncRead, AsyncWrite, AsyncReadExt, AsyncWriteExt, ReadBuf};
use tokio_tungstenite::{accept_async_with_config, tungstenite::protocol::WebSocketConfig};
use tokio_tungstenite::tungstenite::Message;
use futures_util::{StreamExt, SinkExt};

type NodeId = String;
type Clients = Arc<Mutex<HashMap<NodeId, tokio::sync::mpsc::UnboundedSender<String>>>>;

// Wrapper to allow reading pre-buffered data before the actual stream
struct PrefixedReadIo<S> {
    stream: S,
    prefix: Cursor<Vec<u8>>,
}

impl<S: AsyncRead + Unpin> AsyncRead for PrefixedReadIo<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        // First read from prefix
        if self.prefix.position() < self.prefix.get_ref().len() as u64 {
            let pos = self.prefix.position() as usize;
            let data = self.prefix.get_ref();
            let available = &data[pos..];
            let to_read = std::cmp::min(available.len(), buf.remaining());
            buf.put_slice(&available[..to_read]);
            self.prefix.set_position((pos + to_read) as u64);
            return Poll::Ready(Ok(()));
        }
        // Then read from stream
        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for PrefixedReadIo<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

#[tokio::main]
async fn main() {
    let port = std::env::var("PORT").unwrap_or_else(|_| "10000".to_string());
    let addr = format!("0.0.0.0:{}", port);
    
    println!("╔════════════════════════════════════════╗");
    println!("║   P2P Relay Server - WebSocket         ║");
    println!("╚════════════════════════════════════════╝\n");
    println!("🚀 Starting WebSocket relay server");
    println!("📡 Listening on {}", addr);
    
    let listener = TcpListener::bind(&addr).await.expect("Failed to bind");
    let clients: Clients = Arc::new(Mutex::new(HashMap::new()));
    
    println!("✅ Server ready!\n");
    
    while let Ok((stream, addr)) = listener.accept().await {
        let clients = clients.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, addr, clients).await {
                eprintln!("Connection error: {}", e);
            }
        });
    }
}

async fn handle_connection(mut stream: TcpStream, addr: SocketAddr, clients: Clients) -> Result<(), Box<dyn std::error::Error>> {
    println!("🔌 New connection from: {}", addr);
    
    // Read first chunk to detect HTTP method
    let mut buf = vec![0u8; 1024];
    let n = stream.read(&mut buf).await?;
    
    if n == 0 {
        return Ok(());
    }
    
    let request = String::from_utf8_lossy(&buf[..n]);
    
    // Simple health check for Render
    // Check if it does NOT contain "Upgrade: websocket"
    if !request.to_lowercase().contains("upgrade: websocket") {
        println!("💚 Health check or plain HTTP from Render");
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 2\r\n\r\nOK").await?;
        return Ok(());
    }
    
    // It is a WebSocket request!
    // Create a prefixed stream that contains the data we already read
    let prefixed_stream = PrefixedReadIo {
        stream,
        prefix: Cursor::new(buf[..n].to_vec()),
    };
    
    // WebSocket connection
    let config = WebSocketConfig {
        max_message_size: Some(64 << 20),
        max_frame_size: Some(16 << 20),
        accept_unmasked_frames: false,
        ..WebSocketConfig::default()
    };
    
    let ws_stream = accept_async_with_config(prefixed_stream, Some(config)).await?;
    let (mut ws_sender, mut ws_receiver) = ws_stream.split();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    
    let mut node_id: Option<NodeId> = None;
    
    // Task to send messages
    let send_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if ws_sender.send(Message::Text(msg)).await.is_err() {
                break;
            }
        }
    });
    
    // Receive messages
    while let Some(msg) = ws_receiver.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                    if node_id.is_none() {
                        if let Some(from) = json.get("from").and_then(|v| v.as_str()) {
                            node_id = Some(from.to_string());
                            clients.lock().await.insert(from.to_string(), tx.clone());
                            println!("✓ Registered: {} ({})", from, addr);
                            println!("📊 Total clients: {}", clients.lock().await.len());
                        }
                    }
                    
                    if let Some(to) = json.get("to").and_then(|v| v.as_str()) {
                        let clients_lock = clients.lock().await;
                        if let Some(dest_tx) = clients_lock.get(to) {
                            if dest_tx.send(text.clone()).is_ok() {
                                if let Some(ref from) = node_id {
                                    println!("📨 Forwarded: {} → {}", from, to);
                                }
                            }
                        }
                    }
                }
            }
            Ok(Message::Close(_)) => break,
            Err(_) => break,
            _ => {}
        }
    }
    
    if let Some(id) = node_id {
        clients.lock().await.remove(&id);
        println!("❌ Disconnected: {}", id);
    }
    
    send_task.abort();
    Ok(())
}