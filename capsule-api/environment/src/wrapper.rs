use std::fmt;
use std::ops::{Deref, DerefMut};

/// Wrapper struct hiding inner value from Debug output
pub struct SecretKeyWrapper<T>(pub T);

impl<T> fmt::Debug for SecretKeyWrapper<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("<SecretKey>")
    }
}

impl<T> Deref for SecretKeyWrapper<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> DerefMut for SecretKeyWrapper<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<T: Clone> Clone for SecretKeyWrapper<T> {
    fn clone(&self) -> Self {
        SecretKeyWrapper(self.0.clone())
    }
}

impl<T> From<T> for SecretKeyWrapper<T> {
    fn from(value: T) -> Self {
        SecretKeyWrapper(value)
    }
}
