use hyper::body::Incoming;
use hyper::Request;
use hyper_util::rt::TokioIo;
use soketto::handshake::http::Server;
use tokio::sync::broadcast::Receiver;
use tokio_util::compat::TokioAsyncReadCompatExt;
use futures::io::{BufWriter, BufReader};

pub async fn handle_livereload (req: Request<Incoming>, server: Server, mut stop_rx: Receiver<()>, mut reload_rx: Receiver<()>) -> color_eyre::Result<()> {
    let stream = hyper::upgrade::on(req).await?;
    let io = TokioIo::new(stream);
    let stream = BufReader::new(BufWriter::new(io.compat()));

    let (mut sender, mut receiver) = server.into_builder(stream).finish();

    let mut message = Vec::new();
    loop {
        message.clear();

        tokio::select! {
            _ = stop_rx.recv() => {
                sender.close().await?;
                break;
            },
            _ = reload_rx.recv() => {
                trace!("Reloading");
                sender.send_text("reload").await?;
                sender.flush().await?;
                sender.close().await?;
                break;
            }
            msg = receiver.receive_data(&mut message) => {
                match msg {
                    Ok(soketto::Data::Binary(n)) => {
                        debug!(?message, "Received binary data");
                    }
                    Ok(soketto::Data::Text(n)) => {
                        assert_eq!(n, message.len());

                        if let Ok(txt) = std::str::from_utf8(&message) {
                            trace!(?txt, "Received text data");

                            if txt == "ping" {
                                sender.send_text("pong").await?;
                                sender.flush().await?;
                            }
                        } else {
                            warn!("Unable to decode message from WS");
                            break;
                        }
                    }
                    Err(soketto::connection::Error::Closed) => break,
                    Err(e) => {
                        warn!(?e, "Websocket connection error");
                    }
                }
            }
        }
    }

    Ok(())
}