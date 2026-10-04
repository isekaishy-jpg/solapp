use solapp::SANativeWindowRef;

fn require_sync<T: Sync>() {}

fn main() {
    require_sync::<SANativeWindowRef<'static>>();
}
