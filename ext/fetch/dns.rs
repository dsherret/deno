// Copyright 2018-2026 the Deno authors. MIT license.
use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;
use std::task::{self};
use std::time::Duration;
use std::time::Instant;
use std::vec;

use deno_core::parking_lot::Mutex;
use hickory_resolver::name_server::TokioConnectionProvider;
use hyper_util::client::legacy::connect::dns::GaiResolver;
use hyper_util::client::legacy::connect::dns::Name;
use tokio::task::JoinHandle;
use tower::Service;

#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub enum Resolver {
  /// A resolver using blocking `getaddrinfo` calls in a threadpool.
  Gai(GaiResolver),
  /// hickory-resolver's userspace resolver.
  Hickory(hickory_resolver::Resolver<TokioConnectionProvider>),
  /// A custom resolver that implements `Resolve`.
  Custom(Arc<dyn Resolve>),
}

/// Alias for the `Future` type returned by a custom DNS resolver.
// The future has to be `Send` as `tokio::spawn` is used to execute the future.
pub type Resolving =
  Pin<Box<dyn Future<Output = Result<SocketAddrs, io::Error>> + Send>>;

/// A trait for customizing DNS resolution in ext/fetch.
// The resolver needs to be `Send` and `Sync` for two reasons. One is it is
// wrapped inside an `Arc` and will be cloned and moved to an async block to
// perfrom DNS resolution. That async block will be executed by `tokio::spawn`,
// so to make that async block `Send`, `Arc<dyn Resolve>` needs to be
// `Send`. The other is `Resolver` needs to be `Send` to make the wrapping
// `HttpConnector` `Send`.
pub trait Resolve: Send + Sync + std::fmt::Debug {
  fn resolve(&self, name: Name) -> Resolving;
}

impl Default for Resolver {
  fn default() -> Self {
    Self::gai()
  }
}

impl Resolver {
  pub fn gai() -> Self {
    Self::Gai(GaiResolver::new())
  }

  /// Create a [`AsyncResolver`] from system conf.
  pub fn hickory() -> Result<Self, hickory_resolver::ResolveError> {
    Ok(Self::Hickory(
      hickory_resolver::Resolver::builder_tokio()?.build(),
    ))
  }

  pub fn hickory_from_resolver(
    resolver: hickory_resolver::Resolver<TokioConnectionProvider>,
  ) -> Self {
    Self::Hickory(resolver)
  }

  /// Create a GAI resolver wrapped with an in-memory DNS cache.
  ///
  /// Caches resolved addresses for `ttl` to avoid redundant `getaddrinfo`
  /// calls. Particularly useful during package installation where many
  /// concurrent requests target the same registry hostname.
  pub fn gai_cached(ttl: Duration) -> Self {
    Self::Custom(Arc::new(CachingResolver {
      inner: Self::gai(),
      cache: Default::default(),
      ttl,
    }))
  }
}

type SocketAddrs = vec::IntoIter<SocketAddr>;

pub struct ResolveFut {
  inner: JoinHandle<Result<SocketAddrs, io::Error>>,
}

impl Future for ResolveFut {
  type Output = Result<SocketAddrs, io::Error>;

  fn poll(
    mut self: Pin<&mut Self>,
    cx: &mut task::Context<'_>,
  ) -> Poll<Self::Output> {
    Pin::new(&mut self.inner).poll(cx).map(|res| match res {
      Ok(Ok(addrs)) => Ok(addrs),
      Ok(Err(e)) => Err(e),
      Err(join_err) => {
        if join_err.is_cancelled() {
          Err(io::Error::new(io::ErrorKind::Interrupted, join_err))
        } else {
          Err(io::Error::other(join_err))
        }
      }
    })
  }
}

impl Service<Name> for Resolver {
  type Response = SocketAddrs;
  type Error = io::Error;
  type Future = ResolveFut;

  fn poll_ready(
    &mut self,
    _cx: &mut task::Context<'_>,
  ) -> Poll<Result<(), io::Error>> {
    Poll::Ready(Ok(()))
  }

