//! Inspector content must scroll rather than compress its text or actions.
use super::{install_theme, TreeLensApp, TRASH_SUPPORTED};
use gpui_kit::{
    point, px, size, test::TestWindowExt, AppContext, Bounds, ScrollDelta, TestAppContext,
    WindowBounds, WindowOptions,
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!(
                "tree-lens-inspector-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ))
            .join("long-parent-directory-for-the-full-path-regression");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(name), b"synthetic inspector fixture").unwrap();
        Self(root)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // Remove only the unique directory created by this test.
        let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
    }
}

#[gpui_kit::test]
fn inspector_wraps_and_all_controls_remain_reachable(cx: &mut TestAppContext) {
    for name in [
        format!("{}.txt", "a".repeat(220)),
        format!("{}.txt", "图片下面的完整文件名称".repeat(5)),
    ] {
        let fixture = Fixture::new(&name);
        let mut natural_text_heights = None;
        // Minimum inspector-visible width, a short window, and a wide/tall window.
        for (width, height) in [(1220., 760.), (1440., 600.), (1800., 1000.)] {
            let (handle, _view) = cx.update(|cx| {
                gpui_kit::init(cx);
                install_theme(cx);
                gpui_kit::open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(Bounds {
                            origin: Default::default(),
                            size: size(px(width), px(height)),
                        })),
                        focus: false,
                        show: false,
                        ..Default::default()
                    },
                    cx,
                    |window, cx| {
                        cx.new(|cx| {
                            let mut app = TreeLensApp::new(Some(fixture.0.clone()), window, cx);
                            let deadline = Instant::now() + Duration::from_secs(5);
                            while app.session.busy() && Instant::now() < deadline {
                                app.poll_session(window, cx);
                                std::thread::sleep(Duration::from_millis(1));
                            }
                            assert!(!app.session.busy());
                            let scan = app.session.result.as_ref().unwrap();
                            let id = scan
                                .nodes
                                .iter()
                                .find(|node| node.name.as_ref() == std::ffi::OsStr::new(&name))
                                .unwrap()
                                .id;
                            app.select_node(id, cx);
                            app
                        })
                    },
                )
                .unwrap()
            });
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                let heading = window.find("inspector-heading").bounds();
                let viewport = window.find("inspector-scroll").bounds();
                let caveat = window.find("inspector-caveat").bounds();
                assert!(viewport.size.height > px(0.));
                assert!(heading.bottom() <= viewport.top());
                assert!(viewport.bottom() <= caveat.top());
                assert!(caveat.bottom() < px(height));
                let title = window.find("inspector-name");
                let path = window.find("inspector-path");
                assert_eq!(title.label(), Some(name.as_str()));
                let expected_path = fixture.0.join(&name).display().to_string();
                assert_eq!(path.label(), Some(expected_path.as_str()));
                // Both long ASCII tokens and CJK names must wrap, never ellipsize
                // or collapse into a sub-line-height box.
                let text_heights = (title.bounds().size.height, path.bounds().size.height);
                assert!(text_heights.0 > px(36.));
                assert!(text_heights.1 > px(24.));
                for bounds in [title.bounds(), path.bounds()] {
                    assert!(bounds.left() >= viewport.left() && bounds.right() <= viewport.right());
                }
                if let Some(expected) = natural_text_heights {
                    assert_eq!(
                        text_heights, expected,
                        "window height must not compress text"
                    );
                }
                natural_text_heights = Some(text_heights);
                let mut controls = vec![
                    "copy-selected",
                    "delete-selected",
                    "delete-permanent-selected",
                ];
                if TRASH_SUPPORTED {
                    controls.insert(1, "reveal-selected");
                }
                let mut reached = vec![false; controls.len()];
                let mut path_bottom_reached = false;
                for _ in 0..80 {
                    let mut previous_bottom = window.find("inspector-path").bounds().bottom();
                    path_bottom_reached |=
                        previous_bottom >= viewport.top() && previous_bottom <= viewport.bottom();
                    for (index, control) in controls.iter().enumerate() {
                        let bounds = window.find(*control).bounds();
                        assert!(
                            bounds.size.height >= px(24.),
                            "compressed control {control}: {bounds:?}"
                        );
                        assert!(
                            bounds.top() >= previous_bottom,
                            "overlapping control {control}"
                        );
                        assert!(
                            bounds.left() >= viewport.left() && bounds.right() <= viewport.right()
                        );
                        previous_bottom = bounds.bottom();
                        reached[index] |=
                            bounds.top() >= viewport.top() && bounds.bottom() <= viewport.bottom();
                    }
                    if reached.iter().all(|seen| *seen) && path_bottom_reached {
                        break;
                    }
                    window.scroll(
                        "inspector-scroll",
                        ScrollDelta::Pixels(point(px(0.), px(-48.))),
                        cx,
                    );
                    assert_eq!(window.find("inspector-heading").bounds(), heading);
                    assert_eq!(window.find("inspector-caveat").bounds(), caveat);
                }
                assert!(
                    path_bottom_reached,
                    "full path ending must be scrollable into view"
                );
                assert!(
                    reached.iter().all(|seen| *seen),
                    "unreachable controls: {controls:?}, {reached:?}"
                );
            })
            .unwrap();
        }
    }
}
