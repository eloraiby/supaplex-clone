//! Conditional front-end API shared by desktop SDL2 and original PocketGo.

#[cfg(not(any(feature = "pocketgo", target_env = "uclibc")))]
pub use sdl2::*;

#[cfg(any(feature = "pocketgo", target_env = "uclibc"))]
mod pocketgo;
#[cfg(any(feature = "pocketgo", target_env = "uclibc"))]
pub use pocketgo::*;
