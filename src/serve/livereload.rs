use std::sync::Arc;
use hyper::body::Incoming;
use hyper::Request;
use hyper_util::rt::TokioIo;
use soketto::handshake::http::Server;
use tokio_util::compat::{Compat, TokioAsyncReadCompatExt};
use futures::io::{BufWriter, BufReader};
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use hyper::upgrade::Upgraded;
use soketto::Sender;
use tokio::sync::Mutex;

type WSSender = Sender<BufReader<BufWriter<Compat<TokioIo<Upgraded>>>>>;

#[derive(Clone, Debug)]
pub struct LiveReloader {
    senders: Arc<Mutex<Vec<WSSender>>>
}

impl LiveReloader {
    pub fn new () -> Self {
        Self {
            senders: Arc::new(Mutex::new(vec![]))
        }
    }

    pub async fn handle_livereload (&self, req: Request<Incoming>, server: Server) -> color_eyre::Result<()> {
        let stream = hyper::upgrade::on(req).await?;
        let io = TokioIo::new(stream);
        let stream = BufReader::new(BufWriter::new(io.compat()));

        let (sender, _receiver) = server.into_builder(stream).finish();

        self.senders.lock().await.push(sender);

        Ok(())
    }

    pub async fn send_reload (&self) -> color_eyre::Result<()> {
        async fn reload (mut sender: WSSender) -> color_eyre::Result<()> {
            sender.send_text("reload").await?;
            sender.flush().await?;
            sender.close().await?;
            Ok(())
        }

        let senders = std::mem::take::<Vec<_>>(self.senders.lock().await.as_mut());
        let mut fo: FuturesUnordered<_> = senders.into_iter().map(reload).collect();

        while let Some(res) = fo.next().await {
            if let Err(e) = res {
                error!(?e, "Error sending reload message");
            }
        }

        Ok(())
    }

    pub async fn send_stop (&self) -> color_eyre::Result<()> {
        async fn stop (mut sender: WSSender) -> color_eyre::Result<()> {
            sender.close().await?;
            Ok(())
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