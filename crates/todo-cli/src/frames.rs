//! A remote daemon's response body, split at its gRPC-Web frames before
//! tonic-web reads it: each data chunk holds exactly one whole frame.
//!
//! tonic-web 0.14's client loses the trailer frame when it arrives in the
//! same chunk as the last message. It returns the message, keeps the
//! trailers, and then ends the stream without them, so every call fails with
//! "missing grpc-status trailer". The daemon writes each frame as a chunk of
//! its own, but a proxy between it and the client may merge chunks, and
//! `tailscale serve` (the remote listener's usual front, ADR-0008) does.

use std::pin::Pin;
use std::task::{Context, Poll, ready};

use bytes::{Buf, BufMut as _, Bytes, BytesMut};
use http_body::{Body, Frame};
use tonic::codegen::http::HeaderMap;

/// The flag byte and the big-endian length in front of every frame.
const HEADER: usize = 5;

pub(crate) struct SplitFrames<B> {
    inner: B,
    buf: BytesMut,
    /// The inner body has ended (with these HTTP trailers, if any).
    ended: bool,
    trailers: Option<HeaderMap>,
}

impl<B> SplitFrames<B> {
    pub(crate) fn new(inner: B) -> Self {
        SplitFrames {
            inner,
            buf: BytesMut::new(),
            ended: false,
            trailers: None,
        }
    }

    /// The first whole frame in the buffer, if one has arrived.
    fn whole_frame(&mut self) -> Option<Bytes> {
        let header: [u8; HEADER] = self.buf.get(..HEADER)?.try_into().ok()?;
        let len = u32::from_be_bytes([header[1], header[2], header[3], header[4]]);
        let total = HEADER.checked_add(usize::try_from(len).ok()?)?;
        (self.buf.len() >= total).then(|| self.buf.split_to(total).freeze())
    }
}

impl<B> Body for SplitFrames<B>
where
    B: Body + Unpin,
    B::Data: Buf,
{
    type Data = Bytes;
    type Error = B::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, B::Error>>> {
        let this = &mut *self;
        loop {
            if let Some(frame) = this.whole_frame() {
                return Poll::Ready(Some(Ok(Frame::data(frame))));
            }
            if this.ended {
                // A cut-off frame goes on as it is, for tonic-web to report.
                if !this.buf.is_empty() {
                    return Poll::Ready(Some(Ok(Frame::data(this.buf.split().freeze()))));
                }
                return Poll::Ready(this.trailers.take().map(|t| Ok(Frame::trailers(t))));
            }
            match ready!(Pin::new(&mut this.inner).poll_frame(cx)) {
                Some(Ok(frame)) => match frame.into_data() {
                    Ok(data) => this.buf.put(data),
                    Err(frame) => {
                        // HTTP trailers end the body; they follow the frames.
                        this.trailers = frame.into_trailers().ok();
                        this.ended = true;
                    }
                },
                Some(Err(e)) => return Poll::Ready(Some(Err(e))),
                None => this.ended = true,
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.ended && self.buf.is_empty() && self.trailers.is_none()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::convert::Infallible;

    use super::*;

    /// A body that yields `chunks` as they are, one data frame each.
    struct Chunks(VecDeque<Bytes>);

    impl Body for Chunks {
        type Data = Bytes;
        type Error = Infallible;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
            Poll::Ready(self.0.pop_front().map(|c| Ok(Frame::data(c))))
        }
    }

    fn frame(flag: u8, payload: &[u8]) -> Vec<u8> {
        let mut f = vec![flag];
        f.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_be_bytes());
        f.extend_from_slice(payload);
        f
    }

    async fn split(chunks: Vec<Vec<u8>>) -> Vec<Bytes> {
        let mut body = SplitFrames::new(Chunks(chunks.into_iter().map(Bytes::from).collect()));
        let mut out = Vec::new();
        while let Some(frame) = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await
        {
            out.push(frame.unwrap().into_data().unwrap());
        }
        out
    }

    #[tokio::test]
    async fn separates_a_message_and_trailers_sent_as_one_chunk() {
        let message = frame(0, b"\x0a\x05hello");
        let trailers = frame(0x80, b"grpc-status:0\r\n");
        let merged = [message.clone(), trailers.clone()].concat();
        assert_eq!(split(vec![merged]).await, [message, trailers]);
    }

    #[tokio::test]
    async fn joins_a_frame_split_across_chunks() {
        let message = frame(0, b"a longer message");
        let trailers = frame(0x80, b"grpc-status:0\r\n");
        let all = [message.clone(), trailers.clone()].concat();
        let chunks = all.chunks(3).map(<[u8]>::to_vec).collect();
        assert_eq!(split(chunks).await, [message, trailers]);
    }

    #[tokio::test]
    async fn passes_on_a_cut_off_frame() {
        let mut cut = frame(0, b"cut off");
        cut.truncate(8);
        assert_eq!(split(vec![cut.clone()]).await, [cut]);
    }
}
