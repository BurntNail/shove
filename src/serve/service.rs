use http_body_util::Full;
use hyper::{
    body::{Bytes, Incoming},
    http,
    service::Service,
    Method, Request, Response, StatusCode,
};
use std::{future::Future, path::PathBuf, pin::Pin};
use crate::serve::state::State;

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
            let is_head = req.method() == Method::HEAD;
            if !(req.method() == Method::GET || is_head) {
                let rsp = Response::builder()
                    .status(StatusCode::METHOD_NOT_ALLOWED)
                    .body(Full::default())?;

                return Ok(rsp);
            };

            let path = req.uri().path();
            if path == "/healthcheck" {
                let rsp = Response::builder()
                    .status(StatusCode::OK)
                    .body(Full::default())?;
                return Ok(rsp);
            }

            let mut path = path.to_string();

            if PathBuf::from(&path)
                .extension()
                .is_none_or(|x| x.is_empty())
            {
                if path.as_bytes()[path.as_bytes().len() - 1] != b'/' {
                    path.push('/');
                }
                path.push_str("index.html");
            }

            trace!(?path, "Serving");

            match state.get(&path).await {
                Some((content, content_type, sc)) => {
                    let builder = Response::builder()
                        .status(sc)
                        .header("Content-Type", content_type)
                        .header("Content-Length", content.len());

                    if is_head {
                        Ok(builder.body(Full::default())?)
                    } else {
                        Ok(builder.body(Full::new(Bytes::from(content)))?)
                    }
                }
                None => {
                    let rsp = Response::builder()
                        .status(StatusCode::NOT_FOUND)
                        .body(Full::default())?;

                    Ok(rsp)
                }
            }
        })
    }
}
