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

        let raw_path = req.uri().path();
        let path = if !raw_path.is_empty() {
            let mut path = format!("{}{}", state.file_root_dir().to_string_lossy(), raw_path);

            let bytes = path.as_bytes();
            if bytes[bytes.len() - 1] == b'/' {
                path.push_str("index.html");
            }

            path
        } else {
            "index.html".to_string()
        };

        info!(?path, ?raw_path, "Serving");

        Box::pin(async move {
            match state.get(&path).await {
                Some((content, content_type)) => {
                    let rsp = Response::builder()
                        .status(StatusCode::OK)
                        .header("Content-Type", content_type)
                        .body(Full::new(Bytes::from(content)))?;

                    Ok(rsp)
                }
                None => match state.not_found() {
                    Some((content, content_type)) => {
                        let rsp = Response::builder()
                            .status(StatusCode::NOT_FOUND)
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
                },
            }
        })
    }
}
