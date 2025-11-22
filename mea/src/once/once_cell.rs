// Copyright 2024 tison <wander4096@gmail.com>
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::cell::UnsafeCell;
use std::future::Future;
use std::mem::MaybeUninit;
use std::ptr;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

#[cfg(test)]
mod tests;

use crate::internal;

/// A cell which can be written to only once.
pub struct OnceCell<T> {
    set: AtomicBool,
    sem: internal::Semaphore,
    value: UnsafeCell<MaybeUninit<T>>,
}

// Access to OnceCell's value is guarded by the semaphore permit and the
// `set` flag. We can move values across threads via `set`, so T must be Send.
// Once initialized we hand out shared references, so T also needs Sync.
unsafe impl<T: Send> Send for OnceCell<T> {}
unsafe impl<T: Send + Sync> Sync for OnceCell<T> {}

impl<T> OnceCell<T> {
    /// Creates a new empty `OnceCell` instance.
    pub fn new() -> Self {
        OnceCell {
            set: AtomicBool::new(false),
            sem: internal::Semaphore::new(1),
            value: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    /// Returns `true` if the `OnceCell` currently contains a value, and `false`
    /// otherwise.
    pub fn initialized(&self) -> bool {
        // Using acquire ordering so any threads that read a true from this
        // atomic is able to read the value.
        self.set.load(Ordering::Acquire)
    }

    /// Returns a reference to the value currently stored in the `OnceCell`, or
    /// `None` if the `OnceCell` is empty.
    pub fn get(&self) -> Option<&T> {
        if self.initialized() {
            Some(unsafe { self.get_unchecked() })
        } else {
            None
        }
    }

    ///
    pub async fn get_or_init<F, Fut>(&self, f: F) -> &T
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        if self.initialized() {
            // SAFETY: The OnceCell has been fully initialized.
            unsafe { self.get_unchecked() }
        } else {
            // Acquire a permit to initialize the cell.
            self.sem.acquire(1).await;
            let guard = Guard {
                sem: &self.sem,
                permits: 1,
            };

            if self.initialized() {
                // Another task initialized the cell while we were waiting for
                // the permit.
                // SAFETY: The OnceCell has been fully initialized.
                return unsafe { self.get_unchecked() };
            }

            // We are now the only task that can initialize the cell.
            let value = f().await;
            let reference = self.set_value(value);
            drop(guard);
            reference
        }
    }

    ///
    pub async fn get_or_try_init<E, F, Fut>(&self, f: F) -> Result<&T, E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, E>>,
    {
        if self.initialized() {
            // SAFETY: The OnceCell has been fully initialized.
            unsafe { Ok(self.get_unchecked()) }
        } else {
            // Acquire a permit to initialize the cell.
            self.sem.acquire(1).await;
            let guard = Guard {
                sem: &self.sem,
                permits: 1,
            };

            if self.initialized() {
                // Another task initialized the cell while we were waiting for
                // the permit.
                // SAFETY: The OnceCell has been fully initialized.
                return unsafe { Ok(self.get_unchecked()) };
            }

            // We are now the only task that can initialize the cell.
            let value = f().await?;
            let reference = self.set_value(value);
            drop(guard);
            Ok(reference)
        }
    }

    // SAFETY: The OnceCell must not be empty.
    unsafe fn get_unchecked(&self) -> &T {
        let ptr = self.value.get();
        &*(*ptr).as_ptr()
    }

    fn set_value(&self, value: T) -> &T {
        // SAFETY: We are holding the only permit on the semaphore.
        unsafe {
            let ptr = self.value.get();
            (*ptr).as_mut_ptr().write(value);
        }

        // Using release ordering so any threads that read a true from this
        // atomic is able to read the value we just stored.
        self.set.store(true, Ordering::Release);

        // SAFETY: We just initialized the cell.
        unsafe { self.get_unchecked() }
    }

    fn initialized_mut(&mut self) -> bool {
        *self.set.get_mut()
    }
}

impl<T> Drop for OnceCell<T> {
    fn drop(&mut self) {
        if self.initialized_mut() {
            unsafe {
                let ptr = self.value.get();
                ptr::drop_in_place((*ptr).as_mut_ptr());
            }
        }
    }
}

struct Guard<'a> {
    sem: &'a internal::Semaphore,
    permits: usize,
}

impl<'a> Drop for Guard<'a> {
    fn drop(&mut self) {
        if self.permits != 0 {
            self.sem.release(self.permits);
        }
    }
}
