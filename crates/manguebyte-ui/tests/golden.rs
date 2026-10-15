//! Golden images and dumps of a small panel: three knobs, a slider, a
//! switch, two meters, a title and a canvas, at 1x and 2x, and in the
//! light theme.
//!
//! Each dump must match exactly; images may differ by up to [`TOLERANCE`]
//! in any channel, which `manguebyte-gfx` allows for the rasteriser's
//! rounding on other processors. To regenerate the files after an
//! intended change, run locally:
//!
//! ```text
//! MANGUEBYTE_BLESS=1 cargo test -p manguebyte-ui --test golden
//! ```
//!
//! and look at the new images before committing them. With `CI` set,
//! blessing is refused.

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use manguebyte_gfx::snapshot::Snapshots;
use manguebyte_ui::testing::Harness;
use manguebyte_ui::{Action, Role, Theme};

use common::{PANEL, levels, panel_view, params};

/// The same tolerance as `manguebyte-gfx`'s goldens: 0 was measured on
/// x86-64 with and without SIMD; 1 allows for aarch64's rounding.
const TOLERANCE: u8 = 1;

fn snapshots() -> Snapshots {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    let failures = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("ui-golden-failures");
    Snapshots::new(dir, failures).with_tolerance(TOLERANCE)
}

/// The panel with levels on its meters, a switch turned on, a knob moved
/// and the focus on another knob, drawn in several frames.
fn panel(scale: f32, theme: Theme) -> Harness {
    let params = params();
    let levels = levels();
    let mut ui = Harness::with_params(
        panel_view(&params, &levels),
        PANEL,
        scale,
        Arc::clone(&params) as Arc<dyn manguebyte_params::ParamSet>,
    );
    ui.set_theme(theme);
    ui.frame();
    levels.left.store(-12.0, Ordering::Relaxed);
    levels.right.store(-3.5, Ordering::Relaxed);
    ui.advance_ms(16);
    let bypass = ui.get(Role::Switch, "Bypass");
    ui.act(&bypass, Action::Press);
    let cutoff = ui.get(Role::Slider, "Cutoff");
    ui.drag(cutoff.center(), (0.0, -30.0), 3);
    ui.advance_ms(16);
    let drive = ui.get(Role::Slider, "Drive");
    ui.act(&drive, Action::Focus);
    ui.advance_ms(16);
    ui
}

#[test]
fn the_panel_matches_its_goldens_at_1x_and_2x() {
    let snapshots = snapshots();
    for (name, scale, theme) in [
        ("panel@1x", 1.0, Theme::dark()),
        ("panel@2x", 2.0, Theme::dark()),
        ("panel-light@1x", 1.0, Theme::light()),
    ] {
        let ui = panel(scale, theme);
        // Drawn in several partial frames, the buffer is what one whole
        // render of the final scene gives.
        assert!(
            ui.pixels() == &ui.render_fresh(),
            "{name}: partial and whole renders differ"
        );
        if let Err(mismatch) = snapshots.check(name, ui.pixels(), &ui.dump()) {
            panic!("{mismatch}");
        }
    }
}

#[test]
fn the_panel_shows_what_its_goldens_were_made_from() {
    let ui = panel(1.0, Theme::dark());
    assert_eq!(ui.get(Role::Meter, "Left").value_text(), Some("-12.0 dB"));
    assert_eq!(ui.get(Role::Meter, "Right").value_text(), Some("-3.5 dB"));
    assert_eq!(
        ui.get(Role::Switch, "Bypass").node.state.checked,
        Some(true)
    );
    assert!(ui.get(Role::Slider, "Drive").is_focused());
    // 30 pixels up from 1000 Hz is 0.15 of a logarithmic range, which
    // multiplies the frequency by 1000 to the power of 0.15.
    assert_eq!(ui.get(Role::Slider, "Cutoff").value_text(), Some("2818 Hz"));
}
