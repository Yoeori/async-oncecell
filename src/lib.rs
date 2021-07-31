//! Asynchronous implementation of OnceCell and Lazy.
//!
//! This package provides an asynchronous implementation of OnceCell and Lazy, which is usefull in cases where you would like to instantiate
//! these cells with asynchronous code.

#![warn(missing_docs)]
#![crate_name = "async_oncecell"]

use std::{
    cell::UnsafeCell,
    convert::Infallible,
    fmt,
    future::Future,
    pin::Pin,
    ptr,
    sync::atomic::{AtomicPtr, AtomicUsize, Ordering},
};

use futures::lock::Mutex;

/// Cell which can be lazy instantiated with an asynchronous block and is safely share-able between threads.
pub struct OnceCell<T> {
    state: AtomicUsize,
    queue: AtomicPtr<SetupQueue>,
    inner: UnsafeCell<Option<T>>,
}

// Justification: UnsafeCell is not Sync, however is only mutable in set which is guarded by the lock.
unsafe impl<T: Sync + Send> Sync for OnceCell<T> {}
unsafe impl<T: Send> Send for OnceCell<T> {}

struct SetupQueue {
    lock: Mutex<()>,
}

const NEW: usize = 0;
const WAITERS_MASK: usize = usize::MAX >> 1;
const READY_BIT: usize = WAITERS_MASK + 1;

impl<T> OnceCell<T> {
    /// Creates a new empty OnceCell.
    pub const fn new() -> Self {
        Self {
            state: AtomicUsize::new(NEW),
            queue: AtomicPtr::new(ptr::null_mut()),
            inner: UnsafeCell::new(None),
        }
    }

    /// Get or initialize this cell with the given asynchronous block. If the cell is already initialized, the current value will be returned.
    /// Otherwise the asynchronous block is used to initialize the OnceCell. This function will always return a value.
    ///
    /// # Example
    /// ```rust
    /// # use async_oncecell::*;
    /// # tokio_test::block_on(async {
    /// let cell = OnceCell::new();
    /// cell.get_or_init(async {
    ///     0 // expensive calculation
    /// }).await;
    /// assert_eq!(cell.get(), Some(&0));
    /// # })
    /// ```
    pub async fn get_or_init<F>(&self, f: F) -> &T
    where
        F: Future<Output = T>,
    {
        match self
            .get_or_try_init(async { Ok::<_, Infallible>(f.await) })
            .await
        {
            Ok(res) => res,
            Err(e) => match e {},
        }
    }

    /// Get or initialize this cell with the given asynchronous block. If the cell is already initialized, the current value will be returned.
    /// Otherwise the asynchronous block is used to initialize the OnceCell. This function will always return a value.
    ///
    /// # Example
    /// ```rust
    /// # use async_oncecell::*;
    /// # use std::convert::Infallible;
    /// # tokio_test::block_on(async {
    /// let cell = OnceCell::new();
    /// cell.get_or_try_init(async {
    ///     Ok::<_, Infallible>(0) // expensive calculation
    /// }).await;
    /// assert_eq!(cell.get(), Some(&0));
    /// # })
    /// ```
    pub async fn get_or_try_init<F, E>(&self, f: F) -> Result<&T, E>
    where
        F: Future<Output = Result<T, E>>,
    {
        let state = self.state.load(Ordering::Acquire);

        if state & READY_BIT == 0 {
            self.try_init_slow(f).await?;
        }

        // Initializer did not return an error which means we can safely unwrap the value.
        unsafe {
            // Note: we could use unwrap_unchecked here, when that is stable
            Ok((*self.inner.get()).as_ref().unwrap())
        }
    }

