//! The `tower-lsp` `LanguageServer` implementation for HULK.
//!
//! This module is intentionally thin: it stores each open document's text
//! and delegates all compiler-pipeline work to [`crate::diagnostics`].

use std::collections::HashMap;
use std::sync::RwLock;

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    InitializeParams, InitializeResult, InitializedParams, ServerCapabilities,
    TextDocumentSyncCapability, TextDocumentSyncKind, Url,
};
use tower_lsp::{Client, LanguageServer};

use crate::diagnostics::compute_diagnostics;

/// The capabilities this server advertises to the client during
/// `initialize`. Pulled out as its own function so it can be unit-tested
/// without spinning up a real `tower-lsp` `Client`/`Server` pair.
pub fn server_capabilities() -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        ..ServerCapabilities::default()
    }
}

/// The HULK language server.
pub struct Backend {
    client: Client,
    documents: RwLock<HashMap<Url, String>>,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            documents: RwLock::new(HashMap::new()),
        }
    }

    /// Recomputes and publishes diagnostics for `uri` from its currently
    /// stored text. No-op if the document isn't open (e.g. it was already
    /// closed by the time this runs).
    async fn publish_for(&self, uri: Url) {
        let text = self.documents.read().unwrap().get(&uri).cloned();
        let Some(text) = text else { return };
        let diagnostics = compute_diagnostics(&text);
        self.client.publish_diagnostics(uri, diagnostics, None).await;
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: server_capabilities(),
            server_info: None,
        })
    }

    async fn initialized(&self, _: InitializedParams) {}

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        self.documents
            .write()
            .unwrap()
            .insert(uri.clone(), params.text_document.text);
        self.publish_for(uri).await;
    }

    async fn did_change(&self, mut params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        // Full-document sync (see `server_capabilities`): the client always
        // sends exactly one change event containing the whole new text.
        let Some(change) = params.content_changes.pop() else {
            return;
        };
        self.documents.write().unwrap().insert(uri.clone(), change.text);
        self.publish_for(uri).await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        self.documents.write().unwrap().remove(&uri);
        // Clear diagnostics so the editor doesn't keep showing stale
        // squiggles for a file that's no longer open.
        self.client.publish_diagnostics(uri, Vec::new(), None).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_capabilities_use_full_text_document_sync() {
        let caps = server_capabilities();
        assert_eq!(
            caps.text_document_sync,
            Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL))
        );
    }
}
