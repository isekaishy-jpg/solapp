use solapp::SANativeWindowRef;

fn require_send<T: Send>() {}

fn main() {
    require_send::<SANativeWindowRef<'static>>();
}
