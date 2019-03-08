//! CoroStream - async closure used in a coroutine-like way to yield
//! items to a stream.
//!
//! Example:
//!
//! ```no_run
//! let strm = CoroStream::new(async move |mut tx| {
//!     for i in 0..10 {
//!         tx.send(format!("number {}", i));
//!     }
//!     Ok::<_, std::io::Error>(())
//! });
//! strm.for_each(|num| {
//!     println!("{:?}", num);
//! };
//! ```
//!
//! The stream will produce an Item/Error (for 0.1 streams)
//! or a Result<Item, Error> (for 0.3 streams) where the Item
//! is an item sent with tx.send(item). Any errors returned by
//! the async closure will be returned as the final item.
//!
//! On success the async closure should return Ok(()).
//!
use std::cell::Cell;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;

use futures01::Async as Async01;
use futures01::Future as Future01;
use futures01::Poll as Poll01;
use futures01::Stream as Stream01;

use futures::compat::Compat as Compat03As01;
use futures::Future as Future03;
use futures::Stream as Stream03;
use futures::task::Poll as Poll03;
use futures::task::Waker;

use bytes;
use hyper;

/// Future returned by the Sender.send() method.
///
/// Completes when the item is sent.
pub struct SenderFuture<E = ()> {
    state:   bool,
    phantom: PhantomData<E>,
}

impl<E> SenderFuture<E> {
    fn new() -> SenderFuture<E> {
        SenderFuture {
            state:   false,
            phantom: PhantomData::<E>,
        }
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
/// Type of the sender passed as first argument into the async closure.
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
    where T: Into<I> {
        self.0.set(Some(item.into()));
        SenderFuture::new()
    }
}

/// A CoroStream is an abstraction around a future, where the
/// future can internally loop and yield items.
///
/// CoroStream::new() takes a futures 0.3 Future (async closure, usually)
/// and CoroStream then implements both a futures 0.1 Stream and a
/// futures 0.3 Stream.
pub struct CoroStream<Item, Error> {
    item: Sender<Item, Error>,
    fut:  Option<Pin<Box<Future03<Output = Result<(), Error>> + 'static + Send>>>,
}

impl<Item, Error: 'static + Send> CoroStream<Item, Error> {
    /// Create a new stream from a closure returning a Future 0.3,
    /// or an "async closure" (which is the same).
    ///
    /// The closure is passed one argument, the sender, which has a
    /// method "send" that can be called to send a item to the stream.
    ///
    /// The CoroStream instance that is returned impl's both
    /// a futures 0.1 Stream and a futures 0.3 Stream.
    pub fn new<F, R>(f: F) -> Self
    where
        F: FnOnce(Sender<Item, Error>) -> R,
        R: Future03<Output = Result<(), Error>> + Send + 'static,
        Item: 'static,
    {
        let sender = Sender::new(None);
        CoroStream::<Item, Error> {
            item: sender.clone(),
            fut:  Some(Box::pin(f(sender))),
        }
    }
}

/// Stream implementation for Futures 0.1.
impl<I, E> Stream01 for CoroStream<I, E> {
    type Item = I;
    type Error = E;

    fn poll(&mut self) -> Result<Async01<Option<Self::Item>>, Self::Error> {
        // We use a futures::compat::Compat wrapper to be able to call
        // the futures 0.3 Future in a futures 0.1 context. Because
        // the Compat wrapper wants to to take ownership, the future
        // is stored in an Option which we can temporarily move it out
        // of, and then move it back in.
        let mut fut = Compat03As01::new(self.fut.take().unwrap());
        let pollres = fut.poll();
        self.fut.replace(fut.into_inner());
        match pollres {
            // If the future returned Async::Ready, that signals the end of the stream.
            Ok(Async01::Ready(_)) => Ok(Async01::Ready(None)),
            Ok(Async01::NotReady) => {
                // Async::NotReady means that there is a new item.
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

/// Stream implementation for Futures 0.3.
impl<I, E: Unpin> Stream03 for CoroStream<I, E> {
    type Item = Result<I, E>;

    fn poll_next(mut self: Pin<&mut Self>, waker: &Waker) -> Poll03<Option<Result<I, E>>> {
        let pollres = {
            let fut = self.fut.as_mut().unwrap();
            fut.as_mut().poll(waker)
        };
        match pollres {
            // If the future returned Poll::Ready, that signals the end of the stream.
            Poll03::Ready(Ok(_)) => Poll03::Ready(None),
            Poll03::Ready(Err(e)) => Poll03::Ready(Some(Err(e))),
            Poll03::Pending => {
                // Pending means that there is a new item.
                let mut item = self.item.0.replace(None);
                if item.is_none() {
                    Poll03::Pending
                } else {
                    Poll03::Ready(Some(Ok(item.take().unwrap())))
                }
            },
        }
    }
}

/// hyper::body::Payload trait implementation.
///
/// This implementation allows you to use anything that implements
/// IntoBuf as a Payload item.
impl<Item, Error> hyper::body::Payload for CoroStream<Item, Error>
where
    Item: bytes::buf::IntoBuf + Send + Sync + 'static,
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
