//! flect-engine: reads JSON Lines requests on stdin, writes events on stdout.
//! P1 provides the protocol, `export-menu` and task routing; task bodies land in P2+.
use std::io::{self, BufRead, Write};

use flect_core::menu;
use flect_core::protocol::{parse_line, Event, Outcome, Request, MAX_LINE_BYTES};

fn emit(out: &mut impl Write, event: &Event) -> io::Result<()> {
    serde_json::to_writer(&mut *out, event)?;
    out.write_all(b"\n")?;
    out.flush()
}

fn error(key: &str, args: Vec<String>) -> Event {
    Event::Error {
        key: key.into(),
        args,
    }
}

fn failed(out: &mut impl Write, key: &str, args: Vec<String>) -> io::Result<()> {
    emit(out, &error(key, args))?;
    emit(
        out,
        &Event::Done {
            outcome: Outcome::Failed,
            summary_key: None,
        },
    )
}

/// `image.convert?to=PNG` -> `image.convert`; known when it names a catalog entry or its parent.
fn known_task(task: &str) -> bool {
    let name = task.split('?').next().unwrap_or("");
    !name.is_empty()
        && menu::leaf_ids(&menu::catalog()).iter().any(|id| {
            *id == name
                || id
                    .strip_prefix(name)
                    .is_some_and(|rest| rest.starts_with('.'))
        })
}

/// Read one line, never buffering more than the protocol limit.
fn read_bounded(input: &mut impl BufRead, buf: &mut Vec<u8>) -> io::Result<Option<bool>> {
    buf.clear();
    let mut too_long = false;
    loop {
        let chunk = input.fill_buf()?;
        if chunk.is_empty() {
            return Ok(if buf.is_empty() && !too_long {
                None
            } else {
                Some(too_long)
            });
        }
        let (take, done) = match chunk.iter().position(|b| *b == b'\n') {
            Some(i) => (i + 1, true),
            None => (chunk.len(), false),
        };
        if !too_long {
            buf.extend_from_slice(&chunk[..take]);
            if buf.len() > MAX_LINE_BYTES + 1 {
                too_long = true;
                buf.clear();
            }
        }
        input.consume(take);
        if done {
            return Ok(Some(too_long));
        }
    }
}

fn main() -> io::Result<()> {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut buf = Vec::new();

    while let Some(too_long) = read_bounded(&mut input, &mut buf)? {
        if too_long {
            failed(&mut out, "protocol.lineTooLong", vec![])?;
            continue;
        }
        if buf.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        match parse_line(&buf) {
            Err(e) => failed(&mut out, e.key(), vec![])?,
            Ok(Request::ExportMenu) => emit(
                &mut out,
                &Event::Menu {
                    menu: menu::export(),
                },
            )?,
            Ok(Request::Cancel) => emit(
                &mut out,
                &Event::Done {
                    outcome: Outcome::Cancelled,
                    summary_key: None,
                },
            )?,
            Ok(Request::Run { task, .. }) => {
                if known_task(&task) {
                    failed(&mut out, "task.unimplemented", vec![task])?;
                } else {
                    failed(&mut out, "task.unknown", vec![task])?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_tasks() {
        assert!(known_task("image.resize.p1"));
        assert!(known_task("image.convert?to=PNG"));
        assert!(known_task("video.compress"));
        assert!(!known_task("image.nope"));
        assert!(!known_task("imag"));
        assert!(!known_task(""));
    }

    #[test]
    fn bounded_reader_survives_oversized_lines() {
        let mut data = vec![b'a'; MAX_LINE_BYTES * 3];
        data.push(b'\n');
        data.extend_from_slice(b"{\"op\":\"cancel\"}\n");
        let mut r = io::Cursor::new(data);
        let mut buf = Vec::new();
        assert_eq!(read_bounded(&mut r, &mut buf).unwrap(), Some(true));
        assert_eq!(read_bounded(&mut r, &mut buf).unwrap(), Some(false));
        assert!(parse_line(&buf).is_ok());
        assert_eq!(read_bounded(&mut r, &mut buf).unwrap(), None);
    }
}