  fn call(&mut self, name: Name) -> Self::Future {
    let task = match self {
      Resolver::Gai(gai_resolver) => {
        let mut resolver = gai_resolver.clone();
        tokio::spawn(async move {
          let result = resolver.call(name).await?;
          let x: Vec<_> = result.into_iter().collect();
          let iter: SocketAddrs = x.into_iter();
          Ok(iter)
        })
      }
      Resolver::Hickory(async_resolver) => {
        let resolver = async_resolver.clone();
        tokio::spawn(async move {
          let result = resolver.lookup_ip(name.as_str()).await?;

          let x: Vec<_> =
            result.into_iter().map(|x| SocketAddr::new(x, 0)).collect();
          let iter: SocketAddrs = x.into_iter();
          Ok(iter)
        })
      }
      Resolver::Custom(resolver) => {
        let resolver = resolver.clone();
        tokio::spawn(async move { resolver.resolve(name).await })
      }
    };
    ResolveFut { inner: task }
  }
}

/// A DNS resolver that caches results from an inner resolver for a
/// configurable TTL. This avoids redundant `getaddrinfo` calls when
/// many concurrent HTTP requests target the same hostname (e.g. during
/// `deno install` where all npm tarball downloads hit `registry.npmjs.org`).
///
/// In-flight lookups are deduplicated: if multiple callers resolve the
/// same hostname concurrently, only one `getaddrinfo` call is made and
/// all callers share the result via a `Shared` future.
struct CachingResolver {
  inner: Resolver,
  cache: Arc<Mutex<HashMap<String, CacheState>>>,
  ttl: Duration,
}

type SharedResolveFut = futures::future::Shared<
  Pin<Box<dyn Future<Output = Result<Vec<SocketAddr>, String>> + Send>>,
>;

enum CacheState {
  /// A lookup is in progress; additional callers share this future.
  InFlight(SharedResolveFut),
  /// A completed lookup with a TTL.
  Ready {
    addrs: Vec<SocketAddr>,
    expires: Instant,
  },
}

impl std::fmt::Debug for CachingResolver {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("CachingResolver")
      .field("inner", &self.inner)
      .field("ttl", &self.ttl)
      .finish()
  }
}

impl Resolve for CachingResolver {
  fn resolve(&self, name: Name) -> Resolving {
    use futures::FutureExt;
    let mut cache = self.cache.lock();

    let shared = match cache.get(name.as_str()) {
      Some(CacheState::Ready { addrs, expires })
        if *expires > Instant::now() =>
      {
        let addrs = addrs.clone();
        return Box::pin(async move { Ok(addrs.into_iter()) });
      }
      Some(CacheState::InFlight(shared)) => shared.clone(),
      _ => {
        // Expired or not present — start a new lookup.
        // Cache promotion/cleanup lives inside the shared future so
        // it runs exactly once even if outer callers are dropped.
        let mut inner = self.inner.clone();
        let cache_ref = self.cache.clone();
        let ttl = self.ttl;
        let name_key = name.to_string();

        let resolve_fut: Pin<
          Box<dyn Future<Output = Result<Vec<SocketAddr>, String>> + Send>,
        > = Box::pin(async move {
          let name_key = name.to_string();
          let result = inner
            .call(name)
            .await
            .map(|addrs| addrs.collect::<Vec<_>>())
            .map_err(|e| e.to_string());

          let mut cache = cache_ref.lock();
          match &result {
            Ok(addrs) => {
              cache.insert(
                name_key,
                CacheState::Ready {
                  addrs: addrs.clone(),
                  expires: Instant::now() + ttl,
                },
              );
            }
            Err(_) => {
              cache.remove(&name_key);
            }
          }

          result
        });

        let shared = resolve_fut.shared();
        cache.insert(name_key, CacheState::InFlight(shared.clone()));
        shared
      }
    };
    drop(cache);

    Box::pin(async move {
      shared
        .await
        .map(|addrs| addrs.into_iter())
        .map_err(io::Error::other)
    })
  }
}

#[cfg(test)]
mod tests {
  use std::str::FromStr;

  use super::*;

  // A resolver that resolves any name into the same address.
  #[derive(Debug)]
  struct DebugResolver(SocketAddr);

  impl Resolve for DebugResolver {
    fn resolve(&self, _name: Name) -> Resolving {
      let addr = self.0;
      Box::pin(async move { Ok(vec![addr].into_iter()) })
    }
  }

