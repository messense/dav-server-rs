use std::cell::Cell;
use std::fmt;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;

use futures::{Async, Future, Stream};

use futures03;
use futures03::task::{ Poll as Poll03, LocalWaker};
use futures03::future::FutureExt as FutureExt03;
use futures03::future::Future as Future03;
use futures03::compat::Compat as Compat0301;

use bytes;
use hyper;

/// Future returned by the Sender.send() method, completes when the
/// item is sent. All it actually does is to return the "Pending" state
/// once. The next time it is polled it will return "Ready".
pub(crate) struct AsyncSender<E=()> {
    state:      bool,
    phantom:    PhantomData<E>,
}

impl<E> AsyncSender<E> {
    fn new() -> AsyncSender<E> {
        AsyncSender{ state: false, phantom: PhantomData::<E>, }
    }
}

impl<E> Future for AsyncSender<E> {
    type Item = ();
    type Error = E;

    fn poll(&mut self) -> Result<Async<Self::Item>, Self::Error> {
        if self.state {
            Ok(Async::Ready(())
        } else {
            self.state = true;
            Ok(Async::NotReady)
        }
    }
}

impl Future03 for AsyncSender {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, _lw: &LocalWaker) -> Poll03<Self::Output> {
        if self.state {
            Poll03::Ready(())
        } else {
            self.state = true;
            Poll03::Pending
        }
    }
}

// Only internally used by one AsyncStream and never shared
// in any other way, so we don't have to use Arc<Mutex<..>>.
pub(crate) struct Sender<I, E>(Arc<Cell<Option<I>>>, PhantomData<E>);
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
    pub fn send<T>(&mut self, item: T) -> AsyncSender
        where T: Into<I>,
    {
        self.0.set(Some(item.into()));
        AsyncSender::new()
    }

    /// If Item implements From<Vec<u8>>, then the write! macro will
    /// work, and it'll return a AsyncSender future.
    #[allow(dead_code)]
    pub fn write_fmt(&mut self, args: fmt::Arguments) -> AsyncSender
        where I: From<Vec<u8>>
    {
        let mut s = String::new();
        let _ = fmt::write(&mut s, args);
        self.0.set(Some(s.into_bytes().into()));
        AsyncSender::new()
    }
}

/// An AsyncStream is an abstraction around a future, where the
/// future can internally loop and yield items.
///
/// For now it only accepts Future@0.3 and implements Stream@0.1,
/// because it's main use-case is to generate a body stream for
/// a hyper service function.
pub(crate) struct AsyncStream<Item, Error> {
    item:   Sender<Item, Error>,
    fut:    Box<Future<Item=(), Error=Error> + 'static + Send>,
}

impl<Item, Error: 'static + Send> AsyncStream<Item, Error> {
    /// Create a new stream from a closure returning a Future 0.3,
    /// or an "async closure" (which is the same).
    ///
    /// The closure is passed one argument, the sender, which has a
    /// method "send" that can be called to send a item to the stream.
    pub fn stream<F, R>(f: F) -> Self
        where F: FnOnce(Sender<Item, Error>) -> R,
              R: Future03<Output=Result<(), Error>> + Send + 'static,
              Item: 'static,
    {
        let sender = Sender::new(None);
        AsyncStream::<Item, Error> {
            item:   sender.clone(),
            fut:    Box::new(Compat0301::new(f(sender).boxed())),
        }
    }

    /// Create a stream that will produce exactly one item.
    pub fn oneshot<I>(item: I) -> Self
        where I: Into<Item>
    {
        AsyncStream::<Item, Error> {
            item:   Sender::new(Some(item.into())),
            fut:    Box::new(AsyncSender::<Error>::new()),
        }
    }

    /// This is not uber-efficient.
    pub fn empty() -> Self {
        AsyncStream::<Item, Error> {
            item:   Sender::new(None),
            fut:    Box::new(AsyncSender::<Error>::new()),
        }
    }
}

/// Stream implementation for Futures 0.1.
impl<I, E> Stream for AsyncStream<I, E> {
    type Item = I;
    type Error = E;

    fn poll(&mut self) -> Result<Async<Option<Self::Item>>, Self::Error> {
        match self.fut.poll() {
            // If the future returned Async::Ready, that signals the end of the stream.
            Ok(Async::Ready(_)) => Ok(Async::Ready(None)),
            Ok(Async::NotReady) => {
                // Async::NotReady means that there might be new item.
                let mut item = self.item.0.replace(None);
                if item.is_none() {
                    Ok(Async::NotReady)
                } else {
                    Ok(Async::Ready(item.take()))
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
impl<Item, Error> hyper::body::Payload for AsyncStream<Item, Error>
    where Item: bytes::buf::IntoBuf + Send + Sync + 'static,
          Item::Buf: Send,
          Error: std::error::Error + Send + Sync + 'static,
{
    type Data = Item::Buf;
    type Error = Error;

    fn poll_data(&mut self) -> futures::Poll<Option<Self::Data>, Self::Error> {
        match self.poll() {
            Ok(Async::Ready(Some(item))) => Ok(Async::Ready(Some(item.into_buf()))),
            Ok(Async::Ready(None)) => Ok(Async::Ready(None)),
            Ok(Async::NotReady) => Ok(Async::NotReady),
            Err(e) => Err(e),
        }
    }
}

