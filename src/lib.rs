//! Asynchronous implementation of OnceCell and Lazy.
//!
//! This package provides an asynchronous implementation of OnceCell and Lazy, which is usefull in cases where you would like to instantiate
//! these cells with asynchronous code.
//!
//! This package is currently still in development and should _not_ be used in production code. While heavily inspired by existing
//! OnceCell packages, it should not be seen as _safe_. My understanding of unsafe Rust is still rudimentary, and while
//! I have done my best to justify the unsafe code in this crate, I currently do not have the knowledge to fully do so.

#![warn(missing_docs)]
#![crate_name = "async_oncecell"]

use std::{
    cell::UnsafeCell,
    convert::Infallible,
    fmt,
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicBool, Ordering},
};

use futures::lock::Mutex;

/// Cell which can be lazy instantiated with an asynchronous block and is safely share-able between threads.
pub struct OnceCell<T> {
    lock: Mutex<()>,
    initialized: AtomicBool,
    inner: UnsafeCell<Option<T>>,
}

// Justification: UnsafeCell is not Sync, however is only mutable in set which is guarded by the lock.
unsafe impl<T: Sync + Send> Sync for OnceCell<T> {}
unsafe impl<T: Send> Send for OnceCell<T> {}

impl<T> OnceCell<T> {
    /// Creates a new empty OnceCell. Currently this function is not const due to Mutex limitations,
    /// so to share between multiple threads an Arc needs to be used.
    pub fn new() -> Self {
        // TODO: Mutex is not const, need to find suitable lock which can be const
        Self {
            lock: Mutex::new(()),
            initialized: AtomicBool::new(false),
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
            Err(_) => unreachable!(),
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
        if !self.initialized.load(Ordering::Acquire) {
            self.set(f).await?;
        }

        // Initializer did not return an error which means we can safely unwrap the value.
        Ok(self.get().unwrap())
    }

    /// Returns the value of the OnceCell or None if the OnceCell has not been initialized yet.
    pub fn get(&self) -> Option<&T> {
        unsafe { &*self.inner.get() }.as_ref()
    }

    async fn set<F, E>(&self, f: F) -> Result<(), E>
    where
        F: Future<Output = Result<T, E>>,
    {
        // Lock function (gets unlocked at the end of the function)
        let _guard = self.lock.lock().await;

        if !self.initialized.load(Ordering::Acquire) {
            match f.await {
                Ok(v) => {
                    // Justification: Mutation is guarded by lock, and has no references pointed to it since value cannot be set (Some) yet.
                    unsafe {
                        *self.inner.get() = Some(v);
                    };
                    self.initialized.store(true, Ordering::Release);
                    Ok(())
                }
                Err(e) => Err(e),
            }
        } else {
            Ok(())
        }
    }

    /// If the OnceCell is already initialized, either by having have called [`get_or_init()`](Self::get_or_init()) or [`get_or_try_init()`](Self::get_or_try_init()).
    pub fn initialized(&self) -> bool {
        self.initialized.load(Ordering::Acquire)
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
