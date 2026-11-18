use std::cell::Cell;
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::task::{Context, Poll};

thread_local! {
    static CONTAINED: Cell<bool> = const { Cell::new(false) };
}

pub fn is_contained() -> bool {
    CONTAINED.with(Cell::get)
}

struct Restore(bool);

impl Drop for Restore {
    fn drop(&mut self) {
        CONTAINED.with(|c| c.set(self.0));
    }
}

fn enter() -> Restore {
    Restore(CONTAINED.with(|c| c.replace(true)))
}

pub fn contain<R>(work: impl FnOnce() -> R) -> std::thread::Result<R> {
    let _restore = enter();
    catch_unwind(AssertUnwindSafe(work))
}

pub struct Contained<F> {
    inner: Option<Pin<Box<F>>>,
}

impl<F: Future> Contained<F> {
    pub fn new(inner: F) -> Self {
        Self {
            inner: Some(Box::pin(inner)),
        }
    }
}

impl<F: Future> Future for Contained<F> {
    type Output = std::thread::Result<F::Output>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let Some(inner) = self.inner.as_mut() else {
            return Poll::Pending;
        };
        let polled = {
            let _restore = enter();
            catch_unwind(AssertUnwindSafe(|| inner.as_mut().poll(cx)))
        };
        match polled {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(out)) => {
                self.inner = None;
                Poll::Ready(Ok(out))
            }
            Err(payload) => {
                self.inner = None;
                Poll::Ready(Err(payload))
            }
        }
    }
}

pub fn message(payload: &(dyn std::any::Any + Send)) -> Option<&str> {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
}

#[cfg(test)]
mod tests;