  #[tokio::test]
  async fn custom_dns_resolver() {
    let mut resolver = Resolver::Custom(Arc::new(DebugResolver(
      "127.0.0.1:8080".parse().unwrap(),
    )));
    let mut addr = resolver
      .call(Name::from_str("foo.com").unwrap())
      .await
      .unwrap();

    let addr = addr.next().unwrap();
    assert_eq!(addr, "127.0.0.1:8080".parse().unwrap());
  }

  /// A resolver that counts how many times it was called.
  #[derive(Debug)]
  struct CountingResolver {
    addr: SocketAddr,
    call_count: Arc<std::sync::atomic::AtomicUsize>,
  }

  impl Resolve for CountingResolver {
    fn resolve(&self, _name: Name) -> Resolving {
      self
        .call_count
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
      let addr = self.addr;
      Box::pin(async move { Ok(vec![addr].into_iter()) })
    }
  }

  #[tokio::test]
  async fn cached_resolver_deduplicates_lookups() {
    let call_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let inner = Resolver::Custom(Arc::new(CountingResolver {
      addr: "1.2.3.4:0".parse().unwrap(),
      call_count: call_count.clone(),
    }));

    let mut resolver = Resolver::Custom(Arc::new(CachingResolver {
      inner,
      cache: Default::default(),
      ttl: Duration::from_secs(30),
    }));

    // First lookup should call the inner resolver.
    let mut addrs = resolver
      .call(Name::from_str("example.com").unwrap())
      .await
      .unwrap();
    assert_eq!(addrs.next().unwrap(), "1.2.3.4:0".parse().unwrap());
    assert_eq!(call_count.load(std::sync::atomic::Ordering::SeqCst), 1);

    // Second lookup for the same name should be served from cache.
    let mut addrs = resolver
      .call(Name::from_str("example.com").unwrap())
      .await
      .unwrap();
    assert_eq!(addrs.next().unwrap(), "1.2.3.4:0".parse().unwrap());
    assert_eq!(call_count.load(std::sync::atomic::Ordering::SeqCst), 1);

    // A different name should trigger another call.
    let mut addrs = resolver
      .call(Name::from_str("other.com").unwrap())
      .await
      .unwrap();
    assert_eq!(addrs.next().unwrap(), "1.2.3.4:0".parse().unwrap());
    assert_eq!(call_count.load(std::sync::atomic::Ordering::SeqCst), 2);
  }

  #[tokio::test]
  async fn cached_resolver_respects_ttl() {
    let call_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let inner = Resolver::Custom(Arc::new(CountingResolver {
      addr: "1.2.3.4:0".parse().unwrap(),
      call_count: call_count.clone(),
    }));

    let mut resolver = Resolver::Custom(Arc::new(CachingResolver {
      inner,
      cache: Default::default(),
      ttl: Duration::from_millis(1),
    }));

    // First lookup.
    resolver
      .call(Name::from_str("example.com").unwrap())
      .await
      .unwrap();
    assert_eq!(call_count.load(std::sync::atomic::Ordering::SeqCst), 1);

    // Wait for the TTL to expire.
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Should trigger a new lookup after TTL expiry.
    resolver
      .call(Name::from_str("example.com").unwrap())
      .await
      .unwrap();
    assert_eq!(call_count.load(std::sync::atomic::Ordering::SeqCst), 2);
  }

  #[tokio::test]
  async fn cached_resolver_deduplicates_concurrent_lookups() {
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    let call_count = Arc::new(AtomicUsize::new(0));
    let inner = Resolver::Custom(Arc::new(CountingResolver {
      addr: "1.2.3.4:0".parse().unwrap(),
      call_count: call_count.clone(),
    }));

    let resolver = Arc::new(CachingResolver {
      inner,
      cache: Default::default(),
      ttl: Duration::from_secs(30),
    });

    // Spawn many concurrent lookups for the same hostname.
    let mut handles = Vec::new();
    for _ in 0..50 {
      let r = resolver.clone();
      handles.push(tokio::spawn(async move {
        r.resolve(Name::from_str("example.com").unwrap()).await
      }));
    }

    for handle in handles {
      let mut addrs = handle.await.unwrap().unwrap();
      assert_eq!(addrs.next().unwrap(), "1.2.3.4:0".parse().unwrap());
    }

    // Only one actual DNS lookup should have been made.
    assert_eq!(call_count.load(Ordering::SeqCst), 1);
  }
}
