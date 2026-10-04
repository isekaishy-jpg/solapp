use solapp::{SAApplication, SAContext, SAError, SAHost, SAStopProgress};
struct App;
impl SAApplication for App {
    type Message = String;
    type LocalEvent = u8;
    fn started(&mut self, _: &mut SAContext<'_, Self>) -> Result<(), SAError> { Ok(()) }
    fn stopping(&mut self, _: &mut SAContext<'_, Self>) -> SAStopProgress { SAStopProgress::Settled }
}
fn requires_send<T: Send>() {}
fn main() { requires_send::<SAHost<App>>(); }
