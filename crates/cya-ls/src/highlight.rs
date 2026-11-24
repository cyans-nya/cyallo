//! The editor queries are also the server's only highlighting rules.
use crate::document::{Document, DocumentError};
use lsp_types::{
    Position, Range, SemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokensLegend,
    Uri,
};
use std::{error::Error, fmt};
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Cyallo,
    Caie,
}
impl Language {
    pub fn detect(id: &str, uri: &Uri) -> Option<Self> {
        match id {
            "cyallo" => Some(Self::Cyallo),
            "caie" => Some(Self::Caie),
            _ if uri.path().as_str().ends_with(".ya") || uri.path().as_str().ends_with(".yas") => {
                Some(Self::Cyallo)
            }
            _ if uri.path().as_str().ends_with(".psc") => Some(Self::Caie),
            _ if uri.path().as_str().ends_with(".pss") => Some(Self::Caie),
            _ => None,
        }
    }
    pub fn grammar(self) -> tree_sitter::Language {
        match self {
            Self::Cyallo => tree_sitter_cyallo::LANGUAGE.into(),
            Self::Caie => tree_sitter_caie::LANGUAGE.into(),
        }
    }
    pub fn query(self) -> &'static str {
        match self {
            Self::Cyallo => tree_sitter_cyallo::HIGHLIGHTS_QUERY,
            Self::Caie => tree_sitter_caie::HIGHLIGHTS_QUERY,
        }
    }
}

#[derive(Debug)]
pub enum HighlightError {
    Query(tree_sitter::QueryError),
    Highlight(tree_sitter_highlight::Error),
    Document(DocumentError),
    UnknownCapture(usize),
}
impl fmt::Display for HighlightError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Query(e) => write!(f, "invalid highlight query: {e}"),
            Self::Highlight(e) => write!(f, "highlighting failed: {e}"),
            Self::Document(e) => e.fmt(f),
            Self::UnknownCapture(index) => write!(f, "unknown highlight capture {index}"),
        }
    }
}
impl Error for HighlightError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Query(e) => Some(e),
            Self::Highlight(e) => Some(e),
            Self::Document(e) => Some(e),
            Self::UnknownCapture(_) => None,
        }
    }
}
impl From<DocumentError> for HighlightError {
    fn from(error: DocumentError) -> Self {
        Self::Document(error)
    }
}

