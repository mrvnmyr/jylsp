use std::sync::Arc;

use tokio::task;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};
use tracing::{debug, warn};
use url::Url;

use crate::schema::{SchemaCache, ServerConfig};
use crate::validate::{validate_document, DocKind, StoredDocument};

#[derive(Debug)]
pub struct Backend {
    client: Client,
    cache: Arc<SchemaCache>,
    docs: Arc<tokio::sync::RwLock<std::collections::HashMap<Url, StoredDocument>>>,
}

impl Backend {
    pub fn new(client: Client, cfg: ServerConfig) -> Self {
        Self {
            client,
            cache: Arc::new(SchemaCache::new(cfg)),
            docs: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        }
    }

    async fn upsert_doc(&self, uri: &Url, version: i32, text: String) {
        let kind = DocKind::from_uri(uri);
        let doc = StoredDocument { version, text, kind };

        let mut docs = self.docs.write().await;
        docs.insert(uri.clone(), doc);
    }

    async fn remove_doc(&self, uri: &Url) {
        let mut docs = self.docs.write().await;
        docs.remove(uri);
    }

    async fn get_doc(&self, uri: &Url) -> Option<StoredDocument> {
        let docs = self.docs.read().await;
        docs.get(uri).cloned()
    }

    async fn validate_and_publish(&self, uri: Url) {
        let Some(doc) = self.get_doc(&uri).await else {
            return;
        };
        let expected_version = doc.version;

        if std::env::var_os("DEBUG").is_some() {
            eprintln!(
                "[DEBUG] validate_and_publish uri={} version={}",
                uri, expected_version
            );
        }

        let cache = self.cache.clone();
        let uri_for_task = uri.clone();

        let diags = match task::spawn_blocking(move || validate_document(&uri_for_task, &doc, &cache)).await
        {
            Ok(Ok(d)) => d,
            Ok(Err(e)) => {
                warn!("validation failed: {e:#}");
                vec![Diagnostic {
                    range: Range::default(),
                    severity: Some(DiagnosticSeverity::ERROR),
                    source: Some("cgpt-jsonschema-lsp".to_string()),
                    message: format!("validator failure: {e:#}"),
                    ..Default::default()
                }]
            }
            Err(join_err) => {
                warn!("validation task panicked/canceled: {join_err}");
                vec![Diagnostic {
                    range: Range::default(),
                    severity: Some(DiagnosticSeverity::ERROR),
                    source: Some("cgpt-jsonschema-lsp".to_string()),
                    message: format!("validator task failed: {join_err}"),
                    ..Default::default()
                }]
            }
        };

        // Avoid racing old validations vs. new content.
        if let Some(current) = self.get_doc(&uri).await {
            if current.version != expected_version {
                debug!("skipping publish for stale version {}", expected_version);
                if std::env::var_os("DEBUG").is_some() {
                    eprintln!(
                        "[DEBUG] stale publish skipped uri={} expected={} current={}",
                        uri, expected_version, current.version
                    );
                }
                return;
            }
        }

        self.client.publish_diagnostics(uri, diags, None).await;
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _params: InitializeParams) -> Result<InitializeResult> {
        let caps = ServerCapabilities {
            text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
            ..Default::default()
        };

        Ok(InitializeResult {
            capabilities: caps,
            server_info: Some(ServerInfo {
                name: "cgpt-jsonschema-lsp".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
            ..Default::default()
        })
    }

    async fn initialized(&self, _params: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "cgpt-jsonschema-lsp initialized")
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let td = params.text_document;
        let uri = td.uri;
        self.upsert_doc(&uri, td.version, td.text).await;
        self.validate_and_publish(uri).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        let version = params.text_document.version;

        // FULL sync: last change contains whole document.
        let text = params
            .content_changes
            .into_iter()
            .last()
            .map(|c| c.text)
            .unwrap_or_default();

        self.upsert_doc(&uri, version, text).await;
        self.validate_and_publish(uri).await;
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        // If the client provided text, trust it; otherwise reuse the stored text.
        let uri = params.text_document.uri;
        if let Some(text) = params.text {
            // If we don't have a version, keep the current version; otherwise set 0.
            let v = self.get_doc(&uri).await.map(|d| d.version).unwrap_or(0);
            self.upsert_doc(&uri, v, text).await;
        }
        self.validate_and_publish(uri).await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        self.remove_doc(&uri).await;

        // Clear diagnostics.
        self.client.publish_diagnostics(uri, vec![], None).await;
    }

    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        // Revalidate open documents when schema files change.
        // Some clients only send these if explicitly configured; harmless otherwise.
        let mut to_revalidate: Vec<Url> = Vec::new();

        {
            let docs = self.docs.read().await;
            for (uri, doc) in docs.iter() {
                if matches!(doc.kind, DocKind::Json | DocKind::Yaml) {
                    to_revalidate.push(uri.clone());
                }
            }
        }

        if !params.changes.is_empty() {
            debug!("watched files changed: {}", params.changes.len());
        }

        for uri in to_revalidate {
            self.validate_and_publish(uri).await;
        }
    }
}
