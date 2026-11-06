//! cowbell: a keyboard-driven tracker for drill 'n' bass.

#[cfg(feature = "live")]
pub mod audio;
pub mod bounce;
pub mod demo;
pub mod editor;
pub mod engine;
pub mod export;
pub mod graph;
pub mod pattern;
pub mod project;
pub mod rack;
pub mod sample;
pub mod synth;
pub mod synthvoice;
pub mod transform;
pub mod tuning;
pub mod tutorial;
#[cfg(feature = "ui")]
pub mod ui;
pub mod variations;
pub mod voice;
