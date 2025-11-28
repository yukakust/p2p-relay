use std::collections::HashMap;
use std::net::{TcpListener, TcpStream};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread;
use serde_json;

type NodeId = String;

/// Simple relay server - forwards messages between connected clients
struct RelayServer {
    clients: Arc<Mutex<HashMap<NodeId, TcpStream>>>,
}

impl RelayServer {
    fn new() -> Self {
        Self {
            clients: Arc::new(Mutex::new(HashMap::new())),
        }
    }
    
    fn run(&self, port: u16) -> std::io::Result<()> {
        let listener = TcpListener::bind(format!("0.0.0.0:{}", port))?;
        println!("🚀 Relay Server started!");
        println!("📡 Listening on 0.0.0.0:{}", port);
        println!("🌐 Clients can connect to this relay\n");
        
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let clients = self.clients.clone();
                    thread::spawn(move || {
                        if let Err(e) = handle_client(stream, clients) {
                            eprintln!("Client error: {}", e);
                        }
                    });
                }
                Err(e) => eprintln!("Connection error: {}", e),
            }
        }
        
        Ok(())
    }
}

fn handle_client(
    mut stream: TcpStream,
    clients: Arc<Mutex<HashMap<NodeId, TcpStream>>>,
) -> std::io::Result<()> {
    let peer_addr = stream.peer_addr()?;
    println!("🔌 New connection from: {}", peer_addr);
    
    // Read first message to get node_id (registration)
    let mut buffer = vec![0u8; 4096];
    let n = stream.read(&mut buffer)?;
    
    if n == 0 {
        println!("❌ Client disconnected immediately: {}", peer_addr);
        return Ok(());
    }
    
    // Try to parse as JSON to get node_id
    let data = &buffer[..n];
    let node_id = if let Ok(msg) = serde_json::from_slice::<serde_json::Value>(data) {
        if let Some(from) = msg.get("from").and_then(|v| v.as_str()) {
            from.to_string()
        } else {
            format!("unknown_{}", peer_addr)
        }
    } else {
        format!("unknown_{}", peer_addr)
    };
    
    println!("✓ Registered client: {} ({})", node_id, peer_addr);
    
    // Clone stream for storage
    let stream_clone = stream.try_clone()?;
    
    // Register client
    {
        let mut clients_map = clients.lock().unwrap();
        clients_map.insert(node_id.clone(), stream_clone);
        println!("📊 Total clients: {}", clients_map.len());
    }
    
    // Forward first message to destination
    if let Ok(msg) = serde_json::from_slice::<serde_json::Value>(data) {
        if let Some(to) = msg.get("to").and_then(|v| v.as_str()) {
            forward_message(to, data, &clients);
        }
    }
    
    // Main loop: read and forward messages
    loop {
        let mut buffer = vec![0u8; 4096];
        match stream.read(&mut buffer) {
            Ok(0) => {
                // Client disconnected
                println!("❌ Client disconnected: {}", node_id);
                let mut clients_map = clients.lock().unwrap();
                clients_map.remove(&node_id);
                println!("📊 Total clients: {}", clients_map.len());
                break;
            }
            Ok(n) => {
                let data = &buffer[..n];
                
                // Parse message to get destination
                if let Ok(msg) = serde_json::from_slice::<serde_json::Value>(data) {
                    if let Some(to) = msg.get("to").and_then(|v| v.as_str()) {
                        println!("📨 Forwarding message: {} → {}", node_id, to);
                        forward_message(to, data, &clients);
                    }
                }
            }
            Err(e) => {
                eprintln!("Read error from {}: {}", node_id, e);
                break;
            }
        }
    }
    
    Ok(())
}

fn forward_message(
    to: &str,
    data: &[u8],
    clients: &Arc<Mutex<HashMap<NodeId, TcpStream>>>,
) {
    let mut clients_map = clients.lock().unwrap();
    
    if let Some(dest_stream) = clients_map.get_mut(to) {
        match dest_stream.write_all(data) {
            Ok(_) => println!("✓ Message forwarded to {}", to),
            Err(e) => eprintln!("❌ Failed to forward to {}: {}", to, e),
        }
    } else {
        println!("⚠️  Destination not found: {}", to);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    
    let port = if args.len() > 1 {
        args[1].parse().unwrap_or(9000)
    } else {
        9000
    };
    
    let server = RelayServer::new();
    
    println!("╔════════════════════════════════════════╗");
    println!("║   P2P Relay Server - MVP               ║");
    println!("╚════════════════════════════════════════╝\n");
    
    if let Err(e) = server.run(port) {
        eprintln!("❌ Server error: {}", e);
        std::process::exit(1);
    }
}
