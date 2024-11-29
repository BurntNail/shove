use crate::serve::state::State;
use http_body_util::Full;
use hyper::{
    body::{Bytes, Incoming},
    http,
    service::Service,
    Method, Request, Response, StatusCode,
};
use soketto::handshake::http::{is_upgrade_request, Server};
use std::{future::Future, path::PathBuf, pin::Pin, sync::Arc};

#[derive(Debug, Clone)]
pub struct ServeService {
    state: State,
}

impl ServeService {
    pub fn new(state: State) -> Self {
        Self { state }
    }
}

impl Service<Request<Incoming>> for ServeService {
    type Response = Response<Full<Bytes>>;
    type Error = http::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn call(&self, req: Request<Incoming>) -> Self::Future {
        let state = self.state.clone();
        let livereload = state.live_reloader();

        Box::pin(async move {
            //thx https://github.com/paritytech/soketto/blob/master/examples/hyper_server.rs
            if is_upgrade_request(&req) {
                let mut handshake_server = Server::new();

                match handshake_server.receive_request(&req) {
                    Ok(rsp) => {
                        tokio::spawn(async move {
                            if let Err(e) =
                                livereload.handle_livereload(req, handshake_server).await
                            {
                                error!(?e, "Error with websockets");
                            }
                        });
                        Ok(rsp.map(|()| Full::default()))
                    }
                    Err(e) => {
                        error!(?e, "Couldn't upgrade connection");
                        empty_with_code(StatusCode::INTERNAL_SERVER_ERROR)
                    }
                }
            } else {
                match *req.method() {
                    Method::POST => serve_post(req, state).await,
                    Method::GET | Method::HEAD => serve_get_head(req, state).await,
                    _ => empty_with_code(StatusCode::METHOD_NOT_ALLOWED),
                }
            }
        })
    }
}

fn empty_with_code(code: StatusCode) -> Result<Response<Full<Bytes>>, http::Error> {
    Response::builder().status(code).body(Full::default())
}

#[instrument(skip(state, req))]
async fn serve_post(
    req: Request<Incoming>,
    state: State,
) -> Result<Response<Full<Bytes>>, http::Error> {
    match req.uri().path() {
        "/reload" => {
            let Some(actual_tigris_token) = state.tigris_token.clone() else {
                return empty_with_code(StatusCode::METHOD_NOT_ALLOWED);
            };

            if req.uri().path() != "/reload" {
                return empty_with_code(StatusCode::NOT_FOUND);
            }

            let headers = req.headers();
            let provided_auth_token = match headers.get("Authorization").cloned() {
                Some(x) => match x.to_str() {
                    Ok(x) => match x.strip_prefix("Bearer ") {
                        Some(x) => Arc::<str>::from(x),
                        None => {
                            warn!("Unable to find Bearer part");
                            return empty_with_code(StatusCode::BAD_REQUEST);
                        }
                    },
                    Err(e) => {
                        warn!(?e, "Error converting auth token to string");
                        return empty_with_code(StatusCode::BAD_REQUEST);
                    }
                },
                None => return empty_with_code(StatusCode::BAD_REQUEST),
            };

            if actual_tigris_token != provided_auth_token {
                warn!("Tried to reload with incorrect token");
                return empty_with_code(StatusCode::FORBIDDEN);
            }

            info!("Reloading from webhook");
            if let Err(e) = state.reload().await {
                error!(?e, "Error reloading state");
                empty_with_code(StatusCode::INTERNAL_SERVER_ERROR)
            } else {
                empty_with_code(StatusCode::OK)
            }
        }
        _ => empty_with_code(StatusCode::METHOD_NOT_ALLOWED),
    }
}

#[instrument(skip(req, state))]
async fn serve_get_head(
    req: Request<Incoming>,
    state: State,
) -> Result<Response<Full<Bytes>>, http::Error> {
    let path = req.uri().path();
    if path == "/healthcheck" {
        return empty_with_code(StatusCode::OK);
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

            if req.method() == Method::HEAD {
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
}
