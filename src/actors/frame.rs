//! Bounded frame values shared by actor-specific state machines.
//!
//! A frame carries its strip length in its type. Construction validates external
//! numbers once; advancement cannot produce an out-of-range successor.

/// A zero-based frame in a strip containing exactly `N` pictures.
///
/// The private representation prevents bypassing the checked constructor.
/// ```compile_fail
/// use supaplex_clone::actors::Frame;
/// let invalid = Frame::<8>(255);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Frame<const N: u8>(u8);

impl<const N: u8> Frame<N> {
    /// Validates a frame supplied by a level loader or other external source.
    pub const fn new(value: u8) -> Option<Self> {
        match value < N {
            true => Some(Self(value)),
            false => None,
        }
    }

    /// Returns the first frame; empty strips are a programming error.
    pub const fn first() -> Self {
        assert!(N > 0, "an animation strip must contain a frame");
        Self(0)
    }

    /// Returns the last picture without allowing an empty strip.
    pub const fn last() -> Self {
        assert!(N > 0, "an animation strip must contain a frame");
        Self(N - 1)
    }

    /// Returns the validated index for sprite lookup and original timing rules.
    pub const fn index(self) -> u8 {
        self.0
    }

    /// Advances within the strip, reporting completion instead of wrapping.
    pub const fn next(self) -> Option<Self> {
        // Compare before incrementing so even a 255-picture strip stays safe.
        match self.0 < N - 1 {
            true => Some(Self(self.0 + 1)),
            false => None,
        }
    }

    /// Advances a cyclic strip, returning to its first picture after the last.
    pub const fn wrapping_next(self) -> Self {
        match self.next() {
            Some(next) => next,
            None => Self::first(),
        }
    }
}

#[cfg(test)]
mod tests {
    //! Boundary checks for frame construction and finite/cyclic advancement.

    use super::Frame;

    /// Exhausts every byte input and verifies the eight-frame strip boundary.
    #[test]
    fn movement_frames_cannot_escape_their_strip() {
        for index in 0..=u8::MAX {
            let frame = Frame::<8>::new(index);
            assert_eq!(frame.is_some(), index < 8);
            if let Some(frame) = frame {
                assert_eq!(frame.index(), index);
                assert_eq!(
                    frame.next().map(Frame::index),
                    (index < 7).then_some(index + 1)
                );
            }
        }
        assert_eq!(Frame::<8>::last().wrapping_next(), Frame::first());
    }

    /// Covers degenerate and maximal strip lengths without integer overflow.
    #[test]
    fn boundary_lengths_have_explicit_completion() {
        assert!(Frame::<0>::new(0).is_none());
        assert!(Frame::<1>::first().next().is_none());
        assert!(Frame::<255>::last().next().is_none());
    }
}
