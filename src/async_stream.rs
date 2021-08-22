//! Use an [async block][async] to produce items for a stream.
//!
//! Example:
//!
//! ```rust ignore
//! use futures_util::StreamExt;
//! use futures_executor::block_on;
//! # use webdav_handler::async_stream;
//! use async_stream::AsyncStream;
//!
//! let mut strm = AsyncStream::<u8, std::io::Error>::new(|mut tx| async move {
//!     for i in 0u8..10 {
//!         tx.send(i).await;
//!     }
//!     Ok(())
//! });
//!
//! let fut = async {
//!     let mut count = 0;
//!     while let Some(item) = strm.next().await {
//!         println!("{:?}", item);
//!         count += 1;
//!     }
//!     assert!(count == 10);
//! };
//! block_on(fut);
//!
//! ```
//!
//! The stream will produce a `Result<Item, Error>` where the `Item`
//! is an item sent with [tx.send(item)][send]. Any errors returned by
//! the async closure will be returned as an error value on
//! the stream.
//!
//! On success the async closure should return `Ok(())`.
//!
//! [async]: https://rust-lang.github.io/async-book/getting_started/async_await_primer.html
//! [send]: async_stream/struct.Sender.html#method.send
//!
use std::cell::Cell;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll};

use futures_util::Stream;

/// Future returned by the Sender.send() method.
///
/// Completes when the item is sent.
#[must_use]
pub struct SenderFuture {
    is_ready: bool,
}

impl SenderFuture {
    fn new() -> SenderFuture {
        SenderFuture { is_ready: false }
    }
}

impl Future for SenderFuture {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.is_ready {
            Poll::Ready(())
        } else {
            self.is_ready = true;
            Poll::Pending
        }
    }
}

// Only internally used by one AsyncStream and never shared
// in any other way, so we don't have to use Arc<Mutex<..>>.
/// Type of the sender passed as first argument into the async closure.
pub struct Sender<I, E>(Rc<Cell<Option<I>>>, PhantomData<E>);
unsafe impl<I: Sync, E: Sync> Sync for Sender<I, E> {}
unsafe impl<I: Send, E: Send> Send for Sender<I, E> {}

impl<I, E> Sender<I, E> {
    fn new(item_opt: Option<I>) -> Sender<I, E> {
        Sender(Rc::new(Cell::new(item_opt)), PhantomData::<E>)
    }

    // note that this is NOT impl Clone for Sender, it's private.
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

/// An abstraction around a future, where the
/// future can internally loop and yield items.
///
/// AsyncStream::new() takes a [Future][Future] ([async closure][async], usually)
/// and AsyncStream then implements a [futures 0.3 Stream][Stream].
///
/// [async]: https://rust-lang.github.io/async-book/getting_started/async_await_primer.html
/// [Future]: https://doc.rust-lang.org/std/future/trait.Future.html
/// [Stream]: https://docs.rs/futures/0.3/futures/stream/trait.Stream.html
#[must_use]
pub struct AsyncStream<Item, Error> {
    item: Sender<Item, Error>,
    fut:  Option<Pin<Box<dyn Future<Output = Result<(), Error>> + 'static + Send>>>,
}

impl<Item, Error: 'static + Send> AsyncStream<Item, Error> {
    /// Create a new stream from a closure returning a Future 0.3,
    /// or an "async closure" (which is the same).
    ///
    /// The closure is passed one argument, the sender, which has a
    /// method "send" that can be called to send a item to the stream.
    pub fn new<F, R>(f: F) -> Self
    where
        F: FnOnce(Sender<Item, Error>) -> R,
        R: Future<Output = Result<(), Error>> + Send + 'static,
        Item: 'static,
    {
        let sender = Sender::new(None);
        AsyncStream::<Item, Error> {
            item: sender.clone(),
            fut:  Some(Box::pin(f(sender))),
        }
    }
}

/// Stream implementation for Futures 0.3.
impl<I, E: Unpin> Stream for AsyncStream<I, E> {
    type Item = Result<I, E>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<I, E>>> {
        let pollres = {
            let fut = self.fut.as_mut().unwrap();
            fut.as_mut().poll(cx)
        };
        match pollres {
            // If the future returned Poll::Ready, that signals the end of the stream.
            Poll::Ready(Ok(_)) => Poll::Ready(None),
            Poll::Ready(Err(e)) => Poll::Ready(Some(Err(e))),
            Poll::Pending => {
                // Pending means that some sub-future returned pending. That sub-future
                // _might_ have been the SenderFuture returned by Sender.send, so
                // check if there is an item available in self.item.
                let mut item = self.item.0.replace(None);
                if item.is_none() {
                    Poll::Pending
                } else {
                    Poll::Ready(Some(Ok(item.take().unwrap())))
                }
            },
        }
    }
}
