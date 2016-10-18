extern crate core;

use self::core::cell::UnsafeCell;
use self::core::ops::{Deref, DerefMut};
use self::core::sync::atomic::Ordering::{Acquire, Release};
use self::core::sync::atomic::AtomicBool;

pub struct Lock<T> {
    locked: AtomicBool,
    data: UnsafeCell<T>,
}

pub struct TryLock<'a, T: 'a> {
    __ptr: &'a Lock<T>,
}

unsafe impl<T: Send> Send for Lock<T> {}
unsafe impl<T: Send> Sync for Lock<T> {}

impl<T> Lock<T> {
    pub fn new(t: T) -> Lock<T> {
        Lock {
            locked: AtomicBool::new(false),
            data: UnsafeCell::new(t),
        }
    }

    pub fn try_lock(&self) -> Option<TryLock<T>> {
        if !self.locked.swap(true, Acquire) {
            Some(TryLock { __ptr: self })
        } else {
            None
        }
    }
}

impl<'a, T> Deref for TryLock<'a, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // The existence of `TryLock` represents that we own the lock, so we
        // can safely access the data here.
        unsafe { &*self.__ptr.data.get() }
    }
}

impl<'a, T> DerefMut for TryLock<'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        // The existence of `TryLock` represents that we own the lock, so we
        // can safely access the data here.
        //
        // Additionally, we're the *only* `TryLock` in existence so mutable
        // access should be ok.
        unsafe { &mut *self.__ptr.data.get() }
    }
}

impl<'a, T> Drop for TryLock<'a, T> {
    fn drop(&mut self) {
        self.__ptr.locked.store(false, Release);
    }
}

#[cfg(test)]
mod tests {
    use super::Lock;

    #[test]
    fn smoke() {
        let a = Lock::new(1);
        let mut a1 = a.try_lock().unwrap();
        assert!(a.try_lock().is_none());
        assert_eq!(*a1, 1);
        *a1 = 2;
        drop(a1);
        assert_eq!(*a.try_lock().unwrap(), 2);
        assert_eq!(*a.try_lock().unwrap(), 2);
    }
}
