use crate::state::State;
use http_body_util::Full;
use hyper::{
    body::{Bytes, Incoming},
    http,
    service::Service,
    Request, Response, StatusCode,
};
use std::{future::Future, pin::Pin};

#[derive(Debug, Clone)]
pub struct ServeService {
    pub(crate) state: State,
}

impl Service<Request<Incoming>> for ServeService {
    type Response = Response<Full<Bytes>>;
    type Error = http::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn call(&self, req: Request<Incoming>) -> Self::Future {
        let state = self.state.clone();

        Box::pin(async move {
            let path = req.uri().path();
            let mut path = path.to_string();
            if path.is_empty() || path.as_bytes()[path.as_bytes().len() - 1] == b'/' {
                path.push_str("index.html");
            }

            trace!(?path, "Serving");

            match state.get(&path).await {
                Some((content, content_type, sc)) => {
                    let rsp = Response::builder()
                        .status(sc)
                        .header("Content-Type", content_type)
                        .body(Full::new(Bytes::from(content)))?;

                    Ok(rsp)
                }
                None => {
                    let rsp = Response::builder()
                        .status(StatusCode::NOT_FOUND)
                        .body(Full::new(Bytes::new()))?;

                    Ok(rsp)
                }
            }
        })
    }
}
