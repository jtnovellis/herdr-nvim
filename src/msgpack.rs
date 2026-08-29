//! Just enough MessagePack to talk to a headless Neovim over its RPC socket.
//!
//! The alternative is spawning `nvim --server <sock> --remote-expr <expr>`,
//! which costs a whole Neovim start (~11 ms) plus the poll interval the caller
//! then waits out. A direct request on the socket the daemon is already
//! listening on measures ~0.07 ms, and this is on the `edit`, `title` and
//! picker-open paths, so it is worth the ~200 lines rather than a dependency.
//!
//! Only the subset Neovim actually sends back is implemented; anything else
//! decodes to [`Value::Unsupported`] rather than failing the call.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

/// A decoded MessagePack value. Deliberately lossy: callers only ever need the
/// display form of a scalar.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Array(Vec<Value>),
    Map(Vec<(Value, Value)>),
    Unsupported,
}

impl Value {
    pub fn is_nil(&self) -> bool {
        matches!(self, Value::Nil)
    }

    /// How `nvim --remote-expr` would have printed this result, so callers
    /// that used to parse that output keep working unchanged.
    pub fn to_display_string(&self) -> String {
        match self {
            Value::Nil => String::new(),
            Value::Bool(b) => (if *b { "1" } else { "0" }).to_string(),
            Value::Int(n) => n.to_string(),
            Value::Float(f) => format!("{f}"),
            Value::Str(s) => s.clone(),
            Value::Array(items) => items
                .iter()
                .map(Value::to_display_string)
                .collect::<Vec<_>>()
                .join("\n"),
            Value::Map(_) | Value::Unsupported => String::new(),
        }
    }
}

// ----- encoding -----------------------------------------------------------

fn write_uint(out: &mut Vec<u8>, n: u64) {
    if n < 0x80 {
        out.push(n as u8);
    } else if n <= u8::MAX as u64 {
        out.extend_from_slice(&[0xcc, n as u8]);
    } else if n <= u16::MAX as u64 {
        out.push(0xcd);
        out.extend_from_slice(&(n as u16).to_be_bytes());
    } else if n <= u32::MAX as u64 {
        out.push(0xce);
        out.extend_from_slice(&(n as u32).to_be_bytes());
    } else {
        out.push(0xcf);
        out.extend_from_slice(&n.to_be_bytes());
    }
}

fn write_str(out: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    let len = bytes.len();
    if len < 32 {
        out.push(0xa0 | len as u8);
    } else if len <= u8::MAX as usize {
        out.extend_from_slice(&[0xd9, len as u8]);
    } else if len <= u16::MAX as usize {
        out.push(0xda);
        out.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        out.push(0xdb);
        out.extend_from_slice(&(len as u32).to_be_bytes());
    }
    out.extend_from_slice(bytes);
}

fn write_array_header(out: &mut Vec<u8>, len: usize) {
    if len < 16 {
        out.push(0x90 | len as u8);
    } else if len <= u16::MAX as usize {
        out.push(0xdc);
        out.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        out.push(0xdd);
        out.extend_from_slice(&(len as u32).to_be_bytes());
    }
}

/// A msgpack-rpc request frame: `[0, msgid, method, [args...]]`.
pub fn encode_request(msgid: u32, method: &str, args: &[&str]) -> Vec<u8> {
    let mut out = Vec::with_capacity(64 + args.iter().map(|a| a.len()).sum::<usize>());
    write_array_header(&mut out, 4);
    write_uint(&mut out, 0); // type: request
    write_uint(&mut out, msgid as u64);
    write_str(&mut out, method);
    write_array_header(&mut out, args.len());
    for arg in args {
        write_str(&mut out, arg);
    }
    out
}

// ----- decoding -----------------------------------------------------------

/// Decode one value. `Ok(None)` means "need more bytes"; `Err(())` means the
/// stream is malformed and cannot be resynchronised.
type Decoded = Result<Option<(Value, usize)>, ()>;

fn need(buf: &[u8], at: usize, n: usize) -> Result<(), Option<()>> {
    if buf.len() < at + n {
        Err(None)
    } else {
        Ok(())
    }
}

