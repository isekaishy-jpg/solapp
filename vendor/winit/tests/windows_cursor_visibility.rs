//! Pure ownership regressions for Solapp's Windows visibility contribution.
use super::CursorVisibility;

#[test]
fn unrelated_visibility_and_relative_mode_refresh_do_not_restore_owner() {
    let mut state = CursorVisibility::default();
    assert_eq!(state.refresh(1, true, true, true, None), Some(true));
    assert_eq!(state.refresh(2, false, false, false, None), None);
    assert_eq!(state.refresh(2, false, true, false, None), None);
    // Even stale IN_WINDOW state is insufficient for an unrelated setter.
    assert_eq!(state.refresh(2, true, false, false, None), None);
    assert_eq!(state.refresh(1, true, false, false, None), Some(false));
}

#[test]
fn enter_transfers_ownership_and_late_leave_or_close_cannot_restore_new_owner() {
    let mut state = CursorVisibility::default();
    assert_eq!(state.refresh(1, true, true, true, None), Some(true));
    assert_eq!(state.refresh(2, true, true, true, None), None);
    assert_eq!(state.refresh(1, false, true, false, None), None);
    assert_eq!(state.release(1), None);
    assert_eq!(state.refresh(2, false, true, false, None), Some(false));
    assert_eq!(state.release(2), None);
}

#[test]
fn visible_enter_and_nonclient_leave_restore_exactly_once() {
    let mut state = CursorVisibility::default();
    assert_eq!(state.refresh(1, true, true, true, None), Some(true));
    assert_eq!(state.refresh(2, true, false, true, None), Some(false));
    assert_eq!(state.refresh(1, false, true, false, None), None);
    assert_eq!(state.refresh(2, true, true, false, None), Some(true));
    assert_eq!(state.refresh(2, false, true, false, None), Some(false));
    assert_eq!(state.refresh(2, false, true, false, None), None);
}

#[test]
fn capture_controls_outside_client_and_transfer_or_loss_releases_old_owner() {
    let mut state = CursorVisibility::default();
    assert_eq!(state.refresh(1, false, true, false, Some(1)), Some(true));
    assert_eq!(state.refresh(1, false, true, false, Some(1)), None);
    assert_eq!(state.refresh(2, true, false, true, Some(1)), None);
    assert_eq!(state.refresh(1, false, true, false, Some(2)), Some(false));
    assert_eq!(state.refresh(2, false, true, false, Some(2)), Some(true));
    assert_eq!(state.release(1), None);
    assert_eq!(state.refresh(2, false, true, false, None), Some(false));
}

#[test]
fn close_and_exit_balance_only_the_owned_contribution() {
    let mut state = CursorVisibility::default();
    assert_eq!(state.refresh(1, true, true, true, None), Some(true));
    assert_eq!(state.release(2), None);
    assert_eq!(state.release(1), Some(false));
    assert_eq!(state.clear(), None);
    assert_eq!(state.refresh(3, true, true, true, None), Some(true));
    assert_eq!(state.clear(), Some(false));
    assert_eq!(state.release(3), None);
}
