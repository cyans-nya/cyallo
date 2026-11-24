use lsp_server::{Connection, Message, Notification, Request, Response};
use serde_json::{Value, json};
use std::{
    io::Write,
    process::{Command, Stdio},
    thread::JoinHandle,
    time::Duration,
};

struct Client {
    connection: Connection,
    thread: Option<JoinHandle<Result<(), cya_ls::server::ServerError>>>,
    next: i32,
}
impl Client {
    fn new() -> Self {
        let (server, connection) = Connection::memory();
        let thread = Some(std::thread::spawn(|| cya_ls::server::run(server)));
        let mut client = Self {
            connection,
            thread,
            next: 1,
        };
        let early = client.request("textDocument/hover", json!({}));
        assert_eq!(early.response_result.unwrap_err().code, -32002);
        let init = client.request(
            "initialize",
            json!({ "capabilities": {}, "processId": null, "rootUri": null }),
        );
        let value = init.response_result.unwrap();
        assert_eq!(value["serverInfo"]["name"], "cya-ls");
        assert_eq!(value["capabilities"]["positionEncoding"], "utf-16");
        assert_eq!(value["capabilities"]["textDocumentSync"]["change"], 2);
        assert_eq!(
            value["capabilities"]["semanticTokensProvider"]["full"],
            true
        );
        assert!(value["capabilities"].get("completionProvider").is_none());
        assert!(value["capabilities"].get("diagnosticProvider").is_none());
        client.notify("initialized", json!({}));
        client
    }
    fn request(&mut self, method: &str, params: Value) -> Response {
        let id = self.next;
        self.next += 1;
        self.connection
            .sender
            .send(
                Request {
                    id: id.into(),
                    method: method.into(),
                    params,
                }
                .into(),
            )
            .unwrap();
        match self
            .connection
            .receiver
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
        {
            Message::Response(response) => {
                assert_eq!(response.id, id.into());
                response
            }
            message => panic!("unexpected {message:?}"),
        }
    }
    fn notify(&self, method: &str, params: Value) {
        self.connection
            .sender
            .send(
                Notification {
                    method: method.into(),
                    params,
                }
                .into(),
            )
            .unwrap();
    }
    fn open(&self, uri: &str, id: &str, text: &str) {
        self.notify(
            "textDocument/didOpen",
            json!({"textDocument":{"uri":uri,"languageId":id,"version":1,"text":text}}),
        );
    }
    fn tokens(&mut self, uri: &str) -> Value {
        self.request(
            "textDocument/semanticTokens/full",
            json!({"textDocument":{"uri":uri}}),
        )
        .response_result
        .unwrap()
    }
    fn stop(mut self) {
        assert!(
            self.request("shutdown", Value::Null)
                .response_result
                .unwrap()
                .is_null()
        );
        let after = self.request("textDocument/semanticTokens/full", json!({}));
        assert_eq!(after.response_result.unwrap_err().code, -32600);
        self.notify("exit", Value::Null);
        self.thread.take().unwrap().join().unwrap().unwrap();
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = self.connection.sender.send(
                Notification {
                    method: "exit".into(),
                    params: Value::Null,
                }
                .into(),
            );
            let _ = thread.join();
        }
    }
}

#[test]
fn buffers_update_independently_and_reopen_cleanly() {
    let mut client = Client::new();
    let modern = "file:///work/a.ya";
    let caie = "file:///work/b.psc";
    client.open(modern, "cyallo", "let value = 1\n");
    client.open(caie, "caie", "OUTPUT \"水🐈\"\r\n");
    let before = client.tokens(modern);
    let caie_before = client.tokens(caie);
    client.notify("textDocument/didChange", json!({
        "textDocument":{"uri":modern,"version":2},
        "contentChanges":[{"range":{"start":{"line":0,"character":12},"end":{"line":0,"character":13}},"text":"true"}]
    }));
    let after = client.tokens(modern);
    assert_eq!(after["resultId"], "2");
    assert_ne!(after["data"], before["data"]);
    assert_eq!(client.tokens(caie), caie_before);
    client.notify(
        "textDocument/didChange",
        json!({"textDocument":{"uri":modern,"version":1},"contentChanges":[{"text":""}]}),
    );
    assert_eq!(client.tokens(modern), after);
    client.notify(
        "textDocument/didClose",
        json!({"textDocument":{"uri":modern}}),
    );
    let closed = client.request(
        "textDocument/semanticTokens/full",
        json!({"textDocument":{"uri":modern}}),
    );
    assert_eq!(closed.response_result.unwrap_err().code, -32602);
    client.open(modern, "cyallo", "// reopened\n");
    assert_eq!(client.tokens(modern)["resultId"], "1");
    client.stop();
}