    #[cold]
    async fn try_init_slow<E>(&self, init: impl Future<Output = Result<T, E>>) -> Result<(), E> {
        // yes, it's actually a bit
        debug_assert!(READY_BIT.is_power_of_two());

        // Add an entry to the waiter list.  This ensures that queue won't be freed until we exit.
        let old_state = self.state.fetch_add(1, Ordering::Acquire);

        // Now that we hold a place on the list, remove it on drop.
        let guard = RemoveWaiter(self);

        if old_state & READY_BIT != 0 {
            // Very unlikely, but OK: it finished between the load and the fetch_add
            return Ok(());
        }

        // This is split into a helper function so that the Future is Send
        fn init_queue(shared: &AtomicPtr<SetupQueue>) -> &SetupQueue {
            let mut queue = shared.load(Ordering::Acquire);
            if queue.is_null() {
                // Race with other callers of try_init_slow to initialize the queue
                let new_queue = Box::into_raw(Box::new(SetupQueue {
                    lock: Mutex::new(()),
                }));
                match shared.compare_exchange(
                    ptr::null_mut(),
                    new_queue,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_null) => {
                        // Normal case, it was actually set.  The Release part of AcqRel orders this
                        // with all Acquires on the queue.
                        queue = new_queue;
                    }
                    Err(actual) => {
                        // we lost the race, but we have the (non-null) value now.
                        debug_assert!(!actual.is_null(), "compare_exchange failed");
                        queue = actual;
                        // Safety: we just allocated it, and nobody else has seen it
                        unsafe {
                            Box::from_raw(new_queue);
                        }
                    }
                }
            }
            // Safety: we checked that queue isn't null, and we hold a place on the waiter list, so
            // nobody will free it from under us until _guard is dropped.
            unsafe { &*queue }
        }

        let queue = init_queue(&self.queue);

        // Use the Mutex to wake others when init completes and also ensure only one assignment happens.
        let lock = queue.lock.lock().await;

        // See if someone else got the mutex first and set the value.  We hold the lock now, so
        // there are no ordering concerns related to the ready bit (though other bits might be
        // modified by RemoveWaiter).
        let state = self.state.load(Ordering::Acquire);
        if state & READY_BIT != 0 {
            return Ok(());
        }

        let value = init.await?;

        // Safety: we hold the lock, and we just checked that the value was empty
        unsafe {
            *self.inner.get() = Some(value);
        }

        // This Release pairs with the Acquire in get() or any other time we check READY_BIT.
        let prev_state = self.state.fetch_or(READY_BIT, Ordering::Release);

        // we hold the lock, and nobody else sets READY_BIT
        debug_assert_eq!(
            prev_state & READY_BIT,
            0,
            "Invalid state: somoene else set READY_BIT"
        );

        // the reference from our guard must still be present
        debug_assert!(prev_state >= 1, "Invalid reference count");

        // The drop of the lock will wake up each other caller of try_init_slow, and everyone else
        // now has lock-free access to the value.
        drop(lock);
        drop(guard);

