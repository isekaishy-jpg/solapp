//! Owner-thread native resource-retirement checks. No global input or pointer moves.

use super::*;
use windows_sys::Win32::UI::WindowsAndMessaging::{GetIconInfo, ShowCursor, ICONINFO, IDC_HAND};

fn custom_cursor() -> Arc<RaiiCursor> {
    let image = CursorImage {
        rgba: vec![255; 16],
        width: 2,
        height: 2,
        hotspot_x: 0,
        hotspot_y: 0,
    };
    match WinCursor::new(&image).unwrap() {
        WinCursor::Cursor(cursor) => cursor,
        WinCursor::Failed => unreachable!(),
    }
}

fn valid(cursor: HCURSOR) -> bool {
    let mut info: ICONINFO = unsafe { mem::zeroed() };
    // SAFETY: GetIconInfo validates the observed cursor handle. Its bitmap copies
    // are always reclaimed, and no pointer-bearing message is dispatched.
    let result = unsafe { GetIconInfo(cursor, &mut info) } != 0;
    if info.hbmMask != 0 {
        unsafe { DeleteObject(info.hbmMask) };
    }
    if info.hbmColor != 0 {
        unsafe { DeleteObject(info.hbmColor) };
    }
    result
}

#[test]
fn current_custom_retention_covers_named_null_hidden_and_exit_paths() {
    clear_applied_cursor();
    let first = custom_cursor();
    let first_handle = first.as_raw_handle();
    apply_selected_cursor(SelectedCursor::Custom(first));
    assert!(valid(first_handle));
    // This fixture checks resource lifetime independently of HWND authority.
    // Balance its test-only native hide even if a lifetime assertion unwinds.
    struct RestoreVisibility;
    impl Drop for RestoreVisibility {
        fn drop(&mut self) {
            unsafe { ShowCursor(1) };
        }
    }
    unsafe { ShowCursor(0) };
    let restore = RestoreVisibility;
    assert!(valid(first_handle));
    drop(restore);
    assert!(valid(first_handle));
    apply_selected_cursor(SelectedCursor::Named(CursorIcon::Default));
    assert!(!valid(first_handle));

    let second = custom_cursor();
    let second_handle = second.as_raw_handle();
    apply_selected_cursor(SelectedCursor::Custom(second));
    apply_cursor(0, None);
    assert_eq!(unsafe { GetCursor() }, 0);
    assert!(!valid(second_handle));

    for _ in 0..8 {
        let previous = unsafe { GetCursor() };
        let next = custom_cursor();
        let next_handle = next.as_raw_handle();
        apply_selected_cursor(SelectedCursor::Custom(next));
        assert!(valid(next_handle));
        if previous != 0 {
            assert!(!valid(previous));
        }
    }
    let current = unsafe { GetCursor() };
    clear_applied_cursor();
    assert_eq!(unsafe { GetCursor() }, unsafe { LoadCursorW(0, IDC_ARROW) });
    assert!(!valid(current));

    let own = custom_cursor();
    let own_handle = own.as_raw_handle();
    apply_selected_cursor(SelectedCursor::Custom(own));
    let external = unsafe { LoadCursorW(0, IDC_HAND) };
    unsafe { SetCursor(external) };
    clear_applied_cursor();
    assert_eq!(unsafe { GetCursor() }, external);
    assert!(!valid(own_handle));
}