fn decode_at(buf: &[u8], at: usize) -> Decoded {
    macro_rules! want {
        ($n:expr) => {
            match need(buf, at, $n) {
                Ok(()) => {}
                Err(_) => return Ok(None),
            }
        };
    }
    want!(1);
    let tag = buf[at];
    let after = at + 1;
    let (value, end) = match tag {
        0x00..=0x7f => (Value::Int(tag as i64), after),
        0xe0..=0xff => (Value::Int(tag as i8 as i64), after),
        0xc0 => (Value::Nil, after),
        0xc2 => (Value::Bool(false), after),
        0xc3 => (Value::Bool(true), after),
        0xcc => {
            want!(2);
            (Value::Int(buf[after] as i64), after + 1)
        }
        0xcd => {
            want!(3);
            let v = u16::from_be_bytes([buf[after], buf[after + 1]]);
            (Value::Int(v as i64), after + 2)
        }
        0xce => {
            want!(5);
            let v = u32::from_be_bytes(buf[after..after + 4].try_into().map_err(|_| ())?);
            (Value::Int(v as i64), after + 4)
        }
        0xcf => {
            want!(9);
            let v = u64::from_be_bytes(buf[after..after + 8].try_into().map_err(|_| ())?);
            (Value::Int(v as i64), after + 8)
        }
        0xd0 => {
            want!(2);
            (Value::Int(buf[after] as i8 as i64), after + 1)
        }
        0xd1 => {
            want!(3);
            let v = i16::from_be_bytes([buf[after], buf[after + 1]]);
            (Value::Int(v as i64), after + 2)
        }
        0xd2 => {
            want!(5);
            let v = i32::from_be_bytes(buf[after..after + 4].try_into().map_err(|_| ())?);
            (Value::Int(v as i64), after + 4)
        }
        0xd3 => {
            want!(9);
            let v = i64::from_be_bytes(buf[after..after + 8].try_into().map_err(|_| ())?);
            (Value::Int(v), after + 8)
        }
        0xca => {
            want!(5);
            let v = f32::from_be_bytes(buf[after..after + 4].try_into().map_err(|_| ())?);
            (Value::Float(v as f64), after + 4)
        }
        0xcb => {
            want!(9);
            let v = f64::from_be_bytes(buf[after..after + 8].try_into().map_err(|_| ())?);
            (Value::Float(v), after + 8)
        }
        // str and bin: Neovim sends strings as bin/str depending on version.
        0xa0..=0xbf => return decode_bytes(buf, after, (tag & 0x1f) as usize),
        0xd9 | 0xc4 => {
            want!(2);
            match decode_bytes(buf, after + 1, buf[after] as usize)? {
                Some(v) => v,
                None => return Ok(None),
            }
        }
        0xda | 0xc5 => {
            want!(3);
            let len = u16::from_be_bytes([buf[after], buf[after + 1]]) as usize;
            match decode_bytes(buf, after + 2, len)? {
                Some(v) => v,
                None => return Ok(None),
            }
        }
        0xdb | 0xc6 => {
            want!(5);
            let len =
                u32::from_be_bytes(buf[after..after + 4].try_into().map_err(|_| ())?) as usize;
            match decode_bytes(buf, after + 4, len)? {
                Some(v) => v,
                None => return Ok(None),
            }
        }
        0x90..=0x9f => return decode_array(buf, after, (tag & 0x0f) as usize),
        0xdc => {
            want!(3);
            let len = u16::from_be_bytes([buf[after], buf[after + 1]]) as usize;
            return decode_array(buf, after + 2, len);
        }
        0xdd => {
            want!(5);
            let len =
                u32::from_be_bytes(buf[after..after + 4].try_into().map_err(|_| ())?) as usize;
            return decode_array(buf, after + 4, len);
        }
        0x80..=0x8f => return decode_map(buf, after, (tag & 0x0f) as usize),
        0xde => {
            want!(3);
            let len = u16::from_be_bytes([buf[after], buf[after + 1]]) as usize;
            return decode_map(buf, after + 2, len);
        }
        0xdf => {
            want!(5);
            let len =
                u32::from_be_bytes(buf[after..after + 4].try_into().map_err(|_| ())?) as usize;
            return decode_map(buf, after + 4, len);
        }
        // Extension types (Neovim uses these for buffer/window handles). Their
        // length is knowable, so skip them rather than desynchronising.
        0xd4..=0xd8 => {
            let payload = 1usize << (tag - 0xd4);
            want!(1 + payload + 1);
            (Value::Unsupported, after + 1 + payload)
        }
        0xc7 => {
            want!(3);
            let len = buf[after] as usize;
            want!(2 + 1 + len);
            (Value::Unsupported, after + 2 + len)
        }
        0xc8 => {
            want!(4);
            let len = u16::from_be_bytes([buf[after], buf[after + 1]]) as usize;
            want!(3 + 1 + len);
            (Value::Unsupported, after + 3 + len)
        }
        0xc9 => {
            want!(6);
            let len =
                u32::from_be_bytes(buf[after..after + 4].try_into().map_err(|_| ())?) as usize;
            want!(5 + 1 + len);
            (Value::Unsupported, after + 5 + len)
        }
        _ => return Err(()),
    };
    Ok(Some((value, end)))
}

