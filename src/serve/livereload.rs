use color_eyre::eyre::bail;
use futures::{
    io::{BufReader, BufWriter},
    stream::FuturesUnordered,
    StreamExt,
};
use hyper::{body::Incoming, upgrade::Upgraded, Request};
use hyper_util::rt::TokioIo;
use soketto::{handshake::http::Server, Sender};
use std::sync::Arc;
use soketto::connection::Error as SokettoError;
use tokio::sync::Mutex;
use tokio_util::compat::{Compat, TokioAsyncReadCompatExt};

type WSSender = Sender<BufReader<BufWriter<Compat<TokioIo<Upgraded>>>>>;

#[derive(Clone, Debug)]
pub struct LiveReloader {
    senders: Arc<Mutex<Vec<WSSender>>>,
}

impl LiveReloader {
    pub fn new() -> Self {
        Self {
            senders: Arc::new(Mutex::new(vec![])),
        }
    }

    pub async fn handle_livereload(
        &self,
        req: Request<Incoming>,
        server: Server,
    ) -> color_eyre::Result<()> {
        let stream = hyper::upgrade::on(req).await?;
        let io = TokioIo::new(stream);
        let stream = BufReader::new(BufWriter::new(io.compat()));

        let (sender, _) = server.into_builder(stream).finish();

        self.senders.lock().await.push(sender);

        Ok(())
    }

    pub async fn send_reload(&self) -> color_eyre::Result<()> {
        async fn reload(mut sender: WSSender) -> color_eyre::Result<()> {
            fn handle (res: Result<(), SokettoError>) -> color_eyre::Result<()> {
                match res {
                    Ok(()) | Err(SokettoError::Closed) => Ok(()),
                    Err(e) => Err(e.into())
                }
            }

            handle(sender.send_text("reload").await)?;
            handle(sender.flush().await)?;
            handle(sender.close().await)?;

            Ok(())
        }

        let Ok(mut senders) = self.senders.try_lock() else {
            bail!("Already reloading");
        };

        let senders = std::mem::take::<Vec<_>>(senders.as_mut());
        let mut fo: FuturesUnordered<_> = senders.into_iter().map(reload).collect();

        while let Some(res) = fo.next().await {
            if let Err(e) = res {
                error!(?e, "Error sending reload message");
            }
        }

        Ok(())
    }

    pub async fn send_stop(&self) -> color_eyre::Result<()> {
        async fn stop(mut sender: WSSender) -> color_eyre::Result<()> {
            match sender.close().await {
                Ok(()) | Err(SokettoError::Closed) => Ok(()),
                Err(e) => Err(e.into())
            }
        }

        let senders = std::mem::take::<Vec<_>>(self.senders.lock().await.as_mut());
        let mut fo: FuturesUnordered<_> = senders.into_iter().map(stop).collect();

        while let Some(res) = fo.next().await {
            if let Err(e) = res {
                error!(?e, "Error closing sender");
            }
        }

        Ok(())
    }
}
