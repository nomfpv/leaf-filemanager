//! Önizleme için saf Rust oynatıcı: FilmCraft çözücüleriyle açma ve çözme, ses çıkışı, video çizimi.

mod audio_out;
mod clock;
mod convert;
mod player;
// Direct3D 11: Windows only (another system draws the frames itself, from `to_bgra`)
#[cfg(windows)]
mod present;
mod source;
mod stream;

pub use audio_out::{AudioSink, NullSink};
// WASAPI: Windows only (elsewhere `NullSink` is the sink until a native one exists)
#[cfg(windows)]
pub use audio_out::wasapi_sink;
pub use clock::Clock;
pub use convert::to_bgra;
pub use player::{Player, PlayerState};
#[cfg(windows)]
pub use present::{Presenter, render_offscreen};
pub use source::{Frame, Media, Planes, can_open, open};
pub use stream::{HttpReader, head as stream_head, open_stream};
