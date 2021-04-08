# async-oncecell

[![Crates.io][crates-badge]][crates-url]
[![MIT licensed][mit-badge]][mit-url]
[![Continuous integration][actions-badge]][actions-url]

[crates-badge]: https://img.shields.io/crates/v/async-oncecell
[crates-url]: https://crates.io/crates/async-oncecell
[mit-badge]: https://img.shields.io/crates/l/async-oncecell
[mit-url]: https://github.com/Yoeori/async-oncecell/blob/master/LICENSE
[actions-badge]: https://github.com/Yoeori/async-oncecell/actions/workflows/main.yml/badge.svg
[actions-url]: https://github.com/Yoeori/async-oncecell/actions/workflows/main.yml

This crate offers an asynchronous version of Rust's `OnceCell` and `Lazy` structs, which allows its users to use `async`/`await` inside of the OnceCell and Lazy constructors.

The crate _should_ work with any stable asynchronous runtime like [`tokio`](https://github.com/tokio-rs/tokio) and [`async-std`](https://github.com/async-rs/async-std) and only depends on the `futures` crate for an async aware [`Mutex`](https://docs.rs/futures/0.3.13/futures/lock/struct.Mutex.html).

## Usage
The `OnceCell` can be used the similarly to the other popular OnceCell crates, and the implementation in the standard library with the exception that a Future is given instead of a closjure and that some functions return a Future.

### Example
```rust
use async_oncecell::OnceCell;

#[tokio::main]
async fn main() {
    let cell = OnceCell::new();
    let v = cell.get_or_init(async { 
        // Expensive operation
     }).await;
    assert_eq!(cell.get(), Some(v));
}
```

The same holds for `Lazy`, however since the `get()` method needs to be awaited, no automatic dereferencing is implemented.

### Example
```rust
use async_oncecell::Lazy;

#[tokio::main]
async fn main() {
    let lazy_value = Lazy::new(async {
        // Expensive operation
    });
    assert_eq!(lazy_value.get().await, /* result of expensive operation */);
}
```

## Stability
This crate is extremely new and has therefore not extensively been tested. Next to that, the author is also inexperienced in working with `unsafe` Rust. While care has been taken in making this crate safe, it is not recommended for production use. As mentioned in the Contributing section: feedback and pull requests are very much appreciated, even more so if you see any problems with the soundness of the crate.

## Contributing
Contributions are always welcome! Please create an issue or pull request if you have any insight which might help to improve this crate. This is my first _real_ Rust crate, so I'm eager to learn from the community.

## Wishlist
In the future I hope to add asynchronous transform structs as well to be able to transform data lazily.