fn decode_bytes(buf: &[u8], at: usize, len: usize) -> Decoded {
    if buf.len() < at + len {
        return Ok(None);
    }
    let text = String::from_utf8_lossy(&buf[at..at + len]).into_owned();
    Ok(Some((Value::Str(text), at + len)))
}

fn decode_array(buf: &[u8], at: usize, len: usize) -> Decoded {
    let mut items = Vec::with_capacity(len.min(64));
    let mut cursor = at;
    for _ in 0..len {
        match decode_at(buf, cursor)? {
            Some((value, next)) => {
                items.push(value);
                cursor = next;
            }
            None => return Ok(None),
        }
    }
    Ok(Some((Value::Array(items), cursor)))
}

fn decode_map(buf: &[u8], at: usize, len: usize) -> Decoded {
    let mut pairs = Vec::with_capacity(len.min(64));
    let mut cursor = at;
    for _ in 0..len {
        let Some((key, next)) = decode_at(buf, cursor)? else {
            return Ok(None);
        };
        let Some((value, next2)) = decode_at(buf, next)? else {
            return Ok(None);
        };
        pairs.push((key, value));
        cursor = next2;
    }
    Ok(Some((Value::Map(pairs), cursor)))
}

/// Decode one complete value from the front of `buf`, if there is one.
pub fn decode(buf: &[u8]) -> Result<Option<(Value, usize)>, ()> {
    decode_at(buf, 0)
}

// ----- the one call we make ----------------------------------------------

/// Evaluate `expr` in the Neovim listening on `socket` and return its result
/// the way `nvim --remote-expr` would have printed it.
///
/// `None` covers every failure the spawn-based version also reported as
/// failure: unreachable socket, Vim error, or -- importantly -- the daemon
/// exiting before it replies, which is exactly what `execute('qall')` does.
pub fn eval(socket: &Path, expr: &str, timeout: Duration) -> Option<String> {
    let mut stream = UnixStream::connect(socket).ok()?;
    stream.set_read_timeout(Some(timeout)).ok()?;
    stream.set_write_timeout(Some(timeout)).ok()?;
    stream
        .write_all(&encode_request(1, "nvim_eval", &[expr]))
        .ok()?;
    stream.flush().ok()?;

    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 4096];
    loop {
        match decode(&buf) {
            Ok(Some((Value::Array(frame), _))) => return response_result(frame),
            // A complete non-array frame is nonsense; treat it as failure.
            Ok(Some(_)) | Err(()) => return None,
            Ok(None) => {}
        }
        match stream.read(&mut chunk) {
            Ok(0) => return None, // closed before replying (e.g. after :qall)
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(_) => return None,
        }
    }
}

