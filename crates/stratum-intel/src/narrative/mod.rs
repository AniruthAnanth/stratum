//! Markdown / narrative sections — spec §24, design 07 §9.
//!
//! **100 % deterministic. No AI involvement in detection, parsing or
//! rendering.** [`detect`] finds the regions; [`render`] holds the security
//! boundary every rendered event has to pass. Document View is a decoration
//! layer over byte ranges — the view never rewrites the buffer, which is what
//! makes spec §6's "no source pollution" hold structurally rather than by
//! discipline.

pub mod detect;
pub mod render;

pub use detect::{detect, detect_in, NarrativeForm, NarrativeRegion};
pub use render::{classify_image, classify_link, escape_html, LinkVerdict};
