use color_eyre::eyre::bail;
use futures::{
    io::{BufReader, BufWriter},
    stream::FuturesUnordered,
    StreamExt,
};
use hyper::{body::Incoming, upgrade::Upgraded, Request};
use hyper_util::rt::TokioIo;
use soketto::{
    connection::Error as SokettoError, data::ByteSlice125, handshake::http::Server,
    Incoming as WsIncoming,
};
use std::{sync::Arc, time::Duration};
use tokio::sync::{
    mpsc::{channel, Sender},
    Mutex,
};
use tokio_util::compat::{Compat, TokioAsyncReadCompatExt};

type WsSender = soketto::Sender<BufReader<BufWriter<Compat<TokioIo<Upgraded>>>>>;
type WsReceiver = soketto::Receiver<BufReader<BufWriter<Compat<TokioIo<Upgraded>>>>>;

#[derive(Clone, Debug)]
pub struct LiveReloader {
    senders: Arc<Mutex<Vec<(WsSender, WsReceiver)>>>,
    stop_dead_check: Sender<()>,
}

impl LiveReloader {
    pub fn new() -> Self {
        let senders: Arc<Mutex<Vec<(WsSender, WsReceiver)>>> = Arc::new(Mutex::new(vec![]));
        let dead_check_senders = senders.clone();
        let (stop_dead_check, mut stop_rx) = channel(1);
        tokio::task::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(10)) => {}
                    _ = stop_rx.recv() => {
                        break;
                    }
                }

                async fn handle_tx_and_rx(tx: &mut WsSender, rx: &mut WsReceiver) -> bool {
                    //can't use b"" as that gives a const byte array, not a slice :(
                    const PING: &[u8] = "get pinged, loser".as_bytes();

                    //stupid struct doesn't implement copy OR clone
                    //https://github.com/paritytech/soketto/issues/118 ?
                    //(got merged, but not yet released smh)
                    let ping_msg = || ByteSlice125::try_from(PING).unwrap();

                    let mut needs_to_be_removed = false;

                    if let Err(e) = tx.send_ping(ping_msg()).await {
                        match e {
                            SokettoError::Closed => {
                                needs_to_be_removed = true;
                            }
                            other => {
                                warn!(?other, "Error sending ping to WS");
                            }
                        }
                    }
                    if let Err(e) = tx.flush().await {
                        match e {
                            SokettoError::Closed => {
                                needs_to_be_removed = true;
                            }
                            other => {
                                warn!(?other, "Error flushing WS");
                            }
                        }
                    }

                    let mut output = vec![];
                    match rx.receive(&mut output).await {
                        Ok(incoming) => match incoming {
                            WsIncoming::Data(data) => {
                                trace!(?data, "Received data from WS???");
                            }
                            WsIncoming::Pong(pong) => {
                                if pong != PING {
                                    warn!(found=?pong, expected=?PING, "different ping/pong as expected");
                                }
                            }
                            WsIncoming::Closed(_reason) => needs_to_be_removed = true,
                        },
                        Err(e) => {
                            warn!(?e, "Error reading from WS");
                        }
                    }

                    needs_to_be_removed
                }

                let mut senders_and_receivers = dead_check_senders.lock().await;
                let mut needs_to_be_removed: FuturesUnordered<_> = senders_and_receivers
                    .iter_mut()
                    .enumerate()
                    .map(|(i, (tx, rx))| async move {
                        if handle_tx_and_rx(tx, rx).await {
                            Some(i)
                        } else {
                            None
                        }
                    })
                    .collect();

                let mut tbr = vec![];
                //there's a better way to do this, but i can't find it :(
                #[allow(clippy::manual_flatten, for_loops_over_fallibles)]
                for res in needs_to_be_removed.next().await {
                    if let Some(res) = res {
                        tbr.push(res);
                    }
                }
                drop(needs_to_be_removed);

                let senders_left = senders_and_receivers.len() - tbr.len();
                info!(removed=%tbr.len(), %senders_left, "removing dead senders");
                
                //yes, there's probably a performance penalty, but really?
                //like this is so easy to read
                tbr.sort();
                tbr.reverse();
                for i in tbr {
                    senders_and_receivers.remove(i);
                }
            }
        });

        Self {
            senders,
            stop_dead_check,
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

        let ws = server.into_builder(stream).finish();

        self.senders.lock().await.push(ws);

        Ok(())
    }

    pub async fn send_reload(&self) -> color_eyre::Result<()> {
        async fn reload(mut sender: WsSender) -> color_eyre::Result<()> {
            fn handle(res: Result<(), SokettoError>) -> color_eyre::Result<()> {
                match res {
                    Ok(()) | Err(SokettoError::Closed) => Ok(()),
                    Err(e) => Err(e.into()),
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
        let mut fo: FuturesUnordered<_> = senders
            .into_iter()
            .map(|(sender, _)| reload(sender))
            .collect();

        while let Some(res) = fo.next().await {
            if let Err(e) = res {
                error!(?e, "Error sending reload message");
            }
        }

        Ok(())
    }

    pub async fn send_stop(&self) -> color_eyre::Result<()> {
        async fn stop(mut sender: WsSender) -> color_eyre::Result<()> {
            match sender.close().await {
                Ok(()) | Err(SokettoError::Closed) => Ok(()),
                Err(e) => Err(e.into()),
            }
        }

        let _ = self.stop_dead_check.send(()).await;

        let senders = std::mem::take::<Vec<_>>(self.senders.lock().await.as_mut());
        let mut fo: FuturesUnordered<_> = senders
            .into_iter()
            .map(|(sender, _)| stop(sender))
            .collect();

        while let Some(res) = fo.next().await {
            if let Err(e) = res {
                error!(?e, "Error closing sender");
            }
        }

        Ok(())
    }
}
