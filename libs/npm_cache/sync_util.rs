// Copyright 2018-2026 the Deno authors. MIT license.

use parking_lot::Condvar;
use parking_lot::Mutex;

/// A blocking counting semaphore.
pub struct BlockingSemaphore {
  state: Mutex<usize>,
  cond: Condvar,
}

impl std::fmt::Debug for BlockingSemaphore {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("BlockingSemaphore")
      .field("permits", &*self.state.lock())
      .finish()
  }
}

impl BlockingSemaphore {
  pub fn new(permits: usize) -> Self {
    Self {
      state: Mutex::new(permits),
      cond: Condvar::new(),
    }
  }

  pub fn acquire(&self) -> BlockingSemaphorePermit<'_> {
    let mut state = self.state.lock();
    while *state == 0 {
      self.cond.wait(&mut state);
    }
    *state -= 1;
    BlockingSemaphorePermit { semaphore: self }
  }
}

pub struct BlockingSemaphorePermit<'a> {
  semaphore: &'a BlockingSemaphore,
}

impl Drop for BlockingSemaphorePermit<'_> {
  fn drop(&mut self) {
    *self.semaphore.state.lock() += 1;
    self.semaphore.cond.notify_one();
  }
}
