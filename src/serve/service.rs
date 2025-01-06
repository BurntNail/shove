use crate::{
    protect::auth::AuthReturn,
    serve::{empty_with_code, state::State},
};
use http_body_util::{BodyExt, Full};
use hyper::{
    body::{Bytes, Incoming},
    http,
    service::Service,
    Method, Request, Response, StatusCode,
};
use path_clean::PathClean;
use std::{future::Future, net::SocketAddr, path::Path, pin::Pin, sync::Arc};
use std::convert::Infallible;
use http_body_util::combinators::BoxBody;

pub struct ServeService {
    state: State,
    remote_ip: SocketAddr,
}

impl ServeService {
    pub fn new(state: State, remote_ip: SocketAddr) -> Self {
        Self { state, remote_ip }
    }
}

impl Service<Request<Incoming>> for ServeService {
    type Response = Response<BoxBody<Bytes, Infallible>>;
    type Error = http::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn call(&self, req: Request<Incoming>) -> Self::Future {
        let state = self.state.clone();
        let remote_addr = self.remote_ip;

        Box::pin(async move {
            //thx https://github.com/paritytech/soketto/blob/master/examples/hyper_server.rs
            match *req.method() {
                Method::POST => serve_post(req, state).await,
                Method::GET | Method::HEAD => serve_get_head(req, state, remote_addr).await,
                _ => empty_with_code(StatusCode::METHOD_NOT_ALLOWED),
            }
        })
    }
}

#[instrument(skip(state, req))]
async fn serve_post(
    req: Request<Incoming>,
    state: State,
) -> Result<Response<BoxBody<Bytes, Infallible>>, http::Error> {
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
            if let Err(e) = state.check_and_reload().await {
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
    remote_addr: SocketAddr,
) -> Result<Response<BoxBody<Bytes, Infallible>>, http::Error> {
    let path = req.uri().path();
    if path == "/healthcheck" {
        return empty_with_code(StatusCode::OK);
    }
    if path == "/sse" {
        return state.live_reloader().sse_stream();
    }

    let cleaned = Path::new(path).clean();
    let mut path = match cleaned.to_str() {
        Some(st) => st.to_owned(),
        None => {
            warn!(?cleaned, "Couldn't convert path to string");
            return empty_with_code(StatusCode::BAD_REQUEST);
        }
    };

    if cleaned.extension().is_none_or(|x| x.is_empty()) {
        //ensure that we don't miss zero-index fun
        #[allow(clippy::if_same_then_else)]
        if path.is_empty() {
            path.push('/');
        } else if path.as_bytes()[path.as_bytes().len() - 1] != b'/' {
            path.push('/');
        }
        path.push_str("index.html");
    }

    let req = match state.check_auth(&path, req, remote_addr).await {
        AuthReturn::AuthConfirmed(req) => req,
        AuthReturn::ResponseFromAuth(rsp) => return Ok(rsp),
        AuthReturn::Error(e) => return Err(e),
    };

    trace!(?path, "Serving");

    match state.get(&path).await {
        Some(po) => po.into_response(req.method()),
        None => {
            let rsp = Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(BodyExt::boxed(Full::default()))?;

            Ok(rsp)
        }
    }
}
