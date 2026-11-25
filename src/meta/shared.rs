//! Compact, shareable field types for `Module`. The versions of one identifier often have
//! the same name, abstract, resources, relationships and install stanzas: the parser lets
//! them point to one copy (`Module::share_with`). Both types read like `&str` / `&[T]`.

use std::borrow::Borrow;
use std::fmt;
use std::ops::Deref;
use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// An immutable string that clones by reference count. `""` needs no allocation.
#[derive(Clone, Default)]
pub struct IStr(Option<Arc<str>>);

impl IStr {
    pub fn as_str(&self) -> &str {
        self.0.as_deref().unwrap_or("")
    }

    /// Point to `other` when the text is the same (keeps one copy in memory).
    pub fn share(&mut self, other: &IStr) {
        if self == other {
            self.clone_from(other);
        }
    }
}

impl Deref for IStr {
    type Target = str;
    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<str> for IStr {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Borrow<str> for IStr {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl From<&str> for IStr {
    fn from(s: &str) -> Self {
        IStr((!s.is_empty()).then(|| Arc::from(s)))
    }
}

impl From<String> for IStr {
    fn from(s: String) -> Self {
        IStr::from(s.as_str())
    }
}

impl From<&String> for IStr {
    fn from(s: &String) -> Self {
        IStr::from(s.as_str())
    }
}

impl From<IStr> for String {
    fn from(s: IStr) -> String {
        s.as_str().to_string()
    }
}

impl PartialEq for IStr {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}
impl Eq for IStr {}

impl PartialEq<str> for IStr {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}
impl PartialEq<&str> for IStr {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}
impl PartialEq<String> for IStr {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other
    }
}
impl PartialEq<IStr> for str {
    fn eq(&self, other: &IStr) -> bool {
        self == other.as_str()
    }
}
impl PartialEq<IStr> for &str {
    fn eq(&self, other: &IStr) -> bool {
        *self == other.as_str()
    }
}
impl PartialEq<IStr> for String {
    fn eq(&self, other: &IStr) -> bool {
        self == other.as_str()
    }
}

impl PartialOrd for IStr {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for IStr {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.as_str().cmp(other.as_str())
    }
}
impl std::hash::Hash for IStr {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        self.as_str().hash(h)
    }
}

impl fmt::Display for IStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for IStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl Serialize for IStr {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for IStr {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl serde::de::Visitor<'_> for V {
            type Value = IStr;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a string")
            }
            fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<IStr, E> {
                Ok(IStr::from(s))
            }
        }
        d.deserialize_str(V)
    }
}

/// An immutable list that clones by reference count. An empty list needs no allocation.
pub struct List<T>(Option<Arc<[T]>>);

impl<T> List<T> {
    pub fn new() -> Self {
        List(None)
    }

    pub fn as_slice(&self) -> &[T] {
        self.0.as_deref().unwrap_or(&[])
    }

    /// Same as `<[T]>::is_empty`; a path for serde's `skip_serializing_if`.
    pub fn is_empty(&self) -> bool {
        self.as_slice().is_empty()
    }
}

impl<T: PartialEq> List<T> {
    /// Point to `other` when the items are the same (keeps one copy in memory).
    pub fn share(&mut self, other: &List<T>) {
        if !self.is_empty() && self.as_slice() == other.as_slice() {
            self.0.clone_from(&other.0);
        }
    }
}

impl<T> Default for List<T> {
    fn default() -> Self {
        List(None)
    }
}

impl<T> Clone for List<T> {
    fn clone(&self) -> Self {
        List(self.0.clone())
    }
}

impl<T> Deref for List<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        self.as_slice()
    }
}

impl<T> AsRef<[T]> for List<T> {
    fn as_ref(&self) -> &[T] {
        self.as_slice()
    }
}

impl<'a, T> IntoIterator for &'a List<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.as_slice().iter()
    }
}

impl<T> From<Vec<T>> for List<T> {
    fn from(v: Vec<T>) -> Self {
        // Arc<[T]>::from copies into an exact-size allocation (no spare capacity).
        List((!v.is_empty()).then(|| Arc::from(v)))
    }
}

impl<T: Clone> From<&[T]> for List<T> {
    fn from(v: &[T]) -> Self {
        List((!v.is_empty()).then(|| Arc::from(v)))
    }
}

impl<T> FromIterator<T> for List<T> {
    fn from_iter<I: IntoIterator<Item = T>>(it: I) -> Self {
        List::from(it.into_iter().collect::<Vec<T>>())
    }
}

impl<T: PartialEq> PartialEq for List<T> {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl<T: PartialEq, U> PartialEq<Vec<U>> for List<T>
where
    T: PartialEq<U>,
{
    fn eq(&self, other: &Vec<U>) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl<T: PartialEq<U>, U, const N: usize> PartialEq<[U; N]> for List<T> {
    fn eq(&self, other: &[U; N]) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl<T: fmt::Debug> fmt::Debug for List<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_slice(), f)
    }
}

impl<T: Serialize> Serialize for List<T> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.as_slice().serialize(s)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for List<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Vec::<T>::deserialize(d).map(List::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn istr_and_list() {
        let a = IStr::from("abc");
        let mut b = IStr::from(String::from("abc"));
        let owned = String::from("abc");
        assert!(a == "abc" && "abc" == a && a == owned);
        b.share(&a);
        assert!(matches!((&a.0, &b.0), (Some(x), Some(y)) if Arc::ptr_eq(x, y)));
        assert!(IStr::default().0.is_none() && IStr::from("").0.is_none());
        assert_eq!(serde_json::to_string(&a).unwrap(), "\"abc\"");
        let l: List<String> = vec!["x".to_string()].into();
        let mut m: List<String> = ["x".to_string()].into_iter().collect();
        m.share(&l);
        assert!(matches!((&l.0, &m.0), (Some(x), Some(y)) if Arc::ptr_eq(x, y)));
        assert!(List::<String>::from(Vec::new()).0.is_none());
        assert_eq!(l, vec!["x"]);
        assert_eq!(serde_json::to_string(&l).unwrap(), "[\"x\"]");
        assert_eq!(serde_json::from_str::<List<u8>>("[1,2]").unwrap(), [1, 2]);
    }
}