/// `[1, msgid, error, result]` -> the display form of `result`.
fn response_result(frame: Vec<Value>) -> Option<String> {
    if frame.len() != 4 {
        return None;
    }
    if frame[0] != Value::Int(1) {
        return None; // not a response (a notification arrived first)
    }
    if !frame[2].is_nil() {
        return None; // Vim reported an error
    }
    Some(frame[3].to_display_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spawn a throwaway headless Neovim the way `daemon::spawn` does and wait
    /// for its socket. Returns `None` when `nvim` is not installed.
    fn live_nvim(name: &str) -> Option<(std::process::Child, std::path::PathBuf)> {
        use std::process::{Command, Stdio};
        let dir = std::env::temp_dir().join(format!("herdr-nvim-rpc-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let socket = dir.join(format!("{name}.sock"));
        let _ = std::fs::remove_file(&socket);
        let child = Command::new("nvim")
            .arg("--headless")
            .arg("-u")
            .arg("NONE")
            .arg("-i")
            .arg("NONE")
            .arg("--listen")
            .arg(&socket)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if UnixStream::connect(&socket).is_ok() {
                return Some((child, socket));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut child = child;
        let _ = child.kill();
        None
    }

    #[test]
    fn evaluates_expressions_against_a_real_neovim() {
        let Some((mut child, socket)) = live_nvim("eval") else {
            return; // no nvim on this machine
        };
        let t = Duration::from_secs(5);

        // Integer results: what modified_count() and wait_for_ui() parse.
        assert_eq!(eval(&socket, "1+1", t).as_deref(), Some("2"));
        assert_eq!(
            eval(&socket, "len(nvim_list_uis())", t).as_deref(),
            Some("0")
        );
        assert_eq!(
            eval(
                &socket,
                "len(filter(getbufinfo({'buflisted':1}),'v:val.changed'))",
                t
            )
            .as_deref(),
            Some("0")
        );

        // String results: what remote_execute() returns.
        assert_eq!(eval(&socket, "'hi'", t).as_deref(), Some("hi"));
        assert!(eval(&socket, "execute('echo 42')", t)
            .unwrap_or_default()
            .contains("42"));

        // Multibyte survives the round trip.
        assert_eq!(eval(&socket, "'héllo ✓'", t).as_deref(), Some("héllo ✓"));

        // A long expression exercises the str8/str16 encoders.
        let long = format!("'{}'", "x".repeat(500));
        assert_eq!(eval(&socket, &long, t).map(|s| s.len()), Some(500));

        // A Vim error is a failure, exactly as a non-zero --remote-expr exit was.
        assert_eq!(eval(&socket, "this_is_not_a_function()", t), None);

        // An unreachable socket fails without hanging.
        assert_eq!(
            eval(std::path::Path::new("/nonexistent/nope.sock"), "1", t),
            None
        );

        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn a_daemon_that_exits_mid_call_reports_failure_not_a_hang() {
        // stop() sends execute('qall'): Neovim exits without replying, and the
        // old spawn-based path surfaced that as None. It must still.
        let Some((mut child, socket)) = live_nvim("quit") else {
            return;
        };
        let started = std::time::Instant::now();
        let out = eval(&socket, "execute('qall')", Duration::from_secs(5));
        assert!(
            out.is_none() || out.as_deref() == Some(""),
            "unexpected result {out:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "returned only on the timeout, not on the closed connection"
        );
        let _ = child.wait();
    }

    #[test]
    fn encodes_the_request_frame_neovim_expects() {
        let bytes = encode_request(1, "nvim_eval", &["1+1"]);
        // [0, 1, "nvim_eval", ["1+1"]]
        assert_eq!(bytes[0], 0x94, "4-element array header");
        assert_eq!(bytes[1], 0x00, "type 0 = request");
        assert_eq!(bytes[2], 0x01, "msgid");
        assert_eq!(bytes[3], 0xa9, "fixstr of length 9");
        assert_eq!(&bytes[4..13], b"nvim_eval");
        assert_eq!(bytes[13], 0x91, "1-element args array");
        assert_eq!(bytes[14], 0xa3, "fixstr of length 3");
        assert_eq!(&bytes[15..18], b"1+1");
        assert_eq!(bytes.len(), 18);
    }

    #[test]
    fn encodes_long_strings_with_the_right_width() {
        let long = "x".repeat(300);
        let bytes = encode_request(1, "nvim_eval", &[&long]);
        assert!(
            bytes.windows(3).any(|w| w[0] == 0xda),
            "str16 header missing"
        );
        let medium = "y".repeat(100);
        let bytes = encode_request(1, "nvim_eval", &[&medium]);
        assert!(bytes.windows(2).any(|w| w[0] == 0xd9 && w[1] == 100));
    }

    fn round(v: &[u8]) -> Value {
        decode(v).expect("valid").expect("complete").0
    }

    #[test]
    fn decodes_the_scalars_neovim_returns() {
        assert_eq!(round(&[0x00]), Value::Int(0));
        assert_eq!(round(&[0x7f]), Value::Int(127));
        assert_eq!(round(&[0xff]), Value::Int(-1));
        assert_eq!(round(&[0xcc, 0xc8]), Value::Int(200));
        assert_eq!(round(&[0xcd, 0x01, 0x00]), Value::Int(256));
        assert_eq!(round(&[0xd1, 0xff, 0x00]), Value::Int(-256));
        assert_eq!(round(&[0xc0]), Value::Nil);
        assert_eq!(round(&[0xc3]), Value::Bool(true));
        assert_eq!(round(&[0xa2, b'h', b'i']), Value::Str("hi".into()));
        // bin8, which Neovim uses for strings with non-UTF8 bytes
        assert_eq!(round(&[0xc4, 0x02, b'o', b'k']), Value::Str("ok".into()));
    }

    #[test]
    fn reports_incomplete_input_instead_of_guessing() {
        assert_eq!(decode(&[]), Ok(None));
        assert_eq!(decode(&[0xa5, b'a']), Ok(None), "truncated string");
        assert_eq!(decode(&[0x94, 0x01]), Ok(None), "truncated array");
        assert_eq!(decode(&[0xc1]), Err(()), "0xc1 is never valid");
    }

    #[test]
    fn extracts_the_result_from_a_response_frame() {
        // [1, 1, nil, 3]
        let frame = vec![Value::Int(1), Value::Int(1), Value::Nil, Value::Int(3)];
        assert_eq!(response_result(frame), Some("3".to_string()));
        // an error response yields None, like a failed --remote-expr
        let frame = vec![
            Value::Int(1),
            Value::Int(1),
            Value::Array(vec![Value::Int(0), Value::Str("boom".into())]),
            Value::Nil,
        ];
        assert_eq!(response_result(frame), None);
        // a notification is not our reply
        let frame = vec![
            Value::Int(2),
            Value::Str("evt".into()),
            Value::Nil,
            Value::Nil,
        ];
        assert_eq!(response_result(frame), None);
        assert_eq!(response_result(vec![Value::Int(1)]), None);
    }

    #[test]
    fn display_form_matches_what_remote_expr_printed() {
        assert_eq!(Value::Int(42).to_display_string(), "42");
        assert_eq!(Value::Str("hi".into()).to_display_string(), "hi");
        assert_eq!(Value::Nil.to_display_string(), "");
        assert_eq!(Value::Bool(true).to_display_string(), "1");
        assert_eq!(
            Value::Array(vec![Value::Str("a".into()), Value::Str("b".into())]).to_display_string(),
            "a\nb"
        );
    }

    #[test]
    fn a_full_response_frame_round_trips() {
        // [1, 1, nil, "done"] as Neovim would send it
        let bytes = [0x94, 0x01, 0x01, 0xc0, 0xa4, b'd', b'o', b'n', b'e'];
        let (value, used) = decode(&bytes).unwrap().unwrap();
        assert_eq!(used, bytes.len());
        let Value::Array(frame) = value else {
            panic!("expected an array")
        };
        assert_eq!(response_result(frame), Some("done".to_string()));
    }

    #[test]
    fn skips_extension_values_without_desynchronising() {
        // fixext1 (0xd4) carries a type byte plus 1 payload byte, then "ok".
        let bytes = [0xd4, 0x00, 0x07, 0xa2, b'o', b'k'];
        let (value, used) = decode(&bytes).unwrap().unwrap();
        assert_eq!(value, Value::Unsupported);
        assert_eq!(used, 3);
        assert_eq!(round(&bytes[used..]), Value::Str("ok".into()));
    }
}
