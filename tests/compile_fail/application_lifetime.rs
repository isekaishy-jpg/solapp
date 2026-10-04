use solapp::{SAApplication, SAContext, SAError, SAStopProgress};
struct App<'data>(&'data u8);
impl SAApplication for App<'_> {
    type Message = String;
    type LocalEvent = u8;
    fn started(&mut self, _: &mut SAContext<'_, Self>) -> Result<(), SAError> { Ok(()) }
    fn stopping(&mut self, _: &mut SAContext<'_, Self>) -> SAStopProgress { SAStopProgress::Settled }
}
fn change<'cx, 'short>(cx: SAContext<'cx, App<'static>>) -> SAContext<'cx, App<'short>> { cx }
fn main() {}
