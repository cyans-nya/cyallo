//! A synchronous protocol loop. State belongs to one connection, never a global.
use crate::{
    document::{Document, DocumentError},
    highlight::{HighlightError, Language, SyntaxHighlighter, legend},
};
use lsp_server::{Connection, ErrorCode, Message, Notification, Request, Response, ResponseError};
use lsp_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    InitializeParams, InitializeResult, PositionEncodingKind, SemanticTokens,
    SemanticTokensFullOptions, SemanticTokensOptions, SemanticTokensParams,
    SemanticTokensRangeParams, SemanticTokensServerCapabilities, ServerCapabilities, ServerInfo,
    TextDocumentSyncCapability, TextDocumentSyncKind, TextDocumentSyncOptions, Uri,
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::{collections::HashMap, error::Error, fmt};

#[derive(Debug)]
pub enum ServerError {
    Protocol(lsp_server::ProtocolError),
    Json(serde_json::Error),
    Highlight(HighlightError),
    Io(std::io::Error),
    Disconnected,
    ExitBeforeShutdown,
}
impl fmt::Display for ServerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Protocol(e) => e.fmt(f),
            Self::Json(e) => e.fmt(f),
            Self::Highlight(e) => e.fmt(f),
            Self::Io(e) => e.fmt(f),
            Self::Disconnected => f.write_str("LSP client disconnected"),
            Self::ExitBeforeShutdown => f.write_str("exit received before shutdown"),
        }
    }
}
impl Error for ServerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Protocol(e) => Some(e),
            Self::Json(e) => Some(e),
            Self::Highlight(e) => Some(e),
            Self::Io(e) => Some(e),
            Self::Disconnected | Self::ExitBeforeShutdown => None,
        }
    }
}

#[derive(Debug)]
enum RequestFailure {
    Params(serde_json::Error),
    UnknownDocument(Uri),
    Document(DocumentError),
    Highlight(HighlightError),
    Method(String),
    ShuttingDown,
}
impl fmt::Display for RequestFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Params(e) => write!(f, "invalid parameters: {e}"),
            Self::UnknownDocument(uri) => write!(f, "document is not open: {}", uri.as_str()),
            Self::Document(e) => e.fmt(f),
            Self::Highlight(e) => e.fmt(f),
            Self::Method(method) => write!(f, "unsupported method {method}"),
            Self::ShuttingDown => f.write_str("server is shutting down"),
        }
    }
}
impl Error for RequestFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Params(e) => Some(e),
            Self::Document(e) => Some(e),
            Self::Highlight(e) => Some(e),
            _ => None,
        }
    }
}
impl RequestFailure {
    fn response(self) -> ResponseError {
        let code = match &self {
            Self::Method(_) => ErrorCode::MethodNotFound,
            Self::ShuttingDown => ErrorCode::InvalidRequest,
            Self::Highlight(HighlightError::Document(_))
            | Self::Document(_)
            | Self::Params(_)
            | Self::UnknownDocument(_) => ErrorCode::InvalidParams,
            Self::Highlight(_) => ErrorCode::InternalError,
        };
        ResponseError {
            code: code as i32,
            message: self.to_string(),
            data: None,
        }
    }
}
fn params<T: DeserializeOwned>(value: Value) -> Result<T, RequestFailure> {
    serde_json::from_value(value).map_err(RequestFailure::Params)
}

pub fn capabilities() -> ServerCapabilities {
    ServerCapabilities {
        position_encoding: Some(PositionEncodingKind::UTF16),
        text_document_sync: Some(TextDocumentSyncCapability::Options(
            TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(TextDocumentSyncKind::INCREMENTAL),
                ..Default::default()
            },
        )),
        semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(
            SemanticTokensOptions {
                legend: legend(),
                range: Some(true),
                full: Some(SemanticTokensFullOptions::Bool(true)),
                ..Default::default()
            },
        )),
        ..Default::default()
    }
}