        struct RemoveWaiter<'a, T>(&'a OnceCell<T>);

        impl<'a, T> Drop for RemoveWaiter<'a, T> {
            fn drop(&mut self) {
                let prev_state = self.0.state.fetch_sub(1, Ordering::Relaxed);
                if prev_state == READY_BIT + 1 {
                    // We just removed the only waiter on an initialized cell.  This means the
                    // queue is no longer needed.
                    let queue = self.0.queue.swap(ptr::null_mut(), Ordering::Acquire);
                    if !queue.is_null() {
                        // Safety: the last guard is being freed, and queue is only used by
                        // guard-holders.
                        unsafe {
                            Box::from_raw(queue);
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Returns the value of the OnceCell or None if the OnceCell has not been initialized yet.
    pub fn get(&self) -> Option<&T> {
        let state = self.state.load(Ordering::Acquire);

        if state & READY_BIT == 0 {
            None
        } else {
            // ready, so it's safe to inspect
            unsafe { (*self.inner.get()).as_ref() }
        }
    }

    /// If the OnceCell is already initialized, either by having have called [`get_or_init()`](Self::get_or_init()) or [`get_or_try_init()`](Self::get_or_try_init()).
    pub fn initialized(&self) -> bool {
        let state = self.state.load(Ordering::Relaxed);

        state & READY_BIT != 0
    }
}

impl<T> Drop for OnceCell<T> {
    fn drop(&mut self) {
        let queue = *self.queue.get_mut();
        if !queue.is_null() {
            // Safety: nobody else could have a reference
            unsafe {
                Box::from_raw(queue);
            }
        }
    }
}

impl<T> Default for OnceCell<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: fmt::Debug> fmt::Debug for OnceCell<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("OnceCell").field(&self.get()).finish()
    }
}

impl<T: PartialEq> PartialEq for OnceCell<T> {
    fn eq(&self, other: &Self) -> bool {
        self.get() == other.get()
    }
}

impl<T: Eq> Eq for OnceCell<T> {}

/// Lazy cell which is only instantiated upon first retreival of contained value
pub struct Lazy<T, F = Pin<Box<dyn Future<Output = T> + Send>>> {
    cell: OnceCell<T>,
    f: Mutex<Option<F>>,
}

impl<T> Lazy<T, Pin<Box<dyn Future<Output = T> + Send>>> {
    /// Creates a new Lazy with the given instantiator.
    ///
    /// # Example
    /// ```rust
    /// # use async_oncecell::*;
    /// # use std::convert::Infallible;
    /// # tokio_test::block_on(async {
    /// let lazy = Lazy::new(async {
    ///     0 // Expensive calculation    
    /// });
    /// assert_eq!(lazy.get().await, &0);
    /// # })
    /// ```
    pub fn new(f: impl Future<Output = T> + 'static + Send) -> Self {
        Self {
            cell: OnceCell::new(),
            f: Mutex::new(Some(Box::pin(f))),
        }
    }
}

impl<T> Lazy<T, Pin<Box<dyn Future<Output = T> + Send>>> {
    /// Retreives the contents of the Lazy. If not instantiated, the instantiator block will be executed first.
    ///
    /// # Panics
    ///
    /// Panics on the second invocation, if the first invocation was dropped without completing.
    pub async fn get(&self) -> &T {
        self.cell
            .get_or_init(async { self.f.lock().await.take().unwrap().await })
            .await
    }
}

impl<T: fmt::Debug, F> fmt::Debug for Lazy<T, F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Lazy").field(&self.cell.get()).finish()
    }
}

#[cfg(test)]
mod test {
    use std::sync::Arc;

    use super::*;

    #[tokio::test]
    async fn test_once_cell() {
        let cell: OnceCell<i32> = OnceCell::new();
        assert_eq!(cell.get(), None);

        let v = cell.get_or_init(async { 0 }).await;
        assert_eq!(v, &0);
        assert_eq!(cell.get(), Some(&0));
    }

    #[tokio::test]
    async fn test_once_cell_across_threads() {
        let cell: Arc<OnceCell<i32>> = Arc::new(OnceCell::new());
        let cell_clone1 = cell.clone();

        let handler = tokio::spawn(async move {
            cell_clone1.get_or_init(async { 0 }).await;
        });

        assert!(handler.await.is_ok());
        assert_eq!(cell.get(), Some(&0));
    }

    #[tokio::test]
    async fn test_lazy() {
        let lazy = Lazy::new(async { 0 });
        assert_eq!(lazy.get().await, &0);
        assert_eq!(lazy.get().await, &0);
    }

    #[tokio::test]
    async fn test_lazy_multi_threaded() {
        let t = 5;
        let lazy = Arc::new(Lazy::new(async move { t }));
        let lazy_clone = lazy.clone();

        let handle = tokio::spawn(async move {
            assert_eq!(lazy_clone.get().await, &t);
        });

        assert!(handle.await.is_ok());
        assert_eq!(lazy.get().await, &t);
    }

    #[tokio::test]
    async fn test_lazy_struct() {
        struct Test {
            lazy: Lazy<i32>,
        }

        let data = Test {
            lazy: Lazy::new(async { 0 }),
        };

        assert_eq!(data.lazy.get().await, &0);
    }
}
