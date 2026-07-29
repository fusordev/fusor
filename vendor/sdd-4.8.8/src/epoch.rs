/// [`Epoch`] represents a unit of time that governs the lifetime of retired memory regions.
///
/// The global epoch cycles through `64` [`Epoch`] values in the range `[0..63]` instead of
/// monotonically increasing, which reduces the memory footprint of [`Epoch`] values.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct Epoch {
    value: u8,
}

impl Epoch {
    /// The number of epoch values the global epoch cycles through.
    pub(super) const NUM_EPOCHS: u8 = 64;

    /// Returns a future [`Epoch`] by which the current readers will no longer be active.
    ///
    /// Since the current [`Epoch`] may lag behind the global epoch value by `1`, this method
    /// returns an [`Epoch`] three epochs ahead of `self`.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::Epoch;
    ///
    /// let initial = Epoch::default();
    ///
    /// let next_generation = initial.next_generation();
    /// assert_eq!(next_generation, initial.next().next().next());
    /// ```
    #[inline]
    #[must_use]
    pub const fn next_generation(self) -> Epoch {
        self.next().next().next()
    }

    /// Checks if the given [`Epoch`] is in the same generation as the current [`Epoch`].
    ///
    /// This operation is not commutative; for example, `a.in_same_generation(b)` may return a
    /// different result than `b.in_same_generation(a)`. This method returns `true` if the given
    /// [`Epoch`] is the same as, the next after, or two epochs after the current one. When this
    /// method returns `false`, it means that a memory region retired in the current [`Epoch`] will
    /// no longer be reachable in the given [`Epoch`].
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::Epoch;
    ///
    /// let initial = Epoch::default();
    ///
    /// let next_generation = initial.next_generation();
    /// assert!(initial.in_same_generation(initial.next().next()));
    /// assert!(!initial.in_same_generation(initial.next().next().next()));
    /// ```
    #[inline]
    #[must_use]
    pub const fn in_same_generation(self, other: Epoch) -> bool {
        other.value == self.value
            || other.value == self.next().value
            || other.value == self.next().next().value
    }

    /// Returns the next [`Epoch`] value.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::Epoch;
    ///
    /// let initial = Epoch::default();
    ///
    /// let next = initial.next();
    /// assert!(initial < next);
    ///
    /// let next_prev = next.prev();
    /// assert_eq!(initial, next_prev);
    /// ```
    #[inline]
    #[must_use]
    pub const fn next(self) -> Epoch {
        Epoch {
            value: (self.value + 1) % Self::NUM_EPOCHS,
        }
    }

    /// Returns the previous [`Epoch`] value, wrapping around if necessary.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::Epoch;
    ///
    /// let initial = Epoch::default();
    ///
    /// let prev = initial.prev();
    /// assert!(initial < prev);
    ///
    /// let prev_next = prev.next();
    /// assert_eq!(initial, prev_next);
    /// ```
    #[inline]
    #[must_use]
    pub const fn prev(self) -> Epoch {
        Epoch {
            value: (self.value + Self::NUM_EPOCHS - 1) % Self::NUM_EPOCHS,
        }
    }

    /// Constructs an [`Epoch`] from a [`u8`] value.
    #[inline]
    pub(super) const fn from_u8(value: u8) -> Epoch {
        Epoch { value }
    }

    /// Converts an [`Epoch`] into a [`u8`] value.
    #[inline]
    pub(super) const fn into_u8(epoch: Epoch) -> u8 {
        epoch.value
    }
}

impl TryFrom<u8> for Epoch {
    type Error = Epoch;

    #[inline]
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value < Self::NUM_EPOCHS {
            Ok(Epoch { value })
        } else {
            Err(Epoch {
                value: value % Self::NUM_EPOCHS,
            })
        }
    }
}

impl From<Epoch> for u8 {
    #[inline]
    fn from(epoch: Epoch) -> Self {
        Epoch::into_u8(epoch)
    }
}
