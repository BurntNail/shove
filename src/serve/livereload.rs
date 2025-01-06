use std::convert::Infallible;
use hyper::{header, http, Response};
use tokio::sync::broadcast::{channel, Sender};
use futures_util::stream;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::{Bytes, Frame};

#[derive(Clone, Debug)]
pub struct LiveReloader {
    send_reload: Sender<()>,
}

impl LiveReloader {

    pub fn new() -> Self {
        let (send_reload, _rx) = channel(1);

        Self {
            send_reload
        }
    }

    pub fn sse_stream(
        &self,
    ) -> Result<Response<BoxBody<Bytes, Infallible>>, http::Error> {
        let stream = stream::unfold(self.send_reload.subscribe(), |mut rx| async move {
            rx.recv().await.ok().map(|_| (Ok(Frame::data(Bytes::from("event: reload\n\n"))), rx))
        });

        Response::builder()
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(BodyExt::boxed(StreamBody::new(stream)))
    }

    pub fn send_reload(&self) {
        let _ = self.send_reload.send(());
    }
}