// Discriminants are not semantic state: indices are assigned only at the LSP
// serialization boundary below, from this explicit typed mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Variable,
    Number,
    String,
    Keyword,
    Comment,
    Type,
    TypeParameter,
    Parameter,
    Property,
    Namespace,
    Function,
    Method,
    Decorator,
    Constant,
    Operator,
}
const CAPTURES: &[&str] = &[
    "variable",
    "number",
    "string",
    "boolean",
    "comment",
    "type",
    "type.parameter",
    "variable.parameter",
    "property",
    "namespace",
    "function",
    "function.method",
    "attribute",
    "constant",
    "keyword",
    "operator",
    "lifetime",
];
const KINDS: &[Kind] = &[
    Kind::Variable,
    Kind::Number,
    Kind::String,
    Kind::Keyword,
    Kind::Comment,
    Kind::Type,
    Kind::TypeParameter,
    Kind::Parameter,
    Kind::Property,
    Kind::Namespace,
    Kind::Function,
    Kind::Method,
    Kind::Decorator,
    Kind::Constant,
    Kind::Keyword,
    Kind::Operator,
    Kind::TypeParameter,
];
pub fn legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: vec![
            SemanticTokenType::VARIABLE,
            SemanticTokenType::NUMBER,
            SemanticTokenType::STRING,
            SemanticTokenType::KEYWORD,
            SemanticTokenType::COMMENT,
            SemanticTokenType::TYPE,
            SemanticTokenType::TYPE_PARAMETER,
            SemanticTokenType::PARAMETER,
            SemanticTokenType::PROPERTY,
            SemanticTokenType::NAMESPACE,
            SemanticTokenType::FUNCTION,
            SemanticTokenType::METHOD,
            SemanticTokenType::DECORATOR,
            SemanticTokenType::OPERATOR,
        ],
        token_modifiers: vec![SemanticTokenModifier::READONLY],
    }
}
impl Kind {
    fn encoding(self) -> (u32, u32) {
        match self {
            Self::Variable => (0, 0),
            Self::Number => (1, 0),
            Self::String => (2, 0),
            Self::Keyword => (3, 0),
            Self::Comment => (4, 0),
            Self::Type => (5, 0),
            Self::TypeParameter => (6, 0),
            Self::Parameter => (7, 0),
            Self::Property => (8, 0),
            Self::Namespace => (9, 0),
            Self::Function => (10, 0),
            Self::Method => (11, 0),
            Self::Decorator => (12, 0),
            Self::Constant => (0, 1),
            Self::Operator => (13, 0),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub start: Position,
    pub length: u32,
    pub kind: Kind,
}

pub struct SyntaxHighlighter {
    highlighter: Highlighter,
    cyallo: HighlightConfiguration,
    caie: HighlightConfiguration,
}
impl SyntaxHighlighter {
    pub fn new() -> Result<Self, HighlightError> {
        fn config(
            language: Language,
            name: &str,
        ) -> Result<HighlightConfiguration, HighlightError> {
            let mut config =
                HighlightConfiguration::new(language.grammar(), name, language.query(), "", "")
                    .map_err(HighlightError::Query)?;
            config.configure(CAPTURES);
            Ok(config)
        }
        Ok(Self {
            highlighter: Highlighter::new(),
            cyallo: config(Language::Cyallo, "cyallo")?,
            caie: config(Language::Caie, "caie")?,
        })
    }

    pub fn tokens<'a>(&mut self, doc: &'a mut Document) -> Result<&'a [Token], HighlightError> {
        if doc.highlights.is_none() {
            let config = match doc.language {
                Language::Cyallo => &self.cyallo,
                Language::Caie => &self.caie,
            };
            let events = self
                .highlighter
                .highlight(config, doc.text.as_bytes(), None, None, |_| None)
                .map_err(HighlightError::Highlight)?;
            let mut stack = Vec::new();
            let mut tokens: Vec<Token> = Vec::new();
            for event in events {
                match event.map_err(HighlightError::Highlight)? {
                    HighlightEvent::HighlightStart(capture) => stack.push(
                        *KINDS
                            .get(capture.0)
                            .ok_or(HighlightError::UnknownCapture(capture.0))?,
                    ),
                    HighlightEvent::HighlightEnd => {
                        stack.pop();
                    }
                    HighlightEvent::Source { start, end } => {
                        let Some(&kind) = stack.last() else { continue };
                        let mut offset = start;
                        for line in doc.text[start..end].split_inclusive('\n') {
                            let text = line.trim_end_matches(['\r', '\n']);
                            if !text.is_empty() {
                                let position = doc.lines.position(&doc.text, offset)?;
                                let length = text.encode_utf16().count() as u32;
                                if let Some(previous) = tokens.last_mut().filter(|t| {
                                    t.kind == kind
                                        && t.start.line == position.line
                                        && t.start.character + t.length == position.character
                                }) {
                                    previous.length += length;
                                } else {
                                    tokens.push(Token {
                                        start: position,
                                        length,
                                        kind,
                                    });
                                }
                            }
                            offset += line.len();
                        }
                    }
                }
            }
            doc.highlights = Some(tokens);
        }
        Ok(doc.highlights.as_deref().unwrap_or_default())
    }

    pub fn semantic_tokens(
        &mut self,
        doc: &mut Document,
        range: Option<Range>,
    ) -> Result<Vec<SemanticToken>, HighlightError> {
        if let Some(range) = range {
            doc.lines.range(&doc.text, range)?;
        }
        let mut previous = Position::new(0, 0);
        let mut data = Vec::new();
        for token in self.tokens(doc)? {
            let mut start = token.start;
            let mut end = start.character + token.length;
            if let Some(range) = range {
                if start.line < range.start.line || start.line > range.end.line {
                    continue;
                }
                if start.line == range.start.line {
                    start.character = start.character.max(range.start.character);
                }
                if start.line == range.end.line {
                    end = end.min(range.end.character);
                }
            }
            if start.character >= end {
                continue;
            }
            let (token_type, token_modifiers_bitset) = token.kind.encoding();
            data.push(SemanticToken {
                delta_line: start.line - previous.line,
                delta_start: if start.line == previous.line {
                    start.character - previous.character
                } else {
                    start.character
                },
                length: end - start.character,
                token_type,
                token_modifiers_bitset,
            });
            previous = start;
        }
        Ok(data)
    }
}
