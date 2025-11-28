use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio_tungstenite::{accept_async, tungstenite::Message};
use futures_util::{StreamExt, SinkExt};

type NodeId = String;
type Clients = Arc<Mutex<HashMap<NodeId, tokio::sync::mpsc::UnboundedSender<String>>>>;

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

async fn handle_connection(stream: TcpStream, addr: SocketAddr, clients: Clients) -> Result<(), Box<dyn std::error::Error>> {
    println!("🔌 New connection from: {}", addr);
    
    // Peek at first bytes to detect HTTP vs WebSocket
    let mut buf = [0u8; 1024];
    stream.peek(&mut buf).await?;
    
    let request = String::from_utf8_lossy(&buf);
    
    // Check if it's HTTP GET request (health check)
    if request.starts_with("GET / HTTP") || request.starts_with("GET /health HTTP") {
        println!("💚 Health check from Render");
        let mut stream = stream;
        let response = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 2\r\n\r\nOK";
        stream.try_write(response.as_bytes())?;
        return Ok(());
    }
    
    // Otherwise treat as WebSocket
    let ws_stream = accept_async(stream).await?;
    let (mut ws_sender, mut ws_receiver) = ws_stream.split();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    
    let mut node_id: Option<NodeId> = None;
    
    // Task to send messages from channel to WebSocket
    let send_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if ws_sender.send(Message::Text(msg)).await.is_err() {
                break;
            }
        }
    });
    
    // Receive messages from WebSocket
    while let Some(msg) = ws_receiver.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                // Parse message to get node_id and destination
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                    // Register client on first message
                    if node_id.is_none() {
                        if let Some(from) = json.get("from").and_then(|v| v.as_str()) {
                            node_id = Some(from.to_string());
                            clients.lock().await.insert(from.to_string(), tx.clone());
                            println!("✓ Registered: {} ({})", from, addr);
                            println!("📊 Total clients: {}", clients.lock().await.len());
                        }
                    }
                    
                    // Forward message to destination
                    if let Some(to) = json.get("to").and_then(|v| v.as_str()) {
                        let clients_lock = clients.lock().await;
                        if let Some(dest_tx) = clients_lock.get(to) {
                            if dest_tx.send(text.clone()).is_ok() {
                                if let Some(ref from) = node_id {
                                    println!("📨 Forwarded: {} → {}", from, to);
                                }
                            }
                        } else {
                            println!("⚠️  Destination not found: {}", to);
                        }
                    }
                }
            }
            Ok(Message::Close(_)) => break,
            Err(e) => {
                eprintln!("❌ WebSocket error: {}", e);
                break;
            }
            _ => {}
        }
    }
    
    // Cleanup
    if let Some(id) = node_id {
        clients.lock().await.remove(&id);
        println!("❌ Disconnected: {} ({})", id, addr);
        println!("📊 Total clients: {}", clients.lock().await.len());
    }
    
    send_task.abort();
    Ok(())
}