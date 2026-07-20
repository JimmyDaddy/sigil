use super::*;

#[test]
fn window_state_restores_bounded_geometry_and_clamps_removed_displays() {
    let temp = tempfile::tempdir().expect("temporary directory should create");
    let path = temp.path().join("state/window-v1.json");
    let mut store = WindowStateStore::load(path.clone());
    store
        .set(WindowGeometry {
            x: 4_000,
            y: 2_000,
            width: 2_560,
            height: 1_640,
            maximized: false,
        })
        .expect("window state should persist");

    let reopened = WindowStateStore::load(path);
    let restored = reopened
        .initial_geometry(&[DisplayBounds {
            x: 0,
            y: 0,
            width: 3_456,
            height: 2_160,
            scale_factor: 2.0,
        }])
        .expect("window geometry should restore");
    assert_eq!(restored.x, 448.0);
    assert_eq!(restored.y, 260.0);
    assert_eq!(restored.width, 1_280.0);
    assert_eq!(restored.height, 820.0);
    assert!(!restored.maximized);
}

#[test]
fn window_state_rejects_small_invalid_or_oversized_files() {
    let temp = tempfile::tempdir().expect("temporary directory should create");
    let small_path = temp.path().join("small.json");
    std::fs::write(
        &small_path,
        br#"{"schemaVersion":1,"geometry":{"x":0,"y":0,"width":320,"height":480,"maximized":false}}"#,
    )
    .expect("small fixture should write");
    assert!(
        WindowStateStore::load(small_path)
            .initial_geometry(&[DisplayBounds {
                x: 0,
                y: 0,
                width: 1_920,
                height: 1_080,
                scale_factor: 1.0,
            }])
            .is_none()
    );

    let oversized_path = temp.path().join("oversized.json");
    std::fs::write(
        &oversized_path,
        vec![b'x'; MAX_WINDOW_STATE_FILE_BYTES as usize + 1],
    )
    .expect("oversized fixture should write");
    assert!(
        WindowStateStore::load(oversized_path)
            .initial_geometry(&[])
            .is_none()
    );
}

#[test]
fn window_state_selects_the_display_with_the_largest_overlap() {
    let temp = tempfile::tempdir().expect("temporary directory should create");
    let path = temp.path().join("window.json");
    let mut store = WindowStateStore::load(path);
    store
        .set(WindowGeometry {
            x: 2_100,
            y: 100,
            width: 1_200,
            height: 800,
            maximized: true,
        })
        .expect("window state should persist");
    let restored = store
        .initial_geometry(&[
            DisplayBounds {
                x: 0,
                y: 0,
                width: 1_920,
                height: 1_080,
                scale_factor: 1.0,
            },
            DisplayBounds {
                x: 1_920,
                y: 0,
                width: 1_920,
                height: 1_080,
                scale_factor: 1.0,
            },
        ])
        .expect("second display should be selected");
    assert_eq!(restored.x, 2_100.0);
    assert_eq!(restored.y, 100.0);
    assert!(restored.maximized);
}
