//! Optional LSP bridge: query a configured language server for ground-truth
//! go-to-definition. Tree-sitter gives us fast, name-based edges; a real
//! language server gives semantically exact answers. We speak the minimal slice
//! of LSP needed (initialize → definition) over stdio with Content-Length
//! framing.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

/// Encode a JSON-RPC message with the LSP `Content-Length` header.
pub fn encode_message(value: &Value) -> Vec<u8> {
    let body = value.to_string();
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body.as_bytes());
    out
}

/// Read one Content-Length-framed JSON-RPC message from `reader`.
pub fn read_message<R: BufRead>(reader: &mut R) -> Result<Value> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            bail!("LSP stream closed before a complete message");
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break; // end of headers
        }
        if let Some(rest) = trimmed.strip_prefix("Content-Length:") {
            content_length = Some(rest.trim().parse().context("bad Content-Length")?);
        }
    }
    let len = content_length.ok_or_else(|| anyhow!("message had no Content-Length header"))?;
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    Ok(serde_json::from_slice(&buf)?)
}

fn path_to_uri(path: &Path) -> String {
    format!("file://{}", path.to_string_lossy())
}

/// Extract `(uri, line0)` from any of the shapes `textDocument/definition` may
/// return: a single Location, an array of Locations, or LocationLinks.
fn parse_definition(result: &Value) -> Option<(String, u64)> {
    let first = match result {
        Value::Array(arr) => arr.first()?,
        Value::Null => return None,
        other => other,
    };
    // LocationLink uses `targetUri`/`targetRange`; Location uses `uri`/`range`.
    let uri = first
        .get("uri")
        .or_else(|| first.get("targetUri"))?
        .as_str()?
        .to_string();
    let range = first.get("range").or_else(|| first.get("targetRange"))?;
    let line = range.get("start")?.get("line")?.as_u64()?;
    Some((uri, line))
}

pub struct LspClient {
    child: Child,
    next_id: i64,
}

impl LspClient {
    /// Spawn a language server from a shell-style command string and perform the
    /// initialize handshake rooted at `root`.
    pub fn start(command: &str, root: &Path) -> Result<Self> {
        let mut parts = command.split_whitespace();
        let program = parts.next().context("empty LSP command")?;
        let args: Vec<&str> = parts.collect();
        let child = Command::new(program)
            .args(&args)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("failed to start language server '{command}'"))?;
        let mut client = Self { child, next_id: 0 };
        client.initialize(root)?;
        Ok(client)
    }

    fn send(&mut self, msg: &Value) -> Result<()> {
        let stdin = self
            .child
            .stdin
            .as_mut()
            .context("language server stdin closed")?;
        stdin.write_all(&encode_message(msg))?;
        stdin.flush()?;
        Ok(())
    }

    /// Read messages until the response matching `id` arrives, answering any
    /// server→client requests with a null result so the server doesn't stall.
    fn await_response(&mut self, id: i64) -> Result<Value> {
        let stdout = self
            .child
            .stdout
            .take()
            .context("language server stdout closed")?;
        let mut reader = BufReader::new(stdout);
        let result = loop {
            let msg = read_message(&mut reader)?;
            if msg.get("id").and_then(|v| v.as_i64()) == Some(id) && msg.get("method").is_none() {
                break msg.get("result").cloned().unwrap_or(Value::Null);
            }
            // Server-initiated request (has both id and method): ack it.
            if let (Some(req_id), Some(_)) = (msg.get("id").cloned(), msg.get("method")) {
                let reply = json!({"jsonrpc":"2.0","id":req_id,"result":null});
                let stdin = self.child.stdin.as_mut().context("stdin closed")?;
                stdin.write_all(&encode_message(&reply))?;
                stdin.flush()?;
            }
        };
        // Put stdout back for subsequent calls.
        self.child.stdout = Some(reader.into_inner());
        Ok(result)
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))?;
        self.await_response(id)
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.send(&json!({"jsonrpc":"2.0","method":method,"params":params}))
    }

    fn initialize(&mut self, root: &Path) -> Result<()> {
        self.request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": path_to_uri(root),
                "capabilities": {}
            }),
        )?;
        self.notify("initialized", json!({}))?;
        Ok(())
    }

    /// Resolve the definition of the symbol at `(line, character)` (0-based) in
    /// `file`. Returns `(uri, line0)` of the definition, if any.
    pub fn definition(
        &mut self,
        file: &Path,
        line: u32,
        character: u32,
    ) -> Result<Option<(String, u64)>> {
        // Tell the server the file exists; rust-analyzer & co. need didOpen.
        let text = std::fs::read_to_string(file).unwrap_or_default();
        let uri = path_to_uri(file);
        self.notify(
            "textDocument/didOpen",
            json!({"textDocument": {"uri": uri, "languageId": "", "version": 1, "text": text}}),
        )?;
        let result = self.request(
            "textDocument/definition",
            json!({
                "textDocument": {"uri": uri},
                "position": {"line": line, "character": character}
            }),
        )?;
        Ok(parse_definition(&result))
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        // Best-effort shutdown; ignore errors during teardown.
        let _ = self.notify("exit", json!({}));
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn encode_then_read_round_trips() {
        let msg = json!({"jsonrpc":"2.0","id":1,"result":{"ok":true}});
        let bytes = encode_message(&msg);
        let mut cursor = Cursor::new(bytes);
        let back = read_message(&mut cursor).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn read_handles_extra_headers() {
        let body = json!({"id":7});
        let body_str = body.to_string();
        let framed = format!(
            "Content-Type: application/vscode-jsonrpc\r\nContent-Length: {}\r\n\r\n{}",
            body_str.len(),
            body_str
        );
        let mut cursor = Cursor::new(framed.into_bytes());
        assert_eq!(read_message(&mut cursor).unwrap(), body);
    }

    #[test]
    fn parses_location_array_and_location_link() {
        let loc = json!([{"uri":"file:///a.rs","range":{"start":{"line":4,"character":0}}}]);
        assert_eq!(parse_definition(&loc), Some(("file:///a.rs".into(), 4)));
        let link =
            json!([{"targetUri":"file:///b.rs","targetRange":{"start":{"line":9,"character":2}}}]);
        assert_eq!(parse_definition(&link), Some(("file:///b.rs".into(), 9)));
        assert_eq!(parse_definition(&Value::Null), None);
    }
}
