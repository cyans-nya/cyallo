use caie_int_parser::{Token, TokenKind, lex};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};
struct CountingAllocator;
thread_local! {static TRACK:Cell<bool>=const{Cell::new(false)};static ALLOCATIONS:Cell<usize>=const{Cell::new(0)};}
fn count() {
    if TRACK.try_with(Cell::get).unwrap_or(false) {
        let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
    }
}
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        count();
        unsafe { System.realloc(ptr, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;
#[test]
fn compact_copyable_tokens() {
    assert_eq!(size_of::<TokenKind>(), 1);
    assert_eq!(size_of::<Token>(), 16);
    assert!(!std::mem::needs_drop::<Token>());
}
#[test]
fn lexing_does_not_allocate_token_text() {
    let source = "Count <- Count + 123\nOUTPUT \"é水🐈\", '水'\n".repeat(10_000);
    ALLOCATIONS.set(0);
    TRACK.set(true);
    let lexed = lex(&source).unwrap();
    TRACK.set(false);
    let allocations = ALLOCATIONS.get();
    assert!(
        allocations < 32,
        "{allocations} allocations for {} tokens",
        lexed.len()
    );
    assert!(lexed.len() > 100_000);
    let token = lexed[0];
    assert_eq!(lexed.text(token), "Count");
    assert_eq!(lexed.text(token).as_ptr(), source.as_ptr());
    let token = lexed.iter().find(|t| t.kind == TokenKind::Text).unwrap();
    assert_eq!(lexed.text(*token), "\"é水🐈\"");
    assert_eq!(
        lexed.text(*token).as_ptr(),
        source[token.span.start as usize..].as_ptr()
    );
}
#[test]
fn keyword_classification_is_case_insensitive() {
    let tokens = lex("dEcLaRe Count : INTEGER\nCount <- 1 DIV 2\nOUTPUT Count\n").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::Declare);
    assert_eq!(tokens[1].kind, TokenKind::Identifier);
    assert_eq!(tokens[3].kind, TokenKind::IntegerType);
    assert!(tokens.iter().any(|t| t.kind == TokenKind::Div));
    assert_eq!(tokens.last().unwrap().kind, TokenKind::Eof);
}
#[test]
fn unicode_and_ascii_assignment_share_a_tag() {
    let tokens = lex("a ← 1\nb <- 2\n").unwrap();
    let assignments: Vec<_> = tokens
        .iter()
        .filter(|t| t.kind == TokenKind::Assign)
        .collect();
    assert_eq!(assignments.len(), 2);
    assert_eq!(tokens.text(*assignments[0]), "←");
    assert_eq!(tokens.text(*assignments[1]), "<-");
}
