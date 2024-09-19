#[macro_use]
extern crate log;

mod error;
mod handler;
mod utils;

use anyhow::Result;
use async_trait::async_trait;
use axum::{http::Uri, routing::post, Router};
use clap::Parser;
use error::ServerError;
use handler::*;
use hyper::{client::HttpConnector, Client};
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::RwLock;
use tokio::net::TcpListener;
use utils::LogLevel;

type SharedClient = Arc<Client<HttpConnector>>;

// default port of LlamaEdge Gateway
const DEFAULT_PORT: &str = "8080";

#[derive(Debug, Parser)]
#[command(name = "LlamaEdge Gateway", version = env!("CARGO_PKG_VERSION"), author = env!("CARGO_PKG_AUTHORS"), about = "LlamaEdge Gateway")]
struct Cli {
    /// Socket address of LlamaEdge API Server instance
    #[arg(long, default_value = DEFAULT_PORT, value_parser = clap::value_parser!(u16))]
    port: u16,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), ServerError> {
    // get the environment variable `RUST_LOG`
    let rust_log = std::env::var("RUST_LOG").unwrap_or_default().to_lowercase();
    let (_, log_level) = match rust_log.is_empty() {
        true => ("stdout", LogLevel::Info),
        false => match rust_log.split_once("=") {
            Some((target, level)) => (target, level.parse().unwrap_or(LogLevel::Info)),
            None => ("stdout", rust_log.parse().unwrap_or(LogLevel::Info)),
        },
    };

    // set global logger
    wasi_logger::Logger::install().expect("failed to install wasi_logger::Logger");
    log::set_max_level(log_level.into());

    // parse the command line arguments
    let cli = Cli::parse();

    // log the version of the server
    info!(target: "stdout", "version: {}", env!("CARGO_PKG_VERSION"));

    // Create a shared HTTP client
    let client = Arc::new(Client::new());

    let app_state = AppState::new(client);

    // Build our application with routes
    let app = Router::new()
        .route("/v1/chat/completions", post(chat_handler))
        .route("/v1/image/generation", post(image_handler))
        .route("/admin/register/:type", post(add_url_handler))
        .route("/admin/unregister/:type", post(remove_url_handler))
        .with_state(app_state);

    // Run it
    let addr = format!("127.0.0.1:{}", cli.port);
    let tcp_listener = TcpListener::bind(&addr).await.unwrap();
    info!(target: "stdout", "Listening on {}", addr);

    match axum::Server::from_tcp(tcp_listener.into_std().unwrap())
        .unwrap()
        .serve(app.into_make_service())
        .await
    {
        Ok(_) => Ok(()),
        Err(e) => Err(ServerError::Operation(e.to_string())),
    }
}

#[async_trait]
trait RoutingPolicy {
    fn next(&self) -> Uri;
}

/// Represents a LlamaEdge API server
#[derive(Debug)]
struct Server {
    url: Uri,
    connections: AtomicUsize,
}
impl Server {
    fn new(url: Uri) -> Self {
        Self {
            url,
            connections: AtomicUsize::new(0),
        }
    }
}

#[derive(Debug, Default)]
struct Services {
    servers: RwLock<Vec<Server>>,
}
impl Services {
    fn push(&mut self, url: Uri) {
        let server = Server::new(url);
        self.servers.write().unwrap().push(server)
    }
}
impl RoutingPolicy for Services {
    fn next(&self) -> Uri {
        let servers = self.servers.read().unwrap();
        let server = servers
            .iter()
            .min_by(|s1, s2| {
                s1.connections
                    .load(Ordering::Relaxed)
                    .cmp(&s2.connections.load(Ordering::Relaxed))
            })
            .unwrap();

        server.connections.fetch_add(1, Ordering::Relaxed);
        server.url.clone()
    }
}

#[derive(Clone)]
struct AppState {
    client: SharedClient,
    chat_urls: Arc<RwLock<Services>>,
    image_urls: Arc<RwLock<Services>>,
}

impl AppState {
    fn new(client: SharedClient) -> Self {
        Self {
            client,
            chat_urls: Arc::new(RwLock::new(Services::default())),
            image_urls: Arc::new(RwLock::new(Services::default())),
        }
    }

    fn add_url(&self, url_type: UrlType, url: &Uri) {
        match url_type {
            UrlType::Chat => self.chat_urls.write().unwrap().push(url.clone()),
            UrlType::Image => self.image_urls.write().unwrap().push(url.clone()),
            // UrlType::Chat => self.chat_urls.write().unwrap().push(url.clone()),
            // UrlType::Image => self.image_urls.write().unwrap().push(url.clone()),
        }
    }

    fn remove_url(&self, url_type: UrlType, url: &Uri) {
        let services = match &url_type {
            UrlType::Chat => &self.chat_urls,
            UrlType::Image => &self.image_urls,
        };

        let services = services.write().unwrap();
        services
            .servers
            .write()
            .unwrap()
            .retain(|server| &server.url != url);

        // Optionally, log the removal
        println!("Removed {} URL: {}", url_type, url);
    }
}

#[derive(Debug)]
enum UrlType {
    Chat,
    Image,
}
impl fmt::Display for UrlType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UrlType::Chat => write!(f, "Chat"),
            UrlType::Image => write!(f, "Image"),
        }
    }
}
