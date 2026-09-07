//! The `tower-lsp` `LanguageServer` implementation for HULK.
//!
//! This module is intentionally thin: it stores each open document's text
//! and delegates all compiler-pipeline work to [`crate::diagnostics`].

use std::collections::HashMap;
use std::sync::RwLock;

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverContents, HoverParams,
    HoverProviderCapability, InitializeParams, InitializeResult, InitializedParams, Location,
    MarkupContent, MarkupKind, OneOf, Position, ServerCapabilities, TextDocumentSyncCapability,
    TextDocumentSyncKind, Url,
};
use tower_lsp::{Client, LanguageServer};

use hulk_semantic::TypeRegistry;

use crate::diagnostics::compute_diagnostics;

/// The capabilities this server advertises to the client during
/// `initialize`. Pulled out as its own function so it can be unit-tested
/// without spinning up a real `tower-lsp` `Client`/`Server` pair.
pub fn server_capabilities() -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        definition_provider: Some(OneOf::Left(true)),
        ..ServerCapabilities::default()
    }
}

/// Builds a hover response for `position`, or `None` if nothing resolves
/// there (e.g. the cursor is over a literal, keyword, or punctuation).
fn hover_response(verified: &hulk_semantic::VerifiedProgram, position: Position) -> Option<Hover> {
    let hit = crate::resolve::resolve_at(&verified.typed_program, position)?;
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: format!("```hulk\n{}: {}\n```", hit.name, hit.ty),
        }),
        range: None,
    })
}

/// Builds a go-to-definition response for `position`, or `None` if
/// nothing resolves there, or what resolves there has no known
/// definition location (see this plan's "Known limitations").
fn definition_response(
    verified: &hulk_semantic::VerifiedProgram,
    uri: &Url,
    position: Position,
) -> Option<GotoDefinitionResponse> {
    let hit = crate::resolve::resolve_at(&verified.typed_program, position)?;
    let span = definition_span_for(&hit, &verified.registry)?;
    Some(GotoDefinitionResponse::Scalar(Location {
        uri: uri.clone(),
        range: crate::diagnostics::span_to_range(span.line, span.col),
    }))
}

/// Resolves a `Hit`'s definition location. For a variable/`self`/parameter
/// reference this is already known (`resolve_at` found it via scope). For
/// a `.member` reference, `resolve_at` deliberately leaves it unresolved —
/// that needs the type registry: look the member up as a method first
/// (methods have a pre-flattened, inheritance-aware table via
/// `lookup_method`), then as an attribute, walking up the inheritance
/// chain by hand since `TypeInfo.attributes` only holds a type's *own*
/// attributes, not inherited ones.
fn definition_span_for(
    hit: &crate::resolve::Hit,
    registry: &TypeRegistry,
) -> Option<hulk_ast::SourceSpan> {
    if hit.definition.is_some() {
        return hit.definition;
    }

    let receiver_type = hit.receiver_type.as_ref()?;
    if let Some(method) = registry.lookup_method(receiver_type, &hit.name) {
        return Some(method.span);
    }

    let mut current = match receiver_type {
        hulk_semantic::Type::Named(name) => Some(name.clone()),
        _ => None,
    };
    while let Some(type_name) = current {
        if let Some(info) = registry.lookup_type(&type_name) {
            if let Some(attr) = info.attributes.get(&hit.name) {
                return Some(attr.span);
            }
        }
        current = registry.parent_of(&type_name);
    }
    None
}

/// The HULK language server.
pub struct Backend {
    client: Client,
    documents: RwLock<HashMap<Url, DocumentState>>,
}

