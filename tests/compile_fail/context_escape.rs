use solapp::{SAApplication, SAContext, SAError, SAStopProgress};
struct App;
impl SAApplication for App {
    type Message = String;
    type LocalEvent = u8;
    fn started(&mut self, _: &mut SAContext<'_, Self>) -> Result<(), SAError> { Ok(()) }
    fn stopping(&mut self, _: &mut SAContext<'_, Self>) -> SAStopProgress { SAStopProgress::Settled }
}
fn escape<'cx>(cx: SAContext<'cx, App>) -> SAContext<'static, App> { cx }
fn main() {}
