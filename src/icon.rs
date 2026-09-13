//! The tray icon, drawn in code so the repo carries no binary asset.
//!
//! Three states, distinguished by *shape* as well as colour: a 32x32 icon is
//! small, and a difference only in colour is no difference at all to anyone who
//! cannot see it.
//!
//! Plain std Rust with no Windows in it, so the shapes stay testable and could
//! be baked into an `.ico` by a build script later without moving anything.

/// What the icon is saying. With no window and no console, this and the log are
/// the only places the daemon's state is visible.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(u8)]
pub enum Status {
    /// Hotkeys registered, waiting to be pressed.
    #[default]
    Ready = 0,
    /// An action is running. Switching to the TV takes seconds, so this is on
    /// screen long enough to be worth drawing.
    Working = 1,
    /// A hotkey did not register, or the last action failed. The menu says
    /// which, and the log says why.
    Problem = 2,
}

pub const SIZE: usize = 32;

/// BGRA, the order `CreateIcon` wants for a 32-bit colour bitmap.
pub type Pixels = [u8; SIZE * SIZE * 4];

/// Draw one status: a monitor outline with a mark that names the state.
pub fn draw(status: Status) -> Pixels {
    let mut px = [0u8; SIZE * SIZE * 4];

    let (r, g, b) = match status {
        Status::Ready => (0x4C, 0xAF, 0x50),
        Status::Working => (0xFF, 0xB3, 0x00),
        Status::Problem => (0xE5, 0x39, 0x35),
    };

    let mut set = |x: usize, y: usize| {
        let i = (y * SIZE + x) * 4;
        px[i] = b;
        px[i + 1] = g;
        px[i + 2] = r;
        px[i + 3] = 0xFF;
    };

    // A screen: a 24x16 outline two pixels thick, on a stand.
    let (left, right, top, bottom) = (4, 27, 6, 21);
    for x in left..=right {
        for t in 0..2 {
            set(x, top + t);
            set(x, bottom - t);
        }
    }
    for y in top..=bottom {
        for t in 0..2 {
            set(left + t, y);
            set(right - t, y);
        }
    }
    // Stand: a neck and a foot.
    for y in bottom + 1..bottom + 4 {
        for x in 14..=17 {
            set(x, y);
        }
    }
    for x in 10..=21 {
        for y in bottom + 4..bottom + 6 {
            set(x, y);
        }
    }

    // The mark inside the screen, which is what tells the states apart when the
    // icon is 16 pixels across on a scaled display and the colour is a guess.
    match status {
        // Filled: nothing to say, everything on.
        Status::Ready => {
            for y in top + 4..=bottom - 4 {
                for x in left + 4..=right - 4 {
                    set(x, y);
                }
            }
        }
        // A bar, like something in progress.
        Status::Working => {
            for y in 12..=15 {
                for x in left + 4..=right - 4 {
                    set(x, y);
                }
            }
        }
        // A cross.
        Status::Problem => {
            for i in 0..12 {
                for t in 0..3 {
                    set(left + 5 + i + t, top + 3 + i);
                    set(right - 5 - i - t, top + 3 + i);
                }
            }
        }
    }

    px
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opaque_pixels(px: &Pixels) -> usize {
        px.as_chunks::<4>().0.iter().filter(|p| p[3] != 0).count()
    }

    #[test]
    fn every_status_draws_something() {
        for status in [Status::Ready, Status::Working, Status::Problem] {
            let drawn = draw(status);
            let lit = opaque_pixels(&drawn);
            assert!(lit > 100, "{status:?} drew only {lit} pixels");
            assert!(lit < SIZE * SIZE, "{status:?} filled the whole square");
        }
    }

    /// The point of the marks: two states must not be one recolour apart, or
    /// they are indistinguishable to anyone who cannot separate the colours.
    #[test]
    fn the_states_differ_in_shape_not_only_colour() {
        let shape = |s| -> Vec<bool> {
            draw(s)
                .as_chunks::<4>()
                .0
                .iter()
                .map(|p| p[3] != 0)
                .collect()
        };
        let ready = shape(Status::Ready);
        let working = shape(Status::Working);
        let problem = shape(Status::Problem);
        assert_ne!(ready, working);
        assert_ne!(ready, problem);
        assert_ne!(working, problem);
    }

    #[test]
    fn nothing_is_drawn_outside_the_icon() {
        // draw() indexes a fixed-size array, so this is really a check that the
        // shapes stay inside it as they are edited.
        for status in [Status::Ready, Status::Working, Status::Problem] {
            assert_eq!(draw(status).len(), SIZE * SIZE * 4);
        }
    }
}