#[test]
fn language_ids_extensions_ranges_and_invalid_requests() {
    let mut client = Client::new();
    for uri in ["untitled:scratch", "file:///a.psc", "file:///a.pss"] {
        client.open(uri, "caie", "OUTPUT 123\n");
        assert!(!client.tokens(uri)["data"].as_array().unwrap().is_empty());
    }
    client.open("file:///forced.psc", "cyallo", "let value = 1\n");
    assert_eq!(client.tokens("file:///forced.psc")["data"][3], 3);
    client.open("file:///fallback.yas", "plaintext", "let value = 1\n");
    assert_eq!(client.tokens("file:///fallback.yas")["data"][3], 3);
    client.open("file:///fallback.pss", "plaintext", "OUTPUT 1\n");
    assert!(
        !client.tokens("file:///fallback.pss")["data"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let range = client
        .request(
            "textDocument/semanticTokens/range",
            json!({
                "textDocument":{"uri":"file:///a.psc"},
                "range":{"start":{"line":0,"character":8},"end":{"line":0,"character":9}}
            }),
        )
        .response_result
        .unwrap();
    assert_eq!(range["data"], json!([0, 8, 1, 1, 0]));
    let invalid = client.request("textDocument/semanticTokens/full", json!({"broken":true}));
    assert_eq!(invalid.response_result.unwrap_err().code, -32602);
    let invalid = client.request(
        "textDocument/semanticTokens/range",
        json!({
            "textDocument":{"uri":"file:///a.psc"},
            "range":{"start":{"line":9,"character":0},"end":{"line":9,"character":1}}
        }),
    );
    assert_eq!(invalid.response_result.unwrap_err().code, -32602);
    assert_eq!(
        client
            .request("textDocument/completion", json!({}))
            .response_result
            .unwrap_err()
            .code,
        -32601
    );
    client.stop();
}

#[test]
fn stdio_has_only_protocol_frames_and_exits_successfully() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_cya-ls"))
        .arg("--stdio")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for message in [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}),
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"untitled:test","languageId":"caie","version":1,"text":"OUTPUT 1\n"}}}),
        json!({"jsonrpc":"2.0","id":2,"method":"textDocument/semanticTokens/full","params":{"textDocument":{"uri":"untitled:test"}}}),
        json!({"jsonrpc":"2.0","id":3,"method":"shutdown","params":null}),
        json!({"jsonrpc":"2.0","method":"exit","params":null}),
    ] {
        let body = serde_json::to_vec(&message).unwrap();
        write!(stdin, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
        stdin.write_all(&body).unwrap();
    }
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut reader = std::io::Cursor::new(output.stdout);
    for id in 1..=3 {
        let Message::Response(response) = Message::read(&mut reader).unwrap().unwrap() else {
            panic!("expected response")
        };
        assert_eq!(response.id, id.into());
        assert!(response.response_result.is_ok());
    }
    assert!(Message::read(&mut reader).unwrap().is_none());
}

#[test]
fn command_line_help_and_version_do_not_start_the_transport() {
    for flag in ["--help", "--version"] {
        let output = Command::new(env!("CARGO_BIN_EXE_cya-ls"))
            .arg(flag)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8(output.stdout).unwrap().contains("cya-ls"));
    }
}
