//! Decoding an `aws-chunked` PUT body (#178), one piece at a time as it
//! arrives, so memory stays bounded (#99).
//!
//! The body is a run of chunks, each a hex length, optional extensions such
//! as `;chunk-signature=...`, CRLF, that many bytes, and CRLF. A chunk of
//! length 0 ends it, followed by trailer lines, such as a checksum, and an
//! empty line. Signatures aren't checked, since credentials aren't.

use bytes::Bytes;
use futures::Stream;
use std::io;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt};

/// The longest chunk header or trailer line read.
const MAX_LINE: u64 = 4096;
/// The most trailer lines read.
const MAX_TRAILERS: usize = 16;

/// What decoding a body found, for the PUT to act on once the body has
/// been read.
#[derive(Debug, Default)]
pub struct Outcome {
    /// Why the body isn't a valid `aws-chunked` body, if it isn't.
    pub error: Option<String>,
    /// The trailer lines, names lowercase, values trimmed.
    pub trailers: Vec<(String, String)>,
}

/// Decode the `aws-chunked` body `reader`. A body whose framing is wrong,
/// that ends early, that has bytes after its end, or whose decoded length
/// isn't `decoded_length` when that's given, fails the stream, with the
/// reason in `outcome`.
pub fn decode<R>(
    reader: R,
    decoded_length: Option<u64>,
    outcome: Arc<Mutex<Outcome>>,
) -> impl Stream<Item = io::Result<Bytes>> + Send
where
    R: AsyncBufRead + Unpin + Send,
{
    let decoder = Decoder {
        reader,
        left: 0,
        done: false,
        total: 0,
        decoded_length,
        outcome,
    };
    futures::stream::try_unfold(decoder, |mut d| async move {
        match d.next().await {
            Ok(Some(bytes)) => Ok(Some((bytes, d))),
            Ok(None) => Ok(None),
            Err(Failure::Io(err)) => Err(err),
            Err(Failure::Bad(why)) => {
                d.outcome.lock().unwrap_or_else(|e| e.into_inner()).error = Some(why.clone());
                Err(io::Error::new(io::ErrorKind::InvalidData, why))
            }
        }
    })
}

enum Failure {
    /// The body itself failed, such as a client that went away.
    Io(io::Error),
    /// The body isn't valid `aws-chunked`.
    Bad(String),
}

impl From<io::Error> for Failure {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

fn bad(why: &str) -> Failure {
    Failure::Bad(format!("the aws-chunked body {why}"))
}

struct Decoder<R> {
    reader: R,
    /// Bytes left in the chunk being read.
    left: u64,
    done: bool,
    total: u64,
    decoded_length: Option<u64>,
    outcome: Arc<Mutex<Outcome>>,
}

impl<R: AsyncBufRead + Unpin> Decoder<R> {
    async fn next(&mut self) -> Result<Option<Bytes>, Failure> {
        loop {
            if self.done {
                return Ok(None);
            }
            if self.left > 0 {
                let buf = self.reader.fill_buf().await?;
                if buf.is_empty() {
                    return Err(bad("ends inside a chunk"));
                }
                let n = buf
                    .len()
                    .min(usize::try_from(self.left).unwrap_or(usize::MAX));
                let piece = Bytes::copy_from_slice(&buf[..n]);
                self.reader.consume(n);
                self.left -= n as u64;
                self.total += n as u64;
                if self.left == 0 {
                    let mut crlf = [0u8; 2];
                    match self.reader.read_exact(&mut crlf).await {
                        Ok(_) if &crlf == b"\r\n" => {}
                        Ok(_) => return Err(bad("has a chunk not followed by CRLF")),
                        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => {
                            return Err(bad("ends inside a chunk"))
                        }
                        Err(err) => return Err(err.into()),
                    }
                }
                return Ok(Some(piece));
            }
            let line = self.line().await?;
            let size = line.split(';').next().unwrap_or("").trim();
            if size.is_empty() || size.len() > 16 || !size.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(bad(&format!(
                    "has a chunk header that isn't a hex length: '{line}'"
                )));
            }
            let size = u64::from_str_radix(size, 16).map_err(|_| bad("has a chunk too long"))?;
            if size > 0 {
                self.left = size;
                continue;
            }
            self.finish().await?;
            return Ok(None);
        }
    }

