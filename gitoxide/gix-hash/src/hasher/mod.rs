use sha1_checked::CollisionResult;

/// A hash-digest produced by a [`Hasher`] hash implementation.
pub type Digest = [u8; 20];

/// The error returned by [`Hasher::try_finalize()`].
#[derive(Debug, thiserror::Error)]
#[allow(missing_docs)]
pub enum Error {
    #[error("Detected SHA-1 collision attack with digest {digest}")]
    CollisionAttack { digest: crate::ObjectId },
}

/// A implementation of the Sha1 hash, which can be used once.
///
/// We use [`sha1_checked`] to implement the same collision detection
/// algorithm as Git.
#[derive(Clone)]
pub struct Hasher(sha1_checked::Sha1);

impl Default for Hasher {
    #[inline]
    fn default() -> Self {
        // This matches the configuration used by Git, which only uses
        // the collision detection to bail out, rather than computing
        // alternate “safe hashes” for inputs where a collision attack
        // was detected.
        Self(sha1_checked::Builder::default().safe_hash(false).build())
    }
}

impl Hasher {
    /// Digest the given `bytes`.
    pub fn update(&mut self, bytes: &[u8]) {
        use sha1_checked::Digest;
        self.0.update(bytes);
    }

    /// Finalize the hash and produce an object ID.
    ///
    /// Returns [`Error`] if a collision attack is detected.
    #[inline]
    pub fn try_finalize(self) -> Result<crate::ObjectId, Error> {
        match self.0.try_finalize() {
            CollisionResult::Ok(digest) => Ok(crate::ObjectId::Sha1(digest.into())),
            CollisionResult::Mitigated(_) => {
                // SAFETY: `CollisionResult::Mitigated` is only
                // returned when `safe_hash()` is on. `Hasher`’s field
                // is private, and we only construct it in the
                // `Default` instance, which turns `safe_hash()` off.
                //
                // As of Rust 1.84.1, the compiler can’t figure out
                // this function cannot panic without this.
                #[allow(unsafe_code)]
                unsafe {
                    std::hint::unreachable_unchecked()
                }
            }
            CollisionResult::Collision(digest) => Err(Error::CollisionAttack {
                digest: crate::ObjectId::Sha1(digest.into()),
            }),
        }
    }

    /// Finalize the hash and produce an object ID.
    #[inline]
    pub fn finalize(self) -> crate::ObjectId {
        self.try_finalize().expect("Detected SHA-1 collision attack")
    }

    /// Finalize the hash and produce a digest.
    #[inline]
    pub fn digest(self) -> Digest {
        self.finalize()
            .as_slice()
            .try_into()
            .expect("SHA-1 object ID to be 20 bytes long")
    }
}

/// Produce a hasher suitable for the given kind of hash.
#[inline]
pub fn hasher(kind: crate::Kind) -> Hasher {
    match kind {
        crate::Kind::Sha1 => Hasher::default(),
    }
}

/// Hashing utilities for I/O operations.
pub mod io;
