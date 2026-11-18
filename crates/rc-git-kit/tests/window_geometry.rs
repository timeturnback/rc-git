//! The window frame across coordinate systems, and the sidebar's saved split.
#![expect(
    clippy::float_cmp,
    reason = "every value is a whole number of points, exact in f64"
)]

use rc_git_kit::layout_policy::{SIDEBAR_MAXIMUM_WIDTH, SIDEBAR_MINIMUM_WIDTH};
use rc_git_kit::platform::stores::{Rect, SubviewFrame, WindowFrame};
use rc_git_kit::window_geometry::{
    TopLeftRect, from_appkit, restorable, saved_sidebar_width, sidebar_frames, to_appkit,
};

const PRIMARY: f64 = 1000.0;

fn screen() -> TopLeftRect {
    TopLeftRect {
        x: 0.0,
        y: 0.0,
        width: 1600.0,
        height: PRIMARY,
    }
}

fn saved(x: f64, y: f64, width: f64, height: f64) -> WindowFrame {
    WindowFrame {
        frame: Rect {
            x,
            y,
            width,
            height,
        },
        screen: Rect {
            x: 0.0,
            y: 0.0,
            width: 1600.0,
            height: PRIMARY,
        },
    }
}

#[test]
fn a_frame_round_trips_between_the_two_origins() {
    let top_left = TopLeftRect {
        x: 100.0,
        y: 50.0,
        width: 800.0,
        height: 600.0,
    };
    let appkit = to_appkit(top_left, PRIMARY);
    assert_eq!(appkit.y, 350.0);
    assert_eq!(from_appkit(appkit, PRIMARY), top_left);
}

#[test]
fn a_frame_on_a_screen_restores() {
    let restored = restorable(&saved(100.0, 350.0, 800.0, 600.0), PRIMARY, &[screen()]);
    assert_eq!(restored.map(|rect| rect.y), Some(50.0));
}

#[test]
fn a_frame_off_every_screen_or_below_the_minimum_does_not() {
    assert_eq!(
        restorable(&saved(5000.0, 350.0, 800.0, 600.0), PRIMARY, &[screen()]),
        None
    );
    assert_eq!(
        restorable(&saved(100.0, 350.0, 439.0, 600.0), PRIMARY, &[screen()]),
        None
    );
    assert_eq!(
        restorable(&saved(100.0, 350.0, 800.0, 319.0), PRIMARY, &[screen()]),
        None
    );
    assert!(restorable(&saved(100.0, 350.0, 440.0, 320.0), PRIMARY, &[screen()]).is_some());
    assert_eq!(
        restorable(&saved(100.0, 350.0, f64::NAN, 600.0), PRIMARY, &[screen()]),
        None
    );
}

#[test]
fn the_saved_sidebar_width_is_clamped_and_survives_a_collapse() {
    let frames = sidebar_frames(300.0, true, 1000.0, 700.0);
    assert_eq!(saved_sidebar_width(&frames), Some(300.0));
    assert_eq!(frames[1].frame.width, 1000.0);
    let wide = sidebar_frames(9000.0, false, 1000.0, 700.0);
    assert_eq!(saved_sidebar_width(&wide), Some(SIDEBAR_MAXIMUM_WIDTH));
    let narrow = sidebar_frames(10.0, false, 1000.0, 700.0);
    assert_eq!(saved_sidebar_width(&narrow), Some(SIDEBAR_MINIMUM_WIDTH));
    assert_eq!(saved_sidebar_width(&[]), None);
    let poisoned = [SubviewFrame {
        frame: Rect {
            x: 0.0,
            y: 0.0,
            width: f64::INFINITY,
            height: 1.0,
        },
        hidden: false,
        flag: false,
    }];
    assert_eq!(saved_sidebar_width(&poisoned), None);
}