/// Per-document state: the current text, and the most recently
/// successfully analyzed program (if any is available yet).
struct DocumentState {
    text: String,
    last_good: Option<hulk_semantic::VerifiedProgram>,
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
    /// closed by the time this runs). Also refreshes the cached last-good
    /// analyzed program, when analysis succeeded.
    async fn publish_for(&self, uri: Url) {
        let text = {
            let docs = self.documents.read().unwrap();
            docs.get(&uri).map(|state| state.text.clone())
        };
        let Some(text) = text else { return };

        let outcome = compute_diagnostics(&text);
        if outcome.last_good.is_some() {
            let mut docs = self.documents.write().unwrap();
            if let Some(state) = docs.get_mut(&uri) {
                state.last_good = outcome.last_good;
            }
        }

        self.client.publish_diagnostics(uri, outcome.diagnostics, None).await;
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
        self.documents.write().unwrap().insert(
            uri.clone(),
            DocumentState {
                text: params.text_document.text,
                last_good: None,
            },
        );
        self.publish_for(uri).await;
    }

    async fn did_change(&self, mut params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        // Full-document sync (see `server_capabilities`): the client always
        // sends exactly one change event containing the whole new text.
        let Some(change) = params.content_changes.pop() else {
            return;
        };
        self.documents.write().unwrap().insert(
            uri.clone(),
            DocumentState {
                text: change.text,
                last_good: None,
            },
        );
        self.publish_for(uri).await;
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let response = self
            .documents
            .read()
            .unwrap()
            .get(&uri)
            .and_then(|state| state.last_good.as_ref())
            .and_then(|verified| hover_response(verified, position));
        Ok(response)
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let response = self
            .documents
            .read()
            .unwrap()
            .get(&uri)
            .and_then(|state| state.last_good.as_ref())
            .and_then(|verified| definition_response(verified, &uri, position));
        Ok(response)
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

    fn test_analyze(source: &str) -> hulk_semantic::VerifiedProgram {
        let tokens = hulk_lexer::Lexer::new(source).tokenize().expect("valid tokens");
        let mut program = hulk_parser::parse(tokens).expect("valid parse");
        hulk_transpile::expand_program(&mut program);
        hulk_semantic::analyze(&program).expect("valid program")
    }

    #[test]
    fn hover_response_shows_the_resolved_type() {
        let verified = test_analyze("let x = 5 in\nx + 1;");
        let hover = hover_response(&verified, Position { line: 1, character: 0 }).expect("hover");
        match hover.contents {
            HoverContents::Markup(markup) => assert!(markup.value.contains("x: Number")),
            other => panic!("expected markup hover, got {other:?}"),
        }
    }

    #[test]
    fn hover_response_is_none_over_a_literal() {
        let verified = test_analyze("print(1);");
        assert!(hover_response(&verified, Position { line: 0, character: 6 }).is_none());
    }

    #[test]
    fn definition_response_finds_a_let_binding() {
        let verified = test_analyze("let x = 5 in\nx + 1;");
        let uri = Url::parse("file:///t.hulk").unwrap();
        let response = definition_response(&verified, &uri, Position { line: 1, character: 0 })
            .expect("response");
        match response {
            GotoDefinitionResponse::Scalar(location) => {
                assert_eq!(location.range.start, Position { line: 0, character: 4 });
            }
            other => panic!("expected scalar response, got {other:?}"),
        }
    }

    #[test]
    fn definition_response_finds_a_method_via_the_type_registry() {
        let verified = test_analyze("type A {\n    b(): Number => 1;\n}\nnew A().b();");
        let uri = Url::parse("file:///t.hulk").unwrap();
        let response = definition_response(&verified, &uri, Position { line: 3, character: 8 })
            .expect("response");
        match response {
            GotoDefinitionResponse::Scalar(location) => {
                assert_eq!(location.range.start.line, 1);
            }
            other => panic!("expected scalar response, got {other:?}"),
        }
    }

    #[test]
    fn definition_response_is_none_over_a_literal() {
        let verified = test_analyze("print(1);");
        let uri = Url::parse("file:///t.hulk").unwrap();
        assert!(
            definition_response(&verified, &uri, Position { line: 0, character: 6 }).is_none()
        );
    }
}
