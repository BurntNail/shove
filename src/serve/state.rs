use crate::{
    protect::auth::{AuthChecker, AuthReturn},
    s3::get_bucket,
    serve::livereload::LiveReloader,
};
use hyper::{body::Incoming, Request, StatusCode};
use s3::Bucket;
use std::{env, net::SocketAddr, sync::Arc};
use crate::serve::pages::Pages;

#[derive(Clone)]
pub struct State {
    bucket: Box<Bucket>,
    pub tigris_token: Option<Arc<str>>,
    pages: Pages,
    live_reloader: LiveReloader,
    auth: AuthChecker,
}

impl State {

    #[instrument]
    pub async fn new() -> color_eyre::Result<Option<Self>> {
        let bucket = get_bucket();
        let Some(pages) = Pages::new(&bucket).await? else {
            return Ok(None);
        };
        info!("Got bucket & upload data");

        let live_reloader = LiveReloader::new();
        let auth = AuthChecker::new(&bucket).await?;

        let tigris_token = env::var("TIGRIS_TOKEN").ok().map(|x| x.into());
        if tigris_token.is_some() {
            info!("Waiting on Tigris Webhook for reloads");
        } else {
            info!("Checking every 60s for reloads");
        }


        Ok(Some(Self {
            bucket,
            pages,
            tigris_token,
            live_reloader,
            auth,
        }))
    }

    pub fn live_reloader(&self) -> LiveReloader {
        self.live_reloader.clone()
    }

    #[instrument(skip(self))]
    pub async fn check_and_reload(&self) -> color_eyre::Result<()> {
        trace!("Checking for reload");

        if let Err(e) = self.auth.check_and_reload(&self.bucket).await {
            error!(?e, "Error reloading auth checker");
        }
        if let Err(e) = self.pages.check_and_reload(&self.bucket, self.live_reloader.clone()).await {
            error!(?e, "Error reloading pages")
        }

        Ok(())
    }

    #[instrument(skip(self))]
    pub async fn get(&self, path: &str) -> Option<(Vec<u8>, String, StatusCode)> {
        self.pages.get(&self.bucket, path).await
    }

    pub async fn check_auth(
        &self,
        path: &str,
        req: Request<Incoming>,
        remote_addr: SocketAddr,
    ) -> AuthReturn {
        self.auth.check_auth(path, req, remote_addr).await
    }
}