    /// After the last chunk: the trailers, the empty line that ends them,
    /// and nothing after it.
    async fn finish(&mut self) -> Result<(), Failure> {
        let mut trailers = Vec::new();
        loop {
            let line = self.line().await?;
            if line.is_empty() {
                break;
            }
            if trailers.len() == MAX_TRAILERS {
                return Err(bad(&format!("has more than {MAX_TRAILERS} trailers")));
            }
            let (name, value) = line
                .split_once(':')
                .ok_or_else(|| bad(&format!("has a trailer that isn't 'name:value': '{line}'")))?;
            trailers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
        if !self.reader.fill_buf().await?.is_empty() {
            return Err(bad("has bytes after its end"));
        }
        if let Some(expected) = self.decoded_length.filter(|n| *n != self.total) {
            return Err(Failure::Bad(format!(
                "x-amz-decoded-content-length is {expected}, but the aws-chunked body holds {} bytes",
                self.total
            )));
        }
        self.done = true;
        self.outcome
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .trailers = trailers;
        Ok(())
    }

    /// A line ending in CRLF, without it.
    async fn line(&mut self) -> Result<String, Failure> {
        let mut line = Vec::new();
        (&mut self.reader)
            .take(MAX_LINE)
            .read_until(b'\n', &mut line)
            .await?;
        let Some(line) = line.strip_suffix(b"\r\n") else {
            return Err(if line.len() as u64 == MAX_LINE {
                bad("has a line too long")
            } else {
                bad("ends before its last chunk")
            });
        };
        String::from_utf8(line.to_vec()).map_err(|_| bad("has a line that isn't ASCII"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::TryStreamExt;
    use tokio_util::io::StreamReader;

    /// `body` decoded, arriving `piece` bytes at a time.
    async fn run(
        body: &[u8],
        piece: usize,
        decoded_length: Option<u64>,
    ) -> (io::Result<Vec<u8>>, Outcome) {
        let pieces: Vec<io::Result<Bytes>> = body
            .chunks(piece.max(1))
            .map(|c| Ok(Bytes::copy_from_slice(c)))
            .collect();
        let reader = StreamReader::new(futures::stream::iter(pieces));
        let outcome = Arc::new(Mutex::new(Outcome::default()));
        let decoded = decode(Box::pin(reader), decoded_length, Arc::clone(&outcome))
            .try_fold(Vec::new(), |mut acc, b| async move {
                acc.extend_from_slice(&b);
                Ok(acc)
            })
            .await;
        let outcome = std::mem::take(&mut *outcome.lock().unwrap());
        (decoded, outcome)
    }

    #[tokio::test]
    async fn bodies_decode_whatever_pieces_they_arrive_in() {
        let signed = b"5;chunk-signature=abc\r\nhello\r\n6;chunk-signature=def\r\n world\r\n0;chunk-signature=0\r\n\r\n";
        let trailer = b"5\r\nhello\r\n0\r\nx-amz-checksum-crc32:NhCmhg==\r\n\r\n";
        let signed_trailer = b"5;chunk-signature=a\r\nhello\r\n0;chunk-signature=b\r\nX-Amz-Checksum-Crc32: NhCmhg==\r\nx-amz-trailer-signature:c\r\n\r\n";
        for piece in [1, 2, 3, 7, 1000] {
            let (out, outcome) = run(signed, piece, Some(11)).await;
            assert_eq!(out.unwrap(), b"hello world", "piece {piece}");
            assert!(outcome.trailers.is_empty());

            let (out, outcome) = run(trailer, piece, Some(5)).await;
            assert_eq!(out.unwrap(), b"hello");
            assert_eq!(
                outcome.trailers,
                [("x-amz-checksum-crc32".to_string(), "NhCmhg==".to_string())]
            );

            let (out, outcome) = run(signed_trailer, piece, None).await;
            assert_eq!(out.unwrap(), b"hello");
            assert_eq!(outcome.trailers.len(), 2);
            assert_eq!(outcome.trailers[0].0, "x-amz-checksum-crc32");
            assert!(outcome.error.is_none());
        }
    }

    #[tokio::test]
    async fn bodies_that_arent_aws_chunked_fail() {
        for (body, decoded_length, why) in [
            (&b"zz\r\nhello\r\n0\r\n\r\n"[..], None, "hex length"),
            (b"5\r\nhelloXX0\r\n\r\n", None, "CRLF"),
            (b"5\r\nhel", None, "inside a chunk"),
            (b"5\r\nhello\r\n", None, "before its last chunk"),
            (b"5\r\nhello\r\n0\r\n", None, "before its last chunk"),
            (b"5\r\nhello\r\n0\r\n\r\nextra", None, "after its end"),
            (b"5\r\nhello\r\n0\r\nno colon\r\n\r\n", None, "name:value"),
            (
                b"5\r\nhello\r\n0\r\n\r\n",
                Some(6),
                "x-amz-decoded-content-length is 6",
            ),
            (b"11111111111111111\r\n", None, "hex length"),
        ] {
            let (out, outcome) = run(body, 3, decoded_length).await;
            assert!(out.is_err(), "{}", String::from_utf8_lossy(body));
            let error = outcome.error.unwrap_or_default();
            assert!(
                error.contains(why),
                "{}: {error}",
                String::from_utf8_lossy(body)
            );
        }
        let long = format!("{}\r\n", "1".repeat(5000));
        let (out, outcome) = run(long.as_bytes(), 100, None).await;
        assert!(out.is_err());
        assert!(outcome.error.unwrap().contains("too long"));
    }
}
