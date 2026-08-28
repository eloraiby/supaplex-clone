//! Core library for the SDL2 Supaplex clone.
//!
//! Platform-independent parsing and simulation live in the library so they can
//! be tested without opening a window. The binary is a thin SDL2 adapter.

pub mod actor;
pub mod assets;
pub mod audio;
pub mod cli;
pub mod dat_graphics;
pub mod demo;
pub mod frontend;
pub mod game;
pub mod level;
mod murphy_animation;
pub mod profiles;
pub mod render;
mod xm;