struct Server {
    documents: HashMap<Uri, Document>,
    highlighter: SyntaxHighlighter,
}
impl Server {
    fn tokens(
        &mut self,
        uri: &Uri,
        range: Option<lsp_types::Range>,
    ) -> Result<Value, RequestFailure> {
        let doc = self
            .documents
            .get_mut(uri)
            .ok_or_else(|| RequestFailure::UnknownDocument(uri.clone()))?;
        let data = self
            .highlighter
            .semantic_tokens(doc, range)
            .map_err(RequestFailure::Highlight)?;
        serde_json::to_value(SemanticTokens {
            result_id: Some(doc.version.to_string()),
            data,
        })
        .map_err(RequestFailure::Params)
    }
    fn request(&mut self, request: Request) -> Result<Value, RequestFailure> {
        match request.method.as_str() {
            "textDocument/semanticTokens/full" => {
                let p: SemanticTokensParams = params(request.params)?;
                self.tokens(&p.text_document.uri, None)
            }
            "textDocument/semanticTokens/range" => {
                let p: SemanticTokensRangeParams = params(request.params)?;
                self.tokens(&p.text_document.uri, Some(p.range))
            }
            _ => Err(RequestFailure::Method(request.method)),
        }
    }
    fn notification(&mut self, notification: Notification) -> Result<(), RequestFailure> {
        match notification.method.as_str() {
            "textDocument/didOpen" => {
                let p: DidOpenTextDocumentParams = params(notification.params)?;
                let item = p.text_document;
                if let Some(language) = Language::detect(&item.language_id, &item.uri) {
                    let document = Document::new(language, item.version, item.text)
                        .map_err(RequestFailure::Document)?;
                    self.documents.insert(item.uri, document);
                }
            }
            "textDocument/didChange" => {
                let p: DidChangeTextDocumentParams = params(notification.params)?;
                let doc = self
                    .documents
                    .get_mut(&p.text_document.uri)
                    .ok_or_else(|| RequestFailure::UnknownDocument(p.text_document.uri.clone()))?;
                doc.change(p.text_document.version, p.content_changes)
                    .map_err(RequestFailure::Document)?;
            }
            "textDocument/didClose" => {
                let p: DidCloseTextDocumentParams = params(notification.params)?;
                self.documents.remove(&p.text_document.uri);
            }
            // Unknown notifications (including cancellation of completed work)
            // have no response under JSON-RPC.
            _ => {}
        }
        Ok(())
    }
}

#[derive(PartialEq, Eq)]
enum State {
    Running,
    ShuttingDown,
}

/// Also used by integration tests through an in-memory LSP connection.
pub fn run(connection: Connection) -> Result<(), ServerError> {
    let mut server = Server {
        documents: HashMap::new(),
        highlighter: SyntaxHighlighter::new().map_err(ServerError::Highlight)?,
    };
    let (id, value) = connection
        .initialize_start()
        .map_err(ServerError::Protocol)?;
    if let Err(error) = serde_json::from_value::<InitializeParams>(value) {
        connection
            .sender
            .send(
                Response {
                    id,
                    response_result: Err(ResponseError {
                        code: ErrorCode::InvalidParams as i32,
                        message: error.to_string(),
                        data: None,
                    }),
                }
                .into(),
            )
            .map_err(|_| ServerError::Disconnected)?;
        return Err(ServerError::Json(error));
    }
    let result = InitializeResult {
        capabilities: capabilities(),
        server_info: Some(ServerInfo {
            name: "cya-ls".into(),
            version: Some(env!("CARGO_PKG_VERSION").into()),
        }),
    };
    connection
        .initialize_finish(id, serde_json::to_value(result).map_err(ServerError::Json)?)
        .map_err(ServerError::Protocol)?;
    let mut state = State::Running;
    for message in &connection.receiver {
        match message {
            Message::Request(request) => {
                let id = request.id.clone();
                let result = if state == State::ShuttingDown {
                    Err(RequestFailure::ShuttingDown)
                } else if request.method == "shutdown" {
                    state = State::ShuttingDown;
                    Ok(Value::Null)
                } else {
                    server.request(request)
                };
                connection
                    .sender
                    .send(
                        Response {
                            id,
                            response_result: result.map_err(RequestFailure::response),
                        }
                        .into(),
                    )
                    .map_err(|_| ServerError::Disconnected)?;
            }
            Message::Notification(notification) if notification.method == "exit" => {
                return if state == State::ShuttingDown {
                    Ok(())
                } else {
                    Err(ServerError::ExitBeforeShutdown)
                };
            }
            Message::Notification(notification) if state == State::Running => {
                if let Err(error) = server.notification(notification) {
                    eprintln!("cya-ls: {error}");
                }
            }
            Message::Notification(_) | Message::Response(_) => {}
        }
    }
    Err(ServerError::Disconnected)
}

pub fn run_stdio() -> Result<(), ServerError> {
    let (connection, threads) = Connection::stdio();
    // On an initialization error the client may keep stdin open. Do not wait
    // for an unread exit notification on that failure path.
    run(connection)?;
    threads.join().map_err(ServerError::Io)?;
    Ok(())
}
