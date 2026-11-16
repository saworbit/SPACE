//! `aws-chunked` PUT bodies (#178): the framing, the chunk extensions and
//! the trailers, read a few bytes at a time as a slow client sends them.

#![no_main]

use std::io;
use std::pin::{pin, Pin};
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll};

use futures::StreamExt;
use libfuzzer_sys::fuzz_target;
use protocol_s3::aws_chunked::{decode, Outcome};
use tokio::io::{AsyncRead, BufReader, ReadBuf};

/// Hands out the body `step` bytes at a time.
struct Trickle<'a> {
    body: &'a [u8],
    step: usize,
}

impl AsyncRead for Trickle<'_> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let n = self.step.min(self.body.len()).min(buf.remaining());
        let (head, tail) = self.body.split_at(n);
        buf.put_slice(head);
        self.body = tail;
        Poll::Ready(Ok(()))
    }
}

fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
    })
}

fuzz_target!(|input: &[u8]| {
    // The first byte picks how the body arrives, the next two the decoded
    // length the request claims.
    let [control, high, low, body @ ..] = input else {
        return;
    };
    let step = usize::from(control & 0x3f) + 1;
    let decoded_length =
        (control & 0x80 != 0).then(|| u64::from(u16::from_be_bytes([*high, *low])));
    let reader = BufReader::with_capacity(usize::from(control & 0x40) + 8, Trickle { body, step });
    let outcome = Arc::new(Mutex::new(Outcome::default()));

    let (decoded, failed) = runtime().block_on(async {
        let mut stream = pin!(decode(reader, decoded_length, outcome.clone()));
        let mut decoded = 0u64;
        while let Some(piece) = stream.next().await {
            match piece {
                Ok(bytes) => decoded += bytes.len() as u64,
                Err(_) => return (decoded, true),
            }
        }
        (decoded, false)
    });

    let outcome = outcome.lock().unwrap();
    if failed {
        // The reader never fails, so a failed stream is a refused body, and
        // the PUT needs to know why.
        assert!(outcome.error.is_some());
    } else {
        assert!(outcome.error.is_none());
        if let Some(length) = decoded_length {
            assert_eq!(decoded, length);
        }
    }
});
