use cya_ls::{
    document::{Document, DocumentError, LineIndex},
    highlight::Language,
};
use lsp_types::{Position as P, Range, TextDocumentContentChangeEvent as Change};

#[test]
fn utf16_crlf_and_end_of_file() {
    let source = "é水🐈\r\nlast\n";
    let lines = LineIndex::new(source).unwrap();
    for (byte, position) in [
        (0, P::new(0, 0)),
        (2, P::new(0, 1)),
        (5, P::new(0, 2)),
        (9, P::new(0, 4)),
        (11, P::new(1, 0)),
        (16, P::new(2, 0)),
    ] {
        assert_eq!(lines.offset(source, position).unwrap(), byte);
        assert_eq!(lines.position(source, byte).unwrap(), position);
    }
    assert_eq!(lines.offset(source, P::new(0, 999)).unwrap(), 9);
    assert!(matches!(
        lines.offset(source, P::new(0, 3)),
        Err(DocumentError::InvalidPosition(_))
    ));
    assert!(lines.offset(source, P::new(3, 0)).is_err());
    assert!(lines.position(source, 1).is_err());
}

fn edit(start: P, end: P, text: &str) -> Change {
    Change {
        range: Some(Range::new(start, end)),
        range_length: None,
        text: text.into(),
    }
}
#[test]
fn edits_are_sequential_atomic_and_versioned() {
    let mut doc = Document::new(Language::Cyallo, 7, "let 🐈 = 1\n".into()).unwrap();
    doc.change(
        8,
        vec![
            edit(P::new(0, 4), P::new(0, 6), "water"),
            edit(P::new(0, 12), P::new(0, 13), "2"),
        ],
    )
    .unwrap();
    assert_eq!(doc.text, "let water = 2\n");
    let old = doc.text.clone();
    assert!(
        doc.change(
            9,
            vec![
                edit(P::new(0, 0), P::new(0, 3), "const"),
                edit(P::new(4, 0), P::new(4, 1), "bad")
            ]
        )
        .is_err()
    );
    assert_eq!(doc.text, old);
    assert_eq!(doc.version, 8);
    assert!(matches!(
        doc.change(8, vec![]),
        Err(DocumentError::StaleVersion { .. })
    ));
    doc.change(
        9,
        vec![Change {
            range: None,
            range_length: None,
            text: String::new(),
        }],
    )
    .unwrap();
    assert_eq!(doc.lines.offset(&doc.text, P::new(0, 0)).unwrap(), 0);
}
