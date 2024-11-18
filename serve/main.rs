use bloggthingie::setup;

#[macro_use]
extern crate tracing;

#[tokio::main]
async fn main () -> color_eyre::Result<()> {
    setup();
    todo!();

    Ok(())
}
