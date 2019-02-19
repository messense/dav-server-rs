
use futures::prelude::*;
use futures03::future::Future as Future03;
use futures03::future::{FutureExt,TryFutureExt};
use futures03::sink::SinkExt;
use futures03::stream::TryStreamExt;
use futures03::stream::Stream as Stream03;
use futures03::channel::mpsc::Sender as MpscSender03;

use crate::errors::*;

/*
pub(crate) enum MakeStream<I, E> {
    Empty,
    OneShot(I),
    Stream(futures03::stream::Stream
}
*/

async fn map_result<I, E, F>(f: F, mut tx_err: MpscSender03<Result<I, E>>) -> Result<(), ()>
    where F: Future03<Output=Result<(), DavError>> + Send + 'static,
          E: From<DavError>,
{
    match await!(f) {
        Ok(_) |
        Err(DavError::ChanSendError) => {},
        Err(e) => {
            let _ = await!(tx_err.send(Err(e.into())));
        },
    }
    Ok(())
}

pub(crate) fn stream03<I, E, F, R>(fut: F) -> impl Stream03<Item=Result<I, E>>
    where I: Sync + Send + 'static,
          E: std::error::Error + From<DavError> + Sync + Send + 'static,
          F: FnOnce(MpscSender03<Result<I, E>>) -> R,
          R: Future03<Output=Result<(), DavError>> + Send + 'static,
{
    let (tx, rx) = futures03::channel::mpsc::channel::<Result<I, E>>(1);
    let tx_err = tx.clone();
    tokio::spawn(map_result(fut(tx), tx_err).boxed().compat());
    rx
}

pub(crate) fn stream01<I, E, F, R>(fut: F) -> impl Stream<Item=I, Error=E>
    where I: Sync + Send + 'static,
          E: std::error::Error + From<DavError> + Sync + Send + 'static,
          F: FnOnce(MpscSender03<Result<I, E>>) -> R + 'static,
          R: Future03<Output=Result<(), DavError>> + Send + 'static,
{
    stream03(fut).compat()
}

