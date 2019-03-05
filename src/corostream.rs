use std::cell::Cell;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;

use futures::Async as Async01;
use futures::Future as Future01;
use futures::Poll as Poll01;
use futures::Stream as Stream01;

use futures03::task::Poll as Poll03;
use futures03::task::Waker;
use futures03::future::FutureExt as _;
use futures03::future::Future as Future03;
use futures03::compat::Compat as Compat03As01;
use futures03::compat::Compat01As03;

use bytes;
use hyper;

/// Future returned by the Sender.send() method, completes when the
/// item is sent. All it actually does is to return the "Pending" state
/// once. The next time it is polled it will return "Ready".
pub struct SenderFuture<E=()> {
    state:      bool,
    phantom:    PhantomData<E>,
}

impl<E> SenderFuture<E> {
    fn new() -> SenderFuture<E> {
        SenderFuture{ state: false, phantom: PhantomData::<E>, }
    }
}

impl<E> Future01 for SenderFuture<E> {
    type Item = ();
    type Error = E;

    fn poll(&mut self) -> Result<Async01<Self::Item>, Self::Error> {
        if self.state {
            Ok(Async01::Ready(())
        } else {
            self.state = true;
            Ok(Async01::NotReady)
        }
    }
}

impl Future03 for SenderFuture {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, _waker: &Waker) -> Poll03<Self::Output> {
        if self.state {
            Poll03::Ready(())
        } else {
            self.state = true;
            Poll03::Pending
        }
    }
}

// Only internally used by one CoroStream and never shared
// in any other way, so we don't have to use Arc<Mutex<..>>.
pub struct Sender<I, E>(Arc<Cell<Option<I>>>, PhantomData<E>);
unsafe impl<I, E> Sync for Sender<I, E> {}
unsafe impl<I, E> Send for Sender<I, E> {}

impl<I, E> Sender<I, E> {
    fn new(item_opt: Option<I>) -> Sender<I, E> {
        Sender(Arc::new(Cell::new(item_opt)), PhantomData::<E>)
    }

    fn clone(&self) -> Sender<I, E> {
        Sender(self.0.clone(), PhantomData::<E>)
    }

    /// Send one item to the stream.
    pub fn send<T>(&mut self, item: T) -> SenderFuture
        where T: Into<I>,
    {
        self.0.set(Some(item.into()));
        SenderFuture::new()
    }
}

/// A CoroStream is an abstraction around a future, where the
/// future can internally loop and yield items.
///
/// For now it only accepts Future@0.3 and implements Stream@0.1,
/// because it's main use-case is to generate a body stream for
/// a hyper service function.
pub struct CoroStream<Item, Error> {
    item:   Sender<Item, Error>,
    fut:    Box<Future01<Item=(), Error=Error> + 'static + Send>,
}

impl<Item, Error: 'static + Send> CoroStream<Item, Error> {
    /// Create a new stream from a closure returning a Future 0.3,
    /// or an "async closure" (which is the same).
    ///
    /// The closure is passed one argument, the sender, which has a
    /// method "send" that can be called to send a item to the stream.
    pub fn stream01<F, R>(f: F) -> Self
        where F: FnOnce(Sender<Item, Error>) -> R,
              R: Future03<Output=Result<(), Error>> + Send + 'static,
              Item: 'static,
    {
        let sender = Sender::new(None);
        CoroStream::<Item, Error> {
            item:   sender.clone(),
            fut:    Box::new(Compat03As01::new(f(sender).boxed())),
        }
    }

    pub fn stream03<F, R>(f: F) -> Compat01As03<CoroStream<Item, Error>>
        where F: FnOnce(Sender<Item, Error>) -> R,
              R: Future03<Output=Result<(), Error>> + Send + 'static,
              Item: 'static,
    {
        Compat01As03::new(CoroStream::stream01(f))
    }
}

/// Stream implementation for Futures 0.1.
impl<I, E> Stream01 for CoroStream<I, E> {
    type Item = I;
    type Error = E;

    fn poll(&mut self) -> Result<Async01<Option<Self::Item>>, Self::Error> {
        match self.fut.poll() {
            // If the future returned Async::Ready, that signals the end of the stream.
            Ok(Async01::Ready(_)) => Ok(Async01::Ready(None)),
            Ok(Async01::NotReady) => {
                // Async::NotReady means that there might be new item.
                let mut item = self.item.0.replace(None);
                if item.is_none() {
                    Ok(Async01::NotReady)
                } else {
                    Ok(Async01::Ready(item.take()))
                }
            },
            Err(e) => Err(e),
        }
    }
}

/// hyper::body::Payload trait implementation.
///
/// This implementation allows you to use anything that implements
/// IntoBuf as a Payload item.
impl<Item, Error> hyper::body::Payload for CoroStream<Item, Error>
    where Item: bytes::buf::IntoBuf + Send + Sync + 'static,
          Item::Buf: Send,
          Error: std::error::Error + Send + Sync + 'static,
{
    type Data = Item::Buf;
    type Error = Error;

    fn poll_data(&mut self) -> Poll01<Option<Self::Data>, Self::Error> {
        match self.poll() {
            Ok(Async01::Ready(Some(item))) => Ok(Async01::Ready(Some(item.into_buf()))),
            Ok(Async01::Ready(None)) => Ok(Async01::Ready(None)),
            Ok(Async01::NotReady) => Ok(Async01::NotReady),
            Err(e) => Err(e),
        }
    }
}

