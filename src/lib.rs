//! cowbell: a mouse-driven DAW for drill 'n' bass.

#[cfg(feature = "live")]
pub mod audio;
pub mod bounce;
pub mod demo;
pub mod document;
pub mod engine;
pub mod export;
pub mod graph;
pub mod project;
pub mod rack;
pub mod sample;
pub mod selection;
pub mod song;
pub mod synth;
pub mod synthvoice;
pub mod transform;
pub mod tuning;
#[cfg(feature = "ui")]
pub mod ui;
pub mod voice;
