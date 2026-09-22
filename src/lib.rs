//! Core library for the Supaplex clone.
//!
//! Platform-independent parsing and simulation live in the library so they can
//! be tested without opening a window. The binary uses SDL2 on desktops and a
//! direct Linux framebuffer/input/audio backend on the original PocketGo.

pub mod actors;
pub mod assets;
pub mod audio;
pub mod cli;
pub mod demo;
pub mod frontend;
pub mod game;
pub mod level;
mod murphy_animation;
mod opl;
pub mod platform;
pub mod profiles;
pub mod render;